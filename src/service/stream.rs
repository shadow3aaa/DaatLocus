use super::{ApiError, ApiResult, ServiceState, access_actor, bounded, owner, redact};
use crate::actors::ActorEvent;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{Sse, sse::Event},
};
use futures_util::stream;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::VecDeque, convert::Infallible, time::Duration};

const MAX_BATCH: usize = 100;
const HEARTBEAT: Duration = Duration::from_secs(5);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EventsQuery {
    after: Option<i64>,
    limit: Option<usize>,
}
fn cursor(query: &EventsQuery, headers: &HeaderMap, current: i64) -> ApiResult<i64> {
    let last = headers
        .get("last-event-id")
        .map(|id| {
            id.to_str()
                .ok()
                .and_then(|id| id.parse::<i64>().ok())
                .ok_or_else(ApiError::invalid)
        })
        .transpose()?;
    let cursor = last.unwrap_or(query.after.unwrap_or(0));
    if cursor < 0 || cursor > current {
        return Err(ApiError::invalid());
    }
    Ok(cursor)
}
async fn authorized(
    state: &ServiceState,
    headers: &HeaderMap,
    actor: Option<&str>,
) -> ApiResult<()> {
    match actor {
        Some(id) => access_actor(state, headers, id, false).await,
        None => {
            owner(state, headers).await?;
            Ok(())
        }
    }
}
fn batch(
    state: &ServiceState,
    actor: Option<&str>,
    after: i64,
    limit: usize,
) -> ApiResult<(Vec<Value>, i64)> {
    let events = state
        .system
        .store()
        .events(after, actor, actor.is_none(), limit)
        .map_err(ApiError::from_report)?;
    let next = events.last().map_or(after, |e| e.seq);
    let mut values = Vec::new();
    for event in events {
        if actor.is_none() {
            match &event.event {
                ActorEvent::ActorChanged { .. } | ActorEvent::ActorDeleted { .. } => {}
                ActorEvent::ApprovalRequested { .. } | ActorEvent::ApprovalResolved { .. } => {}
                ActorEvent::MessageUpdated { .. } | ActorEvent::MessageAdded { .. } => {}
                _ => continue,
            }
        }
        let mut value = serde_json::to_value(event).map_err(|_| ApiError::internal())?;
        if let Some(actor) = value.pointer_mut("/event/actor") {
            super::actor_conversation_link(actor);
        }
        redact(&mut value, &state.system.config());
        values.push(value);
    }
    Ok((values, next))
}
async fn query_events(
    state: ServiceState,
    headers: HeaderMap,
    actor: Option<String>,
    query: EventsQuery,
) -> ApiResult<Json<Value>> {
    authorized(&state, &headers, actor.as_deref()).await?;
    let current = state
        .system
        .store()
        .event_cursor()
        .map_err(ApiError::from_report)?;
    let after = cursor(&query, &headers, current)?;
    let (items, next) = batch(
        &state,
        actor.as_deref(),
        after,
        bounded(query.limit, MAX_BATCH)?,
    )?;
    Ok(Json(json!({"items": items, "next_cursor": next})))
}
pub(super) async fn catalog_events(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> ApiResult<Json<Value>> {
    query_events(state, headers, None, query).await
}
pub(super) async fn actor_events(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(actor): Path<String>,
    Query(query): Query<EventsQuery>,
) -> ApiResult<Json<Value>> {
    query_events(state, headers, Some(actor), query).await
}

struct StreamState {
    state: ServiceState,
    headers: HeaderMap,
    actor: Option<String>,
    cursor: i64,
    limit: usize,
    queue: VecDeque<Value>,
    ended: bool,
}
async fn make_stream(
    state: ServiceState,
    headers: HeaderMap,
    actor: Option<String>,
    query: EventsQuery,
) -> ApiResult<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>> {
    authorized(&state, &headers, actor.as_deref()).await?;
    let current = state
        .system
        .store()
        .event_cursor()
        .map_err(ApiError::from_report)?;
    let cursor = cursor(&query, &headers, current)?;
    let limit = bounded(query.limit, MAX_BATCH)?;
    let feed = stream::unfold(
        StreamState {
            state,
            headers,
            actor,
            cursor,
            limit,
            queue: VecDeque::new(),
            ended: false,
        },
        |mut feed| async move {
            if feed.ended {
                return None;
            }
            loop {
                if feed.state.system.is_stopping() {
                    return None;
                }
                // Authorization is checked for every emitted event, replay batch and heartbeat.
                match authorized(&feed.state, &feed.headers, feed.actor.as_deref()).await {
                    Ok(()) => (),
                    Err(_) => {
                        feed.ended = true;
                        return Some((
                            Ok(Event::default()
                                .event("access_revoked")
                                .data("{\"code\":\"access_revoked\"}")),
                            feed,
                        ));
                    }
                };
                if let Some(value) = feed.queue.pop_front() {
                    let id = value
                        .get("seq")
                        .and_then(Value::as_i64)
                        .unwrap_or(feed.cursor);
                    let event = Event::default()
                        .event("activity")
                        .id(id.to_string())
                        .json_data(value)
                        .unwrap_or_else(|_| {
                            Event::default()
                                .event("error")
                                .data("{\"code\":\"encoding_error\"}")
                        });
                    return Some((Ok(event), feed));
                }
                let changed = feed.state.system.store().changed();
                let notified = changed.notified();
                tokio::pin!(notified);
                // Register before querying, so a commit between query and wait cannot lose a wakeup.
                notified.as_mut().enable();
                if feed.state.system.is_stopping() {
                    return None;
                }
                let (items, next) =
                    match batch(&feed.state, feed.actor.as_deref(), feed.cursor, feed.limit) {
                        Ok(result) => result,
                        Err(_) => {
                            feed.ended = true;
                            return Some((
                                Ok(Event::default()
                                    .event("error")
                                    .data("{\"code\":\"stream_error\"}")),
                                feed,
                            ));
                        }
                    };
                let moved = next != feed.cursor;
                feed.cursor = next;
                feed.queue = items.into();
                if !feed.queue.is_empty() {
                    continue;
                }
                if moved {
                    return Some((
                        Ok(Event::default()
                            .event("cursor")
                            .id(next.to_string())
                            .data("{}")),
                        feed,
                    ));
                }
                tokio::select! {
                    _ = &mut notified => {}
                    _ = tokio::time::sleep(HEARTBEAT) => {
                        if feed.state.system.is_stopping() { return None; }
                        if authorized(&feed.state, &feed.headers, feed.actor.as_deref()).await.is_err() { feed.ended = true; return Some((Ok(Event::default().event("access_revoked").data("{\"code\":\"access_revoked\"}")), feed)); }
                        return Some((Ok(Event::default().comment("keepalive")), feed));
                    }
                }
            }
        },
    );
    Ok(Sse::new(feed))
}
pub(super) async fn actor_stream(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(actor): Path<String>,
    Query(query): Query<EventsQuery>,
) -> ApiResult<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>> {
    make_stream(state, headers, Some(actor), query).await
}
pub(super) async fn catalog_stream(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> ApiResult<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>> {
    make_stream(state, headers, None, query).await
}
