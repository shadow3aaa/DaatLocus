//! Contact reads derive visible bubbles from their original domain records.
use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContactSort {
    #[default]
    Alphabetical,
    Recent,
}
#[derive(Serialize, serde::Deserialize)]
struct ContactCursor {
    sort: ContactSort,
    key: (u8, i64, String, String),
}
impl ContactSort {
    fn key(self, actor: &ActorSummary) -> (u8, i64, String, String) {
        (
            u8::from(self != Self::Recent || actor.status != ActorStatus::Running),
            if self == Self::Recent {
                actor.updated_at_ms.saturating_neg()
            } else {
                0
            },
            actor.name.to_ascii_lowercase(),
            actor.id.clone(),
        )
    }
    pub(crate) fn compare(self, a: &ActorSummary, b: &ActorSummary) -> std::cmp::Ordering {
        self.key(a).cmp(&self.key(b))
    }
    pub(crate) fn cursor(self, actor: &ActorSummary) -> Result<String> {
        serde_json::to_string(&ContactCursor {
            sort: self,
            key: self.key(actor),
        })
        .into_diagnostic()
    }
}

#[derive(Debug, Serialize, serde::Deserialize)]
pub struct ContactMessage {
    pub message_id: String,
    pub kind: String,
    pub content: String,
    pub result_id: Option<String>,
    pub generation: Option<i64>,
}
#[derive(Debug, Serialize, serde::Deserialize)]
pub struct ActorContact {
    pub actor: ActorSummary,
    pub model: Option<String>,
    pub messages: Vec<ContactMessage>,
}
#[derive(Debug, Serialize, serde::Deserialize)]
pub struct ContactPage {
    pub items: Vec<ActorContact>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[schemars(extend("x-dl-content"="actor"))]
pub struct ConversationPage {
    pub actor_id: String,
    pub items: Vec<ConversationBubble>,
    pub next_cursor: Option<String>,
}
#[derive(Debug, Serialize, schemars::JsonSchema)]
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
 AND (actor_id!='core' OR json_extract(record,'$.source.kind') IN ('user','remote_user'))
 AND NOT (json_extract(record,'$.source.kind')='actor'
   AND json_valid(json_extract(record,'$.content'))
   AND COALESCE(CASE WHEN json_valid(json_extract(record,'$.content')) THEN json_extract(json_extract(record,'$.content'),'$.type')='approval_appeal_decision' ELSE 0 END,0))
 UNION ALL
 SELECT actor_id,id,'reply',json_extract(record,'$.reply'),NULL,NULL,json_extract(record,'$.updated_at_ms')
 FROM messages WHERE length(json_extract(record,'$.reply'))>0
 AND (actor_id!='core' OR (json_extract(record,'$.status')='completed'
   AND (json_extract(record,'$.reply_to.kind')='user' OR (json_extract(record,'$.reply_to.kind')='remote' AND json_extract(record,'$.reply_to.user')=1))
   AND (json_extract(record,'$.kind')!='delivery' OR EXISTS(SELECT 1 FROM events e WHERE e.actor_id='core' AND json_extract(e.record,'$.type')='message_added' AND json_extract(e.record,'$.message_id')=messages.id))))
 UNION ALL
 SELECT actor_id,message_id,'sent',json_extract(record,'$.payload.sent_message.content'),id,NULL,json_extract(record,'$.finished_at_ms')
 FROM tools WHERE actor_id!='core' AND done=1 AND json_extract(record,'$.error') IS NULL
 AND length(json_extract(record,'$.payload.sent_message.content'))>0
 UNION ALL
 SELECT actor_id,message_id,'assistant',group_concat(content,''),NULL,generation,min(at)
 FROM (SELECT e.actor_id,json_extract(e.record,'$.message_id') message_id,json_extract(e.record,'$.generation') generation,
       json_extract(e.record,'$.text') content,e.created_at at FROM events e JOIN messages m ON m.id=json_extract(e.record,'$.message_id')
       WHERE e.actor_id!='core' AND json_extract(e.record,'$.type')='generation_delta'
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
    pub fn contacts(
        &self,
        config: &crate::config::Config,
        owner: &str,
        query: &str,
        after: Option<&str>,
        limit: usize,
        sort: ContactSort,
    ) -> Result<ContactPage> {
        database_io(|| {
            let text = query.trim();
            if text.len() > 512
                || after.is_some_and(|cursor| cursor.is_empty() || cursor.len() > 2048)
                || limit == 0
                || limit > 32
            {
                return Err(ActorError::Invalid("Invalid contacts query".into()).into());
            }
            if let Some(cursor) = after {
                let cursor: ContactCursor = serde_json::from_str(cursor)
                    .map_err(|_| ActorError::Invalid("Invalid contacts cursor".into()))?;
                if cursor.sort != sort {
                    return Err(ActorError::Invalid(
                        "Contacts cursor uses a different sort".into(),
                    )
                    .into());
                }
            }
            let db = self.db.lock();
            let allowed = account_actor_ids_db(&db, owner)?;
            let scope = serde_json::to_string(&allowed).into_diagnostic()?;
            let statement=format!("{BUBBLES}, contacts AS (
                SELECT a.record,a.id,lower(json_extract(a.record,'$.config.name')) name,
                    CASE WHEN ?5 AND json_extract(a.record,'$.status')='running' THEN 0 ELSE 1 END priority,
                    CASE WHEN ?5 THEN -json_extract(a.record,'$.updated_at_ms') ELSE 0 END recency
                FROM actors a WHERE a.id IN (SELECT value FROM json_each(?1))
                AND (?3='' OR instr(lower(json_extract(a.record,'$.config.name')),lower(?3))>0
                    OR EXISTS(SELECT 1 FROM bubbles b WHERE b.actor_id=a.id AND instr(lower(b.content),lower(?3))>0)))
                SELECT record FROM contacts WHERE ?2 IS NULL OR (priority,recency,name,id) >
                    (json_extract(?2,'$.key[0]'),json_extract(?2,'$.key[1]'),json_extract(?2,'$.key[2]'),json_extract(?2,'$.key[3]'))
                ORDER BY priority,recency,name,id LIMIT ?4");
            let mut statement = sql(db.prepare(&statement))?;
            let mut actors = sql(statement.query_map(
                params![
                    scope,
                    after,
                    text,
                    (limit + 1) as i64,
                    sort == ContactSort::Recent
                ],
                |row| row.get::<_, String>(0),
            ))?
            .map(|row| decode::<ActorRecord>(sql(row)?))
            .collect::<Result<Vec<_>>>()?;
            let next_cursor = (actors.len() > limit)
                .then(|| sort.cursor(&actors[limit - 1].summary()))
                .transpose()?;
            actors.truncate(limit);
            let mut items = Vec::new();
            let mut matches=sql(db.prepare(&format!("{BUBBLES} SELECT message_id,kind,content,result_id,generation FROM bubbles
                WHERE actor_id=?1 AND instr(lower(content),lower(?2))>0 ORDER BY at DESC,message_id,kind LIMIT 3")))?;
            for actor in actors {
                let messages = if text.is_empty() {
                    Vec::new()
                } else {
                    sql(matches.query_map(params![actor.id, text], |row| {
                        Ok(ContactMessage {
                            message_id: row.get(0)?,
                            kind: row.get(1)?,
                            content: row.get(2)?,
                            result_id: row.get(3)?,
                            generation: row.get(4)?,
                        })
                    }))?
                    .map(sql)
                    .collect::<Result<Vec<_>>>()?
                };
                items.push(ActorContact {
                    actor: actor.summary(),
                    model: {
                        let model = config.actor_model_name(actor.config.model.as_deref());
                        config.models.contains_key(model).then(|| model.to_owned())
                    },
                    messages,
                });
            }
            Ok(ContactPage { items, next_cursor })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contacts_cannot_cross_account_membership_or_accept_a_foreign_cursor() {
        let directory = tempfile::tempdir().unwrap();
        let store = ActorStore::open(&directory.path().join("actors.sqlite3")).unwrap();
        for id in ["core", "foreign"] {
            store
                .save_actor(&ActorRecord {
                    remote_read_only: false,
                    id: id.into(),
                    parent_id: None,
                    depth: 0,
                    context: Default::default(),
                    config: ActorConfig {
                        name: id.into(),
                        workspace: directory.path().into(),
                        apps: vec![],
                        model: None,
                        instructions: String::new(),
                    },
                    status: ActorStatus::Idle,
                    created_at_ms: 1,
                    updated_at_ms: 1,
                })
                .unwrap();
        }
        store.stage_transaction(|tx|{
            tx.execute_batch("CREATE TABLE stage_account_actors(actor TEXT PRIMARY KEY,account TEXT NOT NULL); INSERT INTO stage_account_actors VALUES('core','account-a'),('foreign','account-b');")?;Ok(())
        }).unwrap();
        let config = crate::config::Config::default();
        let page = store
            .contacts(&config, "core", "", None, 24, ContactSort::Alphabetical)
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].actor.id, "core");
        assert!(
            store
                .contacts(
                    &config,
                    "core",
                    "foreign",
                    None,
                    24,
                    ContactSort::Alphabetical
                )
                .unwrap()
                .items
                .is_empty()
        );
        assert!(
            store
                .contacts(
                    &config,
                    "core",
                    "",
                    Some("foreign"),
                    24,
                    ContactSort::Alphabetical
                )
                .is_err()
        );
    }
}
