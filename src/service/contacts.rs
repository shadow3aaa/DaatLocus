//! Account contacts are bounded reads of each authoritative device, not a replica catalog.
use super::*;
use crate::actors::store::{ActorContact, ContactPage, ContactSort};
use crate::stage::{ActorAddress, ledger::StageLedger};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Query {
    q: Option<String>,
    after: Option<String>,
    limit: Option<usize>,
    #[serde(default)]
    sort: ContactSort,
}

pub(super) async fn list(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<Query>,
) -> ApiResult<Json<Value>> {
    owner(&state, &headers).await?;
    let text = query.q.unwrap_or_default();
    if text.len() > 512 {
        return Err(ApiError::invalid());
    }
    let limit = bounded(query.limit, 32)?;
    let event_cursor = state
        .system
        .store()
        .event_cursor()
        .map_err(ApiError::from_report)?;
    let local = state
        .system
        .store()
        .public_stage_id()
        .map_err(stages::failure)?;
    let mut sources = vec![local];
    if let Some(runtime) = &state.stage {
        let peers = StageLedger::attach(state.system.store(), runtime.host.local)
            .peers(chrono::Utc::now().timestamp_millis())
            .map_err(stages::failure)?;
        sources.extend(
            peers
                .into_iter()
                .map(|peer| peer.device.stage_id)
                .filter(|id| *id != local),
        );
    }
    let after = match query.after {
        Some(value) if value.len() <= 4096 => Some(
            String::from_utf8(
                URL_SAFE_NO_PAD
                    .decode(value)
                    .map_err(|_| ApiError::invalid())?,
            )
            .map_err(|_| ApiError::invalid())?,
        ),
        Some(_) => return Err(ApiError::invalid()),
        None => None,
    };
    let mut unavailable = Vec::new();
    let mut items: Vec<ActorContact> = Vec::new();
    let mut more = false;
    // Each device supplies at most one bounded page beyond the same sort key.
    // Merging those pages gives a global order without loading the Actor catalog.
    for stage in sources {
        let page: ContactPage = if stage == local {
            state
                .system
                .store()
                .contacts(
                    &state.system.config(),
                    crate::stage::shared::ACTOR_ID,
                    &text,
                    after.as_deref(),
                    limit,
                    query.sort,
                )
                .map_err(ApiError::from_report)?
        } else {
            match remote::read(
                &state,
                &headers,
                ActorAddress {
                    stage_id: stage,
                    actor_id: crate::stage::shared::ACTOR_ID.into(),
                },
                ConversationRead::Contacts {
                    text: text.clone(),
                    after: after.clone(),
                    limit: limit as u8,
                    sort: query.sort,
                },
            )
            .await
            {
                Ok(value) => serde_json::from_value(value).map_err(|_| ApiError::internal())?,
                Err(error) if error.status != StatusCode::UNAUTHORIZED => {
                    unavailable.push(stage.to_string());
                    continue;
                }
                Err(error) => return Err(error),
            }
        };
        more |= page.next_cursor.is_some();
        items.extend(page.items);
    }
    items.sort_by(|a, b| query.sort.compare(&a.actor, &b.actor));
    more |= items.len() > limit;
    items.truncate(limit);
    let next = items
        .last()
        .filter(|_| more)
        .map(|item| query.sort.cursor(&item.actor))
        .transpose()
        .map_err(ApiError::from_report)?
        .map(|cursor| URL_SAFE_NO_PAD.encode(cursor));
    let mut page = json!({"items":items,"next_cursor":next,"unavailable_devices":unavailable,"event_cursor":event_cursor});
    redact(&mut page, &state.system.config());
    Ok(Json(page))
}
