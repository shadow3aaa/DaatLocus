//! Contact reads derive visible bubbles from their original domain records.
use super::*;

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
    pub messages: Vec<ContactMessage>,
}
#[derive(Debug, Serialize, serde::Deserialize)]
pub struct ContactPage {
    pub items: Vec<ActorContact>,
    pub next_cursor: Option<String>,
}

// Core exposes only user inputs and explicit user deliveries. Reasoning, tool
// pipelines and machine notifications are never searchable conversation content.
const BUBBLES: &str = r#"
WITH bubbles AS (
 SELECT actor_id,id message_id,'input' kind,json_extract(record,'$.content') content,
        NULL result_id,NULL generation,json_extract(record,'$.created_at_ms') at
 FROM messages WHERE length(json_extract(record,'$.content'))>0
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
 SELECT actor_id,message_id,'assistant',group_concat(content,''),NULL,generation,max(at)
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
    pub fn contacts(
        &self,
        owner: &str,
        query: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<ContactPage> {
        database_io(|| {
            let text = query.trim();
            if text.len() > 512
                || after.is_some_and(|id| id.is_empty() || id.len() > 128)
                || limit == 0
                || limit > 32
            {
                return Err(ActorError::Invalid("Invalid contacts query".into()).into());
            }
            let db = self.db.lock();
            let allowed = account_actor_ids_db(&db, owner)?;
            if after.is_some_and(|id| !allowed.contains(id)) {
                return Err(ActorError::Invalid("Invalid contacts cursor".into()).into());
            }
            let scope = serde_json::to_string(&allowed).into_diagnostic()?;
            let statement=format!("{BUBBLES} SELECT a.record FROM actors a WHERE a.id IN (SELECT value FROM json_each(?1))
                AND (?2 IS NULL OR (lower(json_extract(a.record,'$.config.name')),a.id) >
                    (SELECT lower(json_extract(record,'$.config.name')),id FROM actors WHERE id=?2))
                AND (?3='' OR instr(lower(json_extract(a.record,'$.config.name')),lower(?3))>0
                    OR EXISTS(SELECT 1 FROM bubbles b WHERE b.actor_id=a.id AND instr(lower(b.content),lower(?3))>0))
                ORDER BY lower(json_extract(a.record,'$.config.name')),a.id LIMIT ?4");
            let mut statement = sql(db.prepare(&statement))?;
            let mut actors = sql(statement
                .query_map(params![scope, after, text, (limit + 1) as i64], |row| {
                    row.get::<_, String>(0)
                }))?
            .map(|row| decode::<ActorRecord>(sql(row)?))
            .collect::<Result<Vec<_>>>()?;
            let next_cursor = (actors.len() > limit).then(|| actors[limit - 1].id.clone());
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
        let page = store.contacts("core", "", None, 24).unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].actor.id, "core");
        assert!(
            store
                .contacts("core", "foreign", None, 24)
                .unwrap()
                .items
                .is_empty()
        );
        assert!(store.contacts("core", "", Some("foreign"), 24).is_err());
    }
}
