//! Conversation reads derive visible bubbles from original domain records.
use super::*;

#[derive(Debug, Serialize, serde::Deserialize, schemars::JsonSchema)]
#[schemars(extend("x-dl-content"="actor"))]
pub struct ConversationPage {
    pub actor_id: String,
    pub items: Vec<ConversationBubble>,
    pub next_cursor: Option<String>,
}
#[derive(Debug, Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ConversationBubble {
    pub message_id: String,
    pub kind: String,
    pub sender: String,
    pub recipient: Option<String>,
    pub content: String,
    pub created_at_ms: i64,
    pub attachments: Vec<String>,
    pub offset: usize,
    pub complete: bool,
}
#[derive(Serialize, serde::Deserialize)]
struct BubbleCursor {
    at: i64,
    message: String,
    kind: String,
    result: String,
    generation: i64,
    offset: usize,
    attachment: usize,
    actor: String,
}

// Core exposes only user inputs and explicit user deliveries. Reasoning, tool
// pipelines and machine notifications are never searchable conversation content.
const BUBBLES: &str = r#"
WITH bubbles AS (
 SELECT actor_id,id message_id,'input' kind,json_extract(record,'$.content') content,
        NULL result_id,NULL generation,json_extract(record,'$.created_at_ms') at
 FROM messages WHERE (length(json_extract(record,'$.content'))>0 OR json_array_length(json_extract(record,'$.attachments'))>0)
  AND (actor_id NOT IN (SELECT id FROM actors WHERE json_extract(record,'$.role')='session') OR json_extract(record,'$.source.kind') IN ('user','remote_user'))
 AND NOT (json_extract(record,'$.source.kind')='actor'
   AND json_valid(json_extract(record,'$.content'))
   AND COALESCE(CASE WHEN json_valid(json_extract(record,'$.content')) THEN json_extract(json_extract(record,'$.content'),'$.type')='approval_appeal_decision' ELSE 0 END,0))
 UNION ALL
 SELECT actor_id,id,'reply',json_extract(record,'$.reply'),NULL,NULL,json_extract(record,'$.updated_at_ms')
 FROM messages WHERE length(json_extract(record,'$.reply'))>0
  AND (actor_id NOT IN (SELECT id FROM actors WHERE json_extract(record,'$.role')='session') OR (json_extract(record,'$.status')='completed'
   AND (json_extract(record,'$.reply_to.kind')='user' OR (json_extract(record,'$.reply_to.kind')='remote' AND json_extract(record,'$.reply_to.user')=1))
    AND (json_extract(record,'$.kind')!='delivery' OR EXISTS(SELECT 1 FROM events e WHERE e.actor_id=messages.actor_id AND json_extract(e.record,'$.type')='message_added' AND json_extract(e.record,'$.message_id')=messages.id))))
 UNION ALL
 SELECT actor_id,message_id,'sent',json_extract(record,'$.payload.sent_message.content'),id,NULL,json_extract(record,'$.finished_at_ms')
  FROM tools WHERE actor_id NOT IN (SELECT id FROM actors WHERE json_extract(record,'$.role')='session') AND done=1 AND json_extract(record,'$.error') IS NULL
 AND length(json_extract(record,'$.payload.sent_message.content'))>0
 UNION ALL
 SELECT actor_id,message_id,'assistant',group_concat(content,''),NULL,generation,min(at)
 FROM (SELECT e.actor_id,json_extract(e.record,'$.message_id') message_id,json_extract(e.record,'$.generation') generation,
       json_extract(e.record,'$.text') content,e.created_at at FROM events e JOIN messages m ON m.id=json_extract(e.record,'$.message_id')
        WHERE e.actor_id NOT IN (SELECT id FROM actors WHERE json_extract(record,'$.role')='session') AND json_extract(e.record,'$.type')='generation_delta'
       AND json_extract(e.record,'$.channel')='assistant'
       AND NOT (json_extract(m.record,'$.status')='completed' AND length(COALESCE(json_extract(m.record,'$.reply'),''))>0
           AND json_extract(e.record,'$.generation')=COALESCE(json_extract(m.record,'$.final_generation'),
             (SELECT max(json_extract(f.record,'$.generation')) FROM events f WHERE f.actor_id=e.actor_id AND json_extract(f.record,'$.message_id')=m.id)))
       ORDER BY e.seq)
 GROUP BY actor_id,message_id,generation
)
"#;

impl ActorStore {
    pub(crate) fn conversation_bubbles(
        &self,
        actor: &str,
        cursor: Option<&str>,
        limit: usize,
        budget: usize,
    ) -> Result<ConversationPage> {
        database_io(|| {
            if limit == 0 || limit > 32 || cursor.is_some_and(|value| value.len() > 2048) {
                return Err(ActorError::Invalid("Invalid conversation page".into()).into());
            }
            let cursor: Option<BubbleCursor> = cursor
                .map(serde_json::from_str)
                .transpose()
                .map_err(|_| ActorError::Invalid("Invalid conversation cursor".into()))?;
            if cursor.as_ref().is_some_and(|value| value.actor != actor) {
                return Err(ActorError::Invalid(
                    "Conversation cursor belongs to another Actor".into(),
                )
                .into());
            }
            let db = self.db.lock();
            let exists: bool = sql(db.query_row(
                "SELECT EXISTS(SELECT 1 FROM actors WHERE id=?1)",
                [actor],
                |row| row.get(0),
            ))?;
            if !exists {
                return Err(ActorError::NotFound("Actor not found".into()).into());
            }
            let bound = cursor.as_ref();
            let query=format!("{BUBBLES} SELECT at,message_id,kind,content,COALESCE(result_id,''),COALESCE(generation,0) FROM bubbles
                WHERE actor_id=?1 AND (?2 IS NULL OR (at,message_id,kind,COALESCE(result_id,''),COALESCE(generation,0)) < (?2,?3,?4,?5,?6)
                OR (?7>0 AND (at,message_id,kind,COALESCE(result_id,''),COALESCE(generation,0))=(?2,?3,?4,?5,?6)))
                ORDER BY at DESC,message_id DESC,kind DESC,COALESCE(result_id,'') DESC,COALESCE(generation,0) DESC LIMIT ?8");
            let mut statement = sql(db.prepare(&query))?;
            let rows = sql(statement.query_map(
                params![
                    actor,
                    bound.map(|v| v.at),
                    bound.map(|v| &v.message),
                    bound.map(|v| &v.kind),
                    bound.map(|v| &v.result),
                    bound.map(|v| v.generation),
                    bound.is_some_and(|v| v.offset > 0 || v.attachment > 0),
                    (limit + 1) as i64
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            ))?;
            let mut items = Vec::new();
            let mut remaining = budget.clamp(1024, 65536).saturating_sub(512);
            let mut next_cursor = None;
            let mut last = None;
            for row in rows {
                let (at, message, kind, content, result, generation) = sql(row)?;
                if items.len() == limit || remaining < 512 {
                    next_cursor = last;
                    break;
                }
                let offset = bound
                    .filter(|v| {
                        v.at == at
                            && v.message == message
                            && v.kind == kind
                            && v.result == result
                            && v.generation == generation
                    })
                    .map_or(0, |v| v.offset);
                if offset > content.len() || !content.is_char_boundary(offset) {
                    return Err(ActorError::Invalid("Invalid conversation offset".into()).into());
                }
                let record = message_tx(&db, &message)?;
                let sender = if kind == "input" {
                    match &record.source {
                        MessageSource::User | MessageSource::RemoteUser { .. } => "user".into(),
                        MessageSource::Actor { actor_id }
                        | MessageSource::RemoteActor { actor_id, .. } => actor_id.clone(),
                    }
                } else {
                    actor.to_owned()
                };
                let recipient=match kind.as_str() {
                    "input"=>Some(actor.to_owned()),
                    "sent"=>sql(db.query_row("SELECT json_extract(record,'$.payload.sent_message.conversation_url') FROM tools WHERE id=?1 AND actor_id=?2",params![result,actor],|row|row.get::<_,Option<String>>(0)).optional())?.flatten(),
                    "reply"=>Some(match &record.reply_to {
                        ReplyTarget::Actor{actor_id,..}=>actor_id.clone(),
                        ReplyTarget::Remote{user:false,..}=>match &record.source {
                            MessageSource::RemoteActor{actor_id,..}=>actor_id.clone(),
                            _=>"remote".into(),
                        },
                        _=>"user".into(),
                    }),
                    _=>None,
                };
                let attachments = if kind == "input" {
                    record.attachments
                } else {
                    vec![]
                };
                let mut attachment = bound
                    .filter(|v| {
                        v.at == at
                            && v.message == message
                            && v.kind == kind
                            && v.result == result
                            && v.generation == generation
                    })
                    .map_or(0, |v| v.attachment);
                if attachment > attachments.len() {
                    return Err(ActorError::Invalid("Invalid attachment offset".into()).into());
                }
                let mut bubble = ConversationBubble {
                    message_id: message.clone(),
                    kind: kind.clone(),
                    sender,
                    recipient,
                    content: String::new(),
                    created_at_ms: at,
                    attachments: vec![],
                    offset,
                    complete: false,
                };
                let mut end = content.len().min(offset.saturating_add(remaining));
                loop {
                    while !content.is_char_boundary(end) {
                        end -= 1;
                    }
                    bubble.content = content[offset..end].into();
                    if serde_json::to_vec(&bubble).into_diagnostic()?.len() <= remaining {
                        break;
                    }
                    if end == offset {
                        return Err(ActorError::Invalid(
                            "Conversation metadata exceeds output budget".into(),
                        )
                        .into());
                    }
                    end = offset + (end - offset) / 2;
                }
                if end == content.len() {
                    while attachment < attachments.len() {
                        bubble.attachments.push(attachments[attachment].uri.clone());
                        if serde_json::to_vec(&bubble).into_diagnostic()?.len() > remaining {
                            bubble.attachments.pop();
                            break;
                        }
                        attachment += 1;
                    }
                }
                let complete = end == content.len() && attachment == attachments.len();
                bubble.complete = complete;
                remaining =
                    remaining.saturating_sub(serde_json::to_vec(&bubble).into_diagnostic()?.len());
                items.push(bubble);
                last = Some(
                    serde_json::to_string(&BubbleCursor {
                        at,
                        message,
                        kind,
                        result,
                        generation,
                        offset: if complete { 0 } else { end },
                        attachment: if complete { 0 } else { attachment },
                        actor: actor.into(),
                    })
                    .into_diagnostic()?,
                );
                if !complete {
                    next_cursor = last;
                    break;
                }
            }
            items.reverse();
            Ok(ConversationPage {
                actor_id: actor.into(),
                items,
                next_cursor,
            })
        })
    }
}
