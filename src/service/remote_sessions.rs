//! Session routes retain the authoritative device's HTTP contract over relay RPC.
use super::*;
use crate::stage::peer::PeerHandle;
use crate::stage::{StageId, owner_http};
use axum::{
    body::Body,
    extract::Request,
    response::{Sse, sse::Event},
    routing::any,
};
use std::{collections::VecDeque, convert::Infallible, time::Duration};
use tower::ServiceExt;

pub(super) fn routes() -> Router<ServiceState> {
    Router::new()
        .route("/v1/stages/{stage}/sessions", any(proxy))
        .route("/v1/stages/{stage}/sessions/{*path}", any(proxy))
        .route("/v1/stages/{stage}/events", get(proxy))
        .route("/v1/stages/{stage}/event-stream", get(stream))
        .layer(DefaultBodyLimit::max(
            crate::daemon::attachments::MAX_FILE_ATTACHMENT_BYTES,
        ))
}
fn stage_uri(request: &Request) -> ApiResult<(StageId, String)> {
    let mut parts = request.uri().path().splitn(5, '/');
    if parts.next() != Some("") || parts.next() != Some("v1") || parts.next() != Some("stages") {
        return Err(ApiError::invalid());
    }
    let stage = parts.next().ok_or_else(ApiError::invalid)?;
    let stage = serde_json::from_value(json!(stage)).map_err(|_| ApiError::invalid())?;
    let suffix = parts.next().ok_or_else(ApiError::invalid)?;
    let mut uri = format!("/v1/{suffix}");
    if let Some(query) = request.uri().query() {
        uri.push('?');
        uri.push_str(query);
    }
    Ok((stage, uri))
}
pub(super) async fn request(
    state: &ServiceState,
    stage: StageId,
    method: String,
    uri: String,
    headers: &HeaderMap,
    body: Vec<u8>,
) -> ApiResult<owner_http::Reply> {
    let runtime = stages::runtime(state)?;
    runtime.want(stage);
    let forwarded_headers = headers
        .iter()
        .filter_map(|(name, value)| {
            owner_http::forwarded_header(name.as_str())
                .then(|| {
                    value
                        .to_str()
                        .ok()
                        .map(|v| (name.as_str().into(), v.into()))
                })
                .flatten()
        })
        .collect();
    let reply = owner_http::call(
        &runtime,
        stage,
        owner_http::Request {
            method,
            uri,
            headers: forwarded_headers,
            body,
        },
    )
    .await
    .map_err(stages::failure)?;
    owner(state, headers).await?;
    Ok(reply)
}
pub(super) async fn request_on_handle(
    state: &ServiceState,
    stage: StageId,
    handle: PeerHandle,
    method: String,
    uri: String,
    headers: &HeaderMap,
    body: Vec<u8>,
) -> ApiResult<owner_http::Reply> {
    owner(state, headers).await?;
    let forwarded_headers = headers
        .iter()
        .filter_map(|(name, value)| {
            owner_http::forwarded_header(name.as_str())
                .then(|| {
                    value
                        .to_str()
                        .ok()
                        .map(|v| (name.as_str().into(), v.into()))
                })
                .flatten()
        })
        .collect();
    owner_http::call_handle(
        &stages::runtime(state)?.host,
        stage,
        handle,
        owner_http::Request {
            method,
            uri,
            headers: forwarded_headers,
            body,
        },
    )
    .await
    .map_err(stages::failure)
}
fn qualify(value: &mut Value, stage: StageId) {
    match value {
        Value::Array(items) => {
            for item in items {
                qualify(item, stage);
            }
        }
        Value::Object(object) => {
            for (name, item) in object {
                if let Value::String(text) = item {
                    if name == "conversation_url" && text.starts_with("/actors/") {
                        *text = text.replacen("/actors/", &format!("/stages/{stage}/actors/"), 1);
                    }
                    if name == "uri" && text.starts_with("/v1/actors/") {
                        *text =
                            text.replacen("/v1/actors/", &format!("/v1/stages/{stage}/actors/"), 1);
                    }
                }
                if !matches!(name.as_str(), "payload" | "arguments" | "call") {
                    qualify(item, stage);
                }
            }
        }
        _ => {}
    }
}
async fn proxy(State(state): State<ServiceState>, request_in: Request) -> ApiResult<Response> {
    owner(&state, request_in.headers()).await?;
    let (stage, uri) = stage_uri(&request_in)?;
    if request_in.method() != axum::http::Method::GET {
        manage(&state, request_in.headers()).await?;
    }
    let runtime = stages::runtime(&state)?;
    if stage == runtime.host.local {
        let (mut parts, body) = request_in.into_parts();
        parts.uri = uri.parse().map_err(|_| ApiError::invalid())?;
        return Ok(router(state)
            .oneshot(Request::from_parts(parts, body))
            .await
            .unwrap());
    }
    let method = request_in.method().as_str().to_owned();
    let headers = request_in.headers().clone();
    let body = axum::body::to_bytes(
        request_in.into_body(),
        crate::daemon::attachments::MAX_FILE_ATTACHMENT_BYTES,
    )
    .await
    .map_err(|_| ApiError::invalid())?
    .to_vec();
    let reply = request(&state, stage, method, uri, &headers, body).await?;
    let status = StatusCode::from_u16(reply.status).map_err(|_| ApiError::internal())?;
    let mut bytes = reply.body;
    if reply
        .content_type
        .as_deref()
        .is_some_and(|t| t.starts_with("application/json"))
    {
        let mut value: Value = serde_json::from_slice(&bytes).map_err(|_| ApiError::internal())?;
        qualify(&mut value, stage);
        bytes = serde_json::to_vec(&value).map_err(|_| ApiError::internal())?;
    }
    let mut response = Response::builder().status(status);
    if let Some(content_type) = reply.content_type {
        response = response.header("content-type", content_type);
    }
    response
        .body(Body::from(bytes))
        .map_err(|_| ApiError::internal())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StreamQuery {
    after: Option<i64>,
    limit: Option<usize>,
}
async fn stream(
    State(state): State<ServiceState>,
    Path(stage): Path<StageId>,
    headers: HeaderMap,
    Query(query): Query<StreamQuery>,
) -> ApiResult<Response> {
    owner(&state, &headers).await?;
    let runtime = stages::runtime(&state)?;
    if stage == runtime.host.local {
        let mut uri = format!("/v1/event-stream?limit={}", bounded(query.limit, 100)?);
        if let Some(after) = query.after {
            uri.push_str(&format!("&after={after}"));
        }
        let mut input = axum::http::Request::builder()
            .uri(uri)
            .body(Body::empty())
            .map_err(|_| ApiError::invalid())?;
        *input.headers_mut() = headers;
        return Ok(router(state).oneshot(input).await.unwrap());
    }
    let cursor = headers
        .get("last-event-id")
        .map(|h| {
            h.to_str()
                .ok()
                .and_then(|v| v.parse::<i64>().ok())
                .ok_or_else(ApiError::invalid)
        })
        .transpose()?
        .or(query.after)
        .unwrap_or(0);
    if cursor < 0 {
        return Err(ApiError::invalid());
    }
    let limit = bounded(query.limit, 100)?;
    let initial = request(
        &state,
        stage,
        "GET".into(),
        format!("/v1/events?after={cursor}&limit={limit}"),
        &headers,
        vec![],
    )
    .await?;
    if initial.status != 200 {
        return Err(ApiError::invalid());
    }
    let initial: Value = serde_json::from_slice(&initial.body).map_err(|_| ApiError::internal())?;
    struct Feed {
        state: ServiceState,
        headers: HeaderMap,
        stage: StageId,
        cursor: i64,
        limit: usize,
        queue: VecDeque<Value>,
        pending_cursor: bool,
        ended: bool,
    }
    let next = initial["next_cursor"].as_i64().unwrap_or(cursor);
    let queue: VecDeque<Value> = initial["items"]
        .as_array()
        .ok_or_else(ApiError::internal)?
        .iter()
        .cloned()
        .collect();
    let pending_cursor = next != cursor && queue.is_empty();
    let feed = futures_util::stream::unfold(
        Feed {
            state,
            headers,
            stage,
            cursor: next,
            limit,
            queue,
            pending_cursor,
            ended: false,
        },
        |mut feed| async move {
            if feed.ended {
                return None;
            }
            loop {
                if owner(&feed.state, &feed.headers).await.is_err() {
                    feed.ended = true;
                    return Some((
                        Ok::<_, Infallible>(
                            Event::default()
                                .event("access_revoked")
                                .data("{\"code\":\"access_revoked\"}"),
                        ),
                        feed,
                    ));
                }
                let runtime = feed.state.stage.as_ref()?;
                if owner_http::authorize(&runtime.host, feed.stage).is_err() {
                    feed.ended = true;
                    return Some((
                        Ok(Event::default().event("access_revoked").data("{}")),
                        feed,
                    ));
                }
                if let Some(mut value) = feed.queue.pop_front() {
                    qualify(&mut value, feed.stage);
                    let id = value["seq"].as_i64().unwrap_or(feed.cursor);
                    return Some((
                        Ok(Event::default()
                            .event("activity")
                            .id(id.to_string())
                            .json_data(value)
                            .unwrap()),
                        feed,
                    ));
                }
                if feed.pending_cursor {
                    feed.pending_cursor = false;
                    return Some((
                        Ok(Event::default()
                            .event("cursor")
                            .id(feed.cursor.to_string())
                            .data("{}")),
                        feed,
                    ));
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
                let reply = request(
                    &feed.state,
                    feed.stage,
                    "GET".into(),
                    format!("/v1/events?after={}&limit={}", feed.cursor, feed.limit),
                    &feed.headers,
                    vec![],
                )
                .await;
                let Ok(reply) = reply else {
                    feed.ended = true;
                    return Some((Ok(Event::default().event("resync").data("{}")), feed));
                };
                let value = serde_json::from_slice::<Value>(&reply.body).ok();
                let Some(value) = value.filter(|_| reply.status == 200) else {
                    feed.ended = true;
                    return Some((Ok(Event::default().event("resync").data("{}")), feed));
                };
                let next = value["next_cursor"].as_i64().unwrap_or(feed.cursor);
                feed.queue = value["items"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into();
                feed.pending_cursor = next != feed.cursor && feed.queue.is_empty();
                feed.cursor = next;
                if feed.queue.is_empty() && !feed.pending_cursor {
                    if owner(&feed.state, &feed.headers).await.is_err() {
                        continue;
                    }
                    return Some((Ok(Event::default().comment("keepalive")), feed));
                }
            }
        },
    );
    Ok(Sse::new(feed).into_response())
}
