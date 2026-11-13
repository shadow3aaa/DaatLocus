//! Session catalog and admission derive from the session Core identity.
use super::*;

pub(super) fn ensure_admission(db: &Connection, actor: &ActorRecord) -> Result<()> {
    let stage: String = sql(db.query_row(
        "SELECT value FROM stage_public_meta WHERE key='stage'",
        [],
        |r| r.get(0),
    ))?;
    let local = serde_json::from_value(Value::String(stage)).into_diagnostic()?;
    crate::stage::session::admission(db, actor, local).map_err(|e| miette::miette!("{e}"))?;
    let bound: bool = sql(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM actor_session_hosts WHERE actor=?1 AND host<>?2)",
        params![actor.id, local.to_string()],
        |r| r.get(0),
    ))?;
    let archived: bool = !bound && sql(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM actors WHERE id=?1 AND json_extract(record,'$.role')='session' AND json_extract(record,'$.status')='archived')",
        [&actor.session_id], |row| row.get(0),
    ))?;
    if actor.status == ActorStatus::Archived || archived {
        return Err(ActorError::Conflict("Session or Actor is archived".into()).into());
    }
    Ok(())
}

pub(super) fn summary(db: &Connection, id: &str) -> Result<SessionSummary> {
    let actor = read_actor(db, id)?;
    if actor.role != ActorRole::Session || actor.session_id != id {
        return Err(missing("Session"));
    }
    let (active, working, activity): (bool, i64, i64) = sql(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages m JOIN actors a ON a.id=m.actor_id WHERE json_extract(a.record,'$.session_id')=?1 AND m.status IN ('\"queued\"','\"running\"')),
         (SELECT count(*) FROM actors a WHERE json_extract(a.record,'$.session_id')=?1 AND a.id<>?1 AND NOT EXISTS(SELECT 1 FROM actor_session_hosts h WHERE h.actor=a.id) AND EXISTS(SELECT 1 FROM messages m WHERE m.actor_id=a.id AND m.status='\"running\"')) + (SELECT count(*) FROM session_member_facts WHERE session=?1 AND json_extract(record,'$.status')='running'),
         max(COALESCE((SELECT max(json_extract(m.record,'$.updated_at_ms')) FROM messages m JOIN actors a ON a.id=m.actor_id WHERE json_extract(a.record,'$.session_id')=?1),?2),COALESCE((SELECT max(json_extract(record,'$.updated_at_ms')) FROM session_member_facts WHERE session=?1),?2))",
        params![id, actor.created_at_ms], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
    ))?;
    let stage: String = sql(db.query_row(
        "SELECT value FROM stage_public_meta WHERE key='stage'",
        [],
        |row| row.get(0),
    ))?;
    let stage_id = serde_json::from_value(Value::String(stage)).into_diagnostic()?;
    Ok(SessionSummary {
        id: id.into(),
        stage_id,
        title: actor.title,
        status: if actor.status == ActorStatus::Archived {
            ActorStatus::Archived
        } else if active {
            ActorStatus::Running
        } else {
            ActorStatus::Idle
        },
        working_actors: working as usize,
        model: actor.config.model,
        created_at_ms: actor.created_at_ms,
        updated_at_ms: actor.updated_at_ms.max(activity),
        last_activity_at_ms: activity,
        conversation_url: format!("/actors/{id}"),
    })
}

pub(super) fn changed(tx: &Transaction<'_>, session: &str) -> Result<()> {
    let exists: bool = sql(tx.query_row("SELECT EXISTS(SELECT 1 FROM actors WHERE id=?1 AND json_extract(record,'$.role')='session')", [session], |row| row.get(0)))?;
    if exists {
        event(
            tx,
            session,
            ActorEvent::SessionChanged {
                session: summary(tx, session)?,
            },
        )?;
    }
    Ok(())
}

pub(crate) fn normalize_title(title: Option<String>) -> Option<String> {
    title.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

impl ActorStore {
    pub(crate) fn receive_session_member_fact(
        &self,
        stage: crate::stage::StageId,
        session: &str,
        revision: i64,
        actor: &ActorSummary,
    ) -> Result<()> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            let member:bool=sql(tx.query_row("SELECT EXISTS(SELECT 1 FROM session_members WHERE session=?1 AND actor=?2 AND stage=?3)",params![session,actor.id,stage.to_string()],|r|r.get(0)))?;
            if !member || revision <= 0 {
                return Err(ActorError::Forbidden("Unknown session member".into()).into());
            }
            let exists:bool=sql(tx.query_row("SELECT EXISTS(SELECT 1 FROM actors WHERE id=?1 AND json_extract(record,'$.role')='session')",[session],|r|r.get(0)))?;
            if !exists {
                return Ok(());
            } // A tombstone never resurrects a catalog entry.
            let mut actor = actor.clone();
            actor.conversation_url = crate::stage::ActorAddress {
                stage_id: stage,
                actor_id: actor.id.clone(),
            }
            .conversation_url();
            let count=sql(tx.execute("INSERT INTO session_member_facts VALUES(?1,?2,?3,?4,?5) ON CONFLICT(session,actor) DO UPDATE SET revision=excluded.revision,record=excluded.record WHERE revision<excluded.revision",params![session,actor.id,stage.to_string(),revision,encode(&actor)?]))?;
            if count > 0 {
                let id = actor.id.clone();
                event(&tx, &id, ActorEvent::ActorChanged { actor })?;
                changed(&tx, session)?;
            }
            self.commit(tx)
        })
    }
    pub(crate) fn ensure_actor_admission(&self, id: &str) -> Result<()> {
        database_io(|| {
            let db = self.db.lock();
            let mut actor = read_actor(&db, id)?;
            // Worker archival leaves its already-running request free to finish.
            if actor.role == ActorRole::Worker {
                actor.status = ActorStatus::Idle;
            }
            ensure_admission(&db, &actor)
        })
    }
    pub fn session_summary(&self, id: &str) -> Result<SessionSummary> {
        database_io(|| summary(&self.db.lock(), id))
    }

    pub(crate) fn session_page(
        &self,
        archived: bool,
        after: Option<&str>,
        limit: usize,
    ) -> Result<(Vec<SessionSummary>, Option<String>)> {
        database_io(|| {
            let cursor: Option<(i64, String)> = after
                .map(serde_json::from_str)
                .transpose()
                .map_err(|_| ActorError::Invalid("Invalid session cursor".into()))?;
            let db = self.db.lock();
            let mut query = sql(db.prepare("WITH catalog AS (
                SELECT a.id,COALESCE((SELECT max(json_extract(m.record,'$.updated_at_ms')) FROM messages m JOIN actors w ON w.id=m.actor_id WHERE json_extract(w.record,'$.session_id')=a.id),json_extract(a.record,'$.created_at_ms')) activity
                FROM actors a WHERE json_extract(a.record,'$.role')='session' AND (json_extract(a.record,'$.status')='archived')=?1)
                SELECT id FROM catalog WHERE ?2 IS NULL OR (activity,id)<(?2,?3) ORDER BY activity DESC,id DESC LIMIT ?4"))?;
            let ids = sql(query.query_map(
                params![
                    archived,
                    cursor.as_ref().map(|c| c.0),
                    cursor.as_ref().map(|c| c.1.as_str()),
                    (limit + 1) as i64
                ],
                |row| row.get::<_, String>(0),
            ))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .into_diagnostic()?;
            let mut items = ids
                .into_iter()
                .map(|id| summary(&db, &id))
                .collect::<Result<Vec<_>>>()?;
            let next = if items.len() > limit {
                let last = &items[limit - 1];
                Some(encode(&(last.last_activity_at_ms, &last.id))?)
            } else {
                None
            };
            items.truncate(limit);
            Ok((items, next))
        })
    }

    pub(crate) fn create_session(
        &self,
        actor: &ActorRecord,
        key: Option<&str>,
        request: &str,
    ) -> Result<ActorRecord> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            if let Some(key) = key {
                let previous: Option<(String, String)> = sql(tx
                    .query_row(
                        "SELECT actor_id,request FROM session_creation_keys WHERE key=?1",
                        [key],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional())?;
                if let Some((id, original)) = previous {
                    if original != request {
                        return Err(ActorError::Conflict(
                            "Idempotency key was used for another session".into(),
                        )
                        .into());
                    }
                    return actor_tx(&tx, &id);
                }
            }
            save_actor(&tx, actor)?;
            let enrolled: bool = sql(tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='stage_meta')",
                [],
                |r| r.get(0),
            ))?;
            if enrolled {
                let root: Option<String> = sql(tx
                    .query_row("SELECT value FROM stage_meta WHERE key='root'", [], |r| {
                        r.get(0)
                    })
                    .optional())?;
                if let Some(root) = root {
                    let root: crate::stage::identity::TrustRoot = decode(root)?;
                    crate::stage::ledger::bind_account_tree(&tx, &root.group_id)
                        .map_err(|e| miette::miette!("{e}"))?;
                }
            }
            if let Some(key) = key {
                sql(tx.execute(
                    "INSERT INTO session_creation_keys VALUES(?1,?2,?3)",
                    params![key, actor.id, request],
                ))?;
            }
            self.commit(tx)?;
            Ok(actor.clone())
        })
    }

    pub(crate) fn patch_session(
        &self,
        id: &str,
        title: Option<Option<String>>,
        model: Option<Option<String>>,
    ) -> Result<()> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            let mut actor = actor_tx(&tx, id)?;
            if actor.role != ActorRole::Session {
                return Err(missing("Session"));
            }
            if let Some(title) = title {
                actor.title = normalize_title(title);
            }
            if let Some(model) = model {
                actor.config.model = model;
            }
            actor.updated_at_ms = now().max(actor.updated_at_ms + 1);
            save_actor(&tx, &actor)?;
            self.commit(tx)
        })
    }

    pub(crate) fn session_actors(
        &self,
        session: &str,
        after: Option<&str>,
        running: bool,
        limit: usize,
    ) -> Result<Vec<ActorSummary>> {
        database_io(|| {
            let db = self.db.lock();
            summary(&db, session)?;
            let mut query=sql(db.prepare("SELECT record FROM (SELECT id,record FROM actors a WHERE json_extract(record,'$.session_id')=?1 AND json_extract(record,'$.role')='worker' AND NOT EXISTS(SELECT 1 FROM actor_session_hosts h WHERE h.actor=a.id) AND (NOT ?3 OR EXISTS(SELECT 1 FROM messages m WHERE m.actor_id=a.id AND m.status='\"running\"')) UNION ALL SELECT actor id,record FROM session_member_facts WHERE session=?1 AND (NOT ?3 OR json_extract(record,'$.status')='running')) WHERE id>?2 ORDER BY id LIMIT ?4"))?;
            sql(query.query_map(
                params![session, after.unwrap_or(""), running, limit as i64],
                |row| row.get::<_, String>(0),
            ))?
            .map(|r| {
                let value: Value = decode(sql(r)?)?;
                if value.get("config").is_some() {
                    Ok(serde_json::from_value::<ActorRecord>(value)
                        .into_diagnostic()?
                        .summary())
                } else {
                    serde_json::from_value::<ActorSummary>(value).into_diagnostic()
                }
            })
            .collect()
        })
    }

    pub(crate) fn set_session_archived(&self, id: &str, archived: bool) -> Result<Vec<String>> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            let mut actor = actor_tx(&tx, id)?;
            if actor.role != ActorRole::Session {
                return Err(missing("Session"));
            }
            let ids = {
                let mut q=sql(tx.prepare("SELECT id FROM actors a WHERE json_extract(record,'$.session_id')=?1 AND NOT EXISTS(SELECT 1 FROM actor_session_hosts h WHERE h.actor=a.id) ORDER BY id"))?;
                sql(q.query_map([id], |r| r.get::<_, String>(0)))?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .into_diagnostic()?
            };
            actor.status = if archived {
                ActorStatus::Archived
            } else {
                ActorStatus::Idle
            };
            actor.updated_at_ms = now().max(actor.updated_at_ms + 1);
            save_actor(&tx, &actor)?;
            crate::stage::session::lifecycle(&tx, id, if archived { "archived" } else { "active" })
                .map_err(|e| miette::miette!("{e}"))?;
            self.commit(tx)?;
            Ok(ids)
        })
    }

    pub(crate) fn delete_session(&self, id: &str) -> Result<Vec<String>> {
        self.delete_session_members(id, None)
    }
    pub(crate) fn delete_session_members(
        &self,
        id: &str,
        host: Option<crate::stage::StageId>,
    ) -> Result<Vec<String>> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            if host.is_none() {
                let actor = actor_tx(&tx, id)?;
                if actor.role != ActorRole::Session {
                    return Err(missing("Session"));
                }
                if actor.status != ActorStatus::Archived {
                    return Err(ActorError::Conflict(
                        "Archive the session before deleting it".into(),
                    )
                    .into());
                }
                crate::stage::session::lifecycle(&tx, id, "deleted")
                    .map_err(|e| miette::miette!("{e}"))?;
            }
            let ids = {
                let mut q = sql(tx.prepare(
                    "SELECT a.id FROM actors a LEFT JOIN actor_session_hosts h ON h.actor=a.id WHERE json_extract(record,'$.session_id')=?1 AND ((?2 IS NULL AND h.host IS NULL) OR h.host=?2)",
                ))?;
                sql(q.query_map(params![id, host.map(|h| h.to_string())], |r| {
                    r.get::<_, String>(0)
                }))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .into_diagnostic()?
            };
            for actor in &ids {
                sql(tx.execute(
                    "INSERT OR IGNORE INTO actor_cleanup_pending VALUES(?1)",
                    [actor],
                ))?;
                sql(tx.execute("DELETE FROM stage_blobs WHERE hash IN (SELECT hash FROM stage_blob_actors WHERE actor=?1) AND NOT EXISTS(SELECT 1 FROM stage_blob_actors b WHERE b.hash=stage_blobs.hash AND b.actor<>?1)",[actor]))?;
                for table in [
                    "actor_text_heads",
                    "actor_event_heads",
                    "stage_snapshot_pages",
                    "stage_send_keys",
                    "stage_resources",
                    "stage_resource_files",
                    "stage_message_content",
                    "stage_inbox",
                    "stage_blob_actors",
                    "stage_content_sources",
                ] {
                    let exists: bool = sql(tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
                        [table],
                        |row| row.get(0),
                    ))?;
                    if exists {
                        sql(tx.execute(&format!("DELETE FROM {table} WHERE actor=?1"), [actor]))?;
                    }
                }
                sql(tx.execute(
                    "DELETE FROM stage_sent WHERE json_extract(record,'$.local_actor')=?1",
                    [actor],
                ))?;
                sql(tx.execute("DELETE FROM stage_ready_deliveries WHERE json_extract(CAST(record AS TEXT),'$.body.actor.actor_id')=?1",[actor]))?;
                let outbox: bool = sql(tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='stage_outbox')",
                    [],
                    |row| row.get(0),
                ))?;
                if outbox {
                    sql(tx.execute("DELETE FROM stage_outbox WHERE json_extract(CAST(record AS TEXT),'$.body.origin.actor.actor_id')=?1 OR json_extract(CAST(record AS TEXT),'$.body.actor.actor_id')=?1",[actor]))?;
                }
                sql(tx.execute("DELETE FROM stage_delivery_receipts WHERE local_message IN (SELECT id FROM messages WHERE actor_id=?1)",[actor]))?;
                let local_approvals: bool = sql(tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='stage_local_approvals')",
                    [],
                    |row| row.get(0),
                ))?;
                if local_approvals {
                    sql(tx.execute("DELETE FROM stage_local_approvals WHERE id IN (SELECT id FROM approvals WHERE message_id IN (SELECT id FROM messages WHERE actor_id=?1))",[actor]))?;
                }
                sql(tx.execute("DELETE FROM stage_local_tool_results WHERE id IN (SELECT id FROM tools WHERE actor_id=?1)",[actor]))?;
                sql(tx.execute("DELETE FROM events WHERE actor_id=?1", [actor]))?;
                sql(tx.execute("DELETE FROM actors WHERE id=?1", [actor]))?;
                event(
                    &tx,
                    actor,
                    ActorEvent::ActorDeleted {
                        actor_id: actor.clone(),
                    },
                )?;
            }
            event(
                &tx,
                id,
                ActorEvent::SessionDeleted {
                    session_id: id.into(),
                },
            )?;
            self.commit(tx)?;
            Ok(ids)
        })
    }
    pub(crate) fn pending_actor_cleanup(&self) -> Result<Vec<String>> {
        database_io(|| {
            let db = self.db.lock();
            let mut query = sql(db.prepare("SELECT actor FROM actor_cleanup_pending"))?;
            sql(query.query_map([], |row| row.get(0)))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .into_diagnostic()
        })
    }
    pub(crate) fn finish_actor_cleanup(&self, id: &str) -> Result<()> {
        database_io(|| {
            sql(self
                .db
                .lock()
                .execute("DELETE FROM actor_cleanup_pending WHERE actor=?1", [id]))?;
            Ok(())
        })
    }
}
