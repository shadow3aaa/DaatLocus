//! Account contacts are bounded reads of each authoritative device, not a replica catalog.
use super::*;
use crate::stage::{ActorAddress, StageId, ledger::StageLedger};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Query {
    q: Option<String>,
    after: Option<String>,
    limit: Option<usize>,
}
#[derive(Serialize, Deserialize)]
struct Cursor {
    stage: StageId,
    after: Option<String>,
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
    let cursor = match query.after {
        Some(value) if value.len() <= 1024 => serde_json::from_slice::<Cursor>(
            &URL_SAFE_NO_PAD
                .decode(value)
                .map_err(|_| ApiError::invalid())?,
        )
        .map_err(|_| ApiError::invalid())?,
        Some(_) => return Err(ApiError::invalid()),
        None => Cursor {
            stage: local,
            after: None,
        },
    };
    let index = sources
        .iter()
        .position(|id| *id == cursor.stage)
        .ok_or_else(ApiError::invalid)?;
    let mut unavailable = Vec::new();
    let mut page = if cursor.stage == local {
        serde_json::to_value(
            state
                .system
                .store()
                .contacts(
                    &state.system.config(),
                    crate::stage::shared::ACTOR_ID,
                    &text,
                    cursor.after.as_deref(),
                    limit,
                )
                .map_err(ApiError::from_report)?,
        )
        .map_err(|_| ApiError::internal())?
    } else {
        match remote::read(
            &state,
            &headers,
            ActorAddress {
                stage_id: cursor.stage,
                actor_id: crate::stage::shared::ACTOR_ID.into(),
            },
            ConversationRead::Contacts {
                text: text.clone(),
                after: cursor.after.clone(),
                limit: limit as u8,
            },
        )
        .await
        {
            Ok(value) => value,
            Err(error) if error.status != StatusCode::UNAUTHORIZED => {
                unavailable.push(cursor.stage.to_string());
                json!({"items":[],"next_cursor":null})
            }
            Err(error) => return Err(error),
        }
    };
    let next = if let Some(after) = page["next_cursor"].as_str() {
        Some(Cursor {
            stage: cursor.stage,
            after: Some(after.into()),
        })
    } else {
        sources.get(index + 1).map(|stage| Cursor {
            stage: *stage,
            after: None,
        })
    };
    page["next_cursor"] = match next {
        Some(next) => json!(
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&next).map_err(|_| ApiError::internal())?)
        ),
        None => Value::Null,
    };
    page["unavailable_devices"] = json!(unavailable);
    page["event_cursor"] = json!(event_cursor);
    redact(&mut page, &state.system.config());
    Ok(Json(page))
}
