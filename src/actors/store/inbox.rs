use super::*;

fn same_authority(first: &MessageRecord, next: &MessageRecord) -> bool {
    first.remote_authority.is_none()
        && next.remote_authority.is_none()
        && matches!(
            (&first.source, &next.source),
            (MessageSource::User, MessageSource::User)
                | (MessageSource::Actor { .. }, MessageSource::Actor { .. })
        )
}

pub(super) fn execution_inputs(
    db: &Connection,
    input: &MessageRecord,
) -> Result<Vec<MessageRecord>> {
    let mut inputs = Vec::new();
    let mut after = 0;
    loop {
        let mut statement = sql(db.prepare("SELECT record FROM messages WHERE actor_id=?1 AND seq>?2 AND (status='\"running\"' OR id=?3 OR (?4 AND status='\"queued\"' AND json_extract(record,'$.started_at_ms') IS NOT NULL)) ORDER BY seq LIMIT 128"))?;
        let page = sql(statement.query_map(
            params![
                input.actor_id,
                after,
                input.id,
                input.status == MessageStatus::Queued
            ],
            |row| row.get::<_, String>(0),
        ))?
        .map(|row| decode(sql(row)?))
        .collect::<Result<Vec<MessageRecord>>>()?;
        if page.is_empty() {
            return Ok(inputs);
        }
        after = page.last().unwrap().seq;
        inputs.extend(
            page.into_iter()
                .filter(|next| next.id == input.id || same_authority(input, next)),
        );
    }
}

impl ActorStore {
    pub(crate) fn execution_inputs(&self, id: &str) -> Result<Vec<MessageRecord>> {
        database_io(|| {
            let db = self.db.lock();
            execution_inputs(&db, &message_tx(&db, id)?)
        })
    }

    pub(crate) fn active_input(&self, id: &str) -> Result<MessageRecord> {
        let inputs = self.execution_inputs(id)?;
        inputs
            .iter()
            .find(|input| input.status == MessageStatus::Running && !input.cancel_requested)
            .cloned()
            .or_else(|| inputs.into_iter().find(|input| input.id == id))
            .ok_or_else(|| missing("Execution input"))
    }

    /// Admit buffered messages only while idle or after a settled tool group.
    pub(crate) fn receive_inputs(&self, id: &str) -> Result<Vec<MessageRecord>> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            let current = message_tx(&tx, id)?;
            if current.status != MessageStatus::Running || current.cancel_requested {
                return Err(
                    ActorError::Forbidden("Execution input is no longer active".into()).into(),
                );
            }
            let shared = current.actor_id == crate::stage::shared::ACTOR_ID
                && sql(tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='core_order')",
                    [],
                    |row| row.get::<_, bool>(0),
                ))?;
            let query = if shared {
                "SELECT m.record FROM messages m LEFT JOIN core_order o ON o.entity='messages' AND o.remote_key=m.id WHERE m.actor_id=?1 AND m.status='\"queued\"' ORDER BY o.ordinal IS NULL,o.ordinal,m.seq LIMIT 128"
            } else {
                "SELECT record FROM messages WHERE actor_id=?1 AND status='\"queued\"' ORDER BY seq LIMIT 128"
            };
            let mut received = vec![current.clone()];
            'inbox: loop {
                let page = {
                    let mut statement = sql(tx.prepare(query))?;
                    sql(statement.query_map([&current.actor_id], |row| row.get::<_, String>(0)))?
                        .map(|row| decode(sql(row)?))
                        .collect::<Result<Vec<MessageRecord>>>()?
                };
                if page.is_empty() {
                    break;
                }
                for mut next in page {
                    if next.cancel_requested || !same_authority(&current, &next) {
                        break 'inbox;
                    }
                    next.status = MessageStatus::Running;
                    next.started_at_ms.get_or_insert(now());
                    next.updated_at_ms = now();
                    save_message(&tx, &next)?;
                    received.push(next);
                }
            }
            self.commit(tx)?;
            Ok(received)
        })
    }

    pub(crate) fn cancellation_pending(&self, id: &str) -> Result<bool> {
        database_io(|| {
            let db = self.db.lock();
            let input = message_tx(&db, id)?;
            sql(db.query_row("SELECT EXISTS(SELECT 1 FROM messages WHERE actor_id=?1 AND status='\"running\"' AND json_extract(record,'$.cancel_requested')=1)", [&input.actor_id], |row| row.get(0)))
        })
    }

    pub(crate) fn settle_cancellations(&self, id: &str) -> Result<()> {
        if !self.cancellation_pending(id)? {
            return Ok(());
        }
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            for input in execution_inputs(&tx, &message_tx(&tx, id)?)? {
                if input.status == MessageStatus::Running && input.cancel_requested {
                    finish_message_tx(
                        &tx,
                        &input.id,
                        MessageStatus::Cancelled,
                        None,
                        Some("Request cancelled; effects may have occurred"),
                    )?;
                    let notice = AgentMessage::user(format!(
                        "Input {} was cancelled. Continue other work; completed effects remain recorded.",
                        input.id
                    ));
                    sql(tx.execute(
                        "INSERT INTO history(actor_id,message_id,record) VALUES(?1,?2,?3)",
                        params![input.actor_id, input.id, encode(&notice)?],
                    ))?;
                }
            }
            self.commit(tx)
        })
    }
}
