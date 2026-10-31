use super::*;

fn compatible(first: &MessageRecord, next: &MessageRecord) -> bool {
    // Owner instructions must not lend their authority to collaboration input.
    // Remote invocations also keep their individual authenticated provenance.
    let same_source_authority = matches!(
        (&first.source, &next.source),
        (MessageSource::User, MessageSource::User)
            | (MessageSource::Actor { .. }, MessageSource::Actor { .. })
    );
    same_source_authority
        && first.remote_authority.is_none()
        && next.remote_authority.is_none()
        && first.reply_to == next.reply_to
        && !matches!(first.reply_to, ReplyTarget::Remote { .. })
        && !next.cancel_requested
}

pub(super) fn members(db: &Connection, input: &MessageRecord) -> Result<Vec<MessageRecord>> {
    let Some(batch) = &input.input_batch else {
        return Ok(vec![input.clone()]);
    };
    let mut result = Vec::new();
    let mut after = 0;
    loop {
        let mut query = sql(db.prepare("SELECT record FROM messages WHERE actor_id=?1 AND seq>?2 AND json_extract(record,'$.input_batch')=?3 ORDER BY seq LIMIT 128"))?;
        let page = sql(
            query.query_map(params![input.actor_id, after, batch], |row| {
                row.get::<_, String>(0)
            }),
        )?
        .map(|row| decode(sql(row)?))
        .collect::<Result<Vec<MessageRecord>>>()?;
        if page.is_empty() {
            return Ok(result);
        }
        after = page.last().unwrap().seq;
        result.extend(page);
    }
}

impl ActorStore {
    /// Coalesce only the contiguous, unstarted inbox prefix. Bubbles and source
    /// history remain separate; the durable link survives Core worker takeover.
    pub(crate) fn claim_input_batch(&self, id: &str) -> Result<Vec<MessageRecord>> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            let mut first = message_tx(&tx, id)?;
            let mut batch = members(&tx, &first)?;
            if first.input_batch.is_none() {
                let started: bool = sql(tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM history WHERE actor_id=?1 AND message_id=?2)",
                    params![first.actor_id, first.id],
                    |row| row.get(0),
                ))?;
                if !started && !first.cancel_requested {
                    let shared: bool = sql(tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='core_order')",
                        [],
                        |row| row.get(0),
                    ))?;
                    let query = if shared && first.actor_id == crate::stage::shared::ACTOR_ID {
                        "SELECT m.record FROM messages m LEFT JOIN core_order o ON o.entity='messages' AND o.remote_key=m.id WHERE m.actor_id=?1 AND m.status='\"queued\"' ORDER BY o.ordinal IS NULL,o.ordinal,m.seq LIMIT 128"
                    } else {
                        "SELECT record FROM messages WHERE actor_id=?1 AND status='\"queued\"' ORDER BY seq LIMIT 128"
                    };
                    'prefix: loop {
                        let page = {
                            let mut stmt = sql(tx.prepare(query))?;
                            sql(stmt.query_map([&first.actor_id], |row| row.get::<_, String>(0)))?
                                .map(|row| decode(sql(row)?))
                                .collect::<Result<Vec<MessageRecord>>>()?
                        };
                        if page.is_empty() {
                            break;
                        }
                        for mut next in page {
                            let started: bool = sql(tx.query_row(
                                "SELECT EXISTS(SELECT 1 FROM history WHERE message_id=?1)",
                                [&next.id],
                                |row| row.get(0),
                            ))?;
                            if next.input_batch.is_some()
                                || next.started_at_ms.is_some()
                                || started
                                || !compatible(&first, &next)
                            {
                                break 'prefix;
                            }
                            next.input_batch = Some(first.id.clone());
                            next.status = MessageStatus::Running;
                            next.started_at_ms = Some(now());
                            next.updated_at_ms = now();
                            save_message(&tx, &next)?;
                            batch.push(next);
                        }
                    }
                    if batch.len() > 1 {
                        first.input_batch = Some(first.id.clone());
                        save_message(&tx, &first)?;
                        batch[0] = first;
                    }
                }
            } else {
                for member in &mut batch {
                    if member.status == MessageStatus::Queued {
                        member.status = MessageStatus::Running;
                        member.started_at_ms.get_or_insert(now());
                        member.updated_at_ms = now();
                        save_message(&tx, member)?;
                    }
                }
            }
            self.commit(tx)?;
            Ok(batch)
        })
    }

    pub(crate) fn input_batch(&self, id: &str) -> Result<Vec<MessageRecord>> {
        database_io(|| {
            let db = self.db.lock();
            members(&db, &message_tx(&db, id)?)
        })
    }

    pub(crate) fn active_batch_input(&self, id: &str) -> Result<MessageRecord> {
        let inputs = self.input_batch(id)?;
        inputs
            .iter()
            .find(|input| input.status == MessageStatus::Running && !input.cancel_requested)
            .cloned()
            .or_else(|| inputs.into_iter().find(|input| input.id == id))
            .ok_or_else(|| missing("Input batch"))
    }

    pub(crate) fn batch_cancellation_pending(&self, id: &str) -> Result<bool> {
        database_io(|| {
            let db = self.db.lock();
            let input = message_tx(&db, id)?;
            if let Some(batch) = input.input_batch {
                sql(db.query_row("SELECT EXISTS(SELECT 1 FROM messages WHERE actor_id=?1 AND status='\"running\"' AND json_extract(record,'$.input_batch')=?2 AND json_extract(record,'$.cancel_requested')=1)",
                    params![input.actor_id,batch], |row| row.get(0)))
            } else {
                Ok(input.status == MessageStatus::Running && input.cancel_requested)
            }
        })
    }

    /// Cancel only the exact input. The remaining batch continues from durable
    /// tool observations, with the cancellation visible before more inference.
    pub(crate) fn settle_batch_cancellations(&self, id: &str) -> Result<()> {
        if !self.batch_cancellation_pending(id)? {
            return Ok(());
        }
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            for member in members(&tx, &message_tx(&tx, id)?)? {
                if member.status == MessageStatus::Running && member.cancel_requested {
                    finish_message_tx(
                        &tx,
                        &member.id,
                        MessageStatus::Cancelled,
                        None,
                        Some("Request cancelled; effects may have occurred"),
                        true,
                    )?;
                    // This is an execution fact, never Owner authorization.
                    let notice = AgentMessage::user(format!(
                        "Input {} was cancelled. Do not continue its unfinished work; completed effects remain recorded.",
                        member.id
                    ));
                    sql(tx.execute(
                        "INSERT INTO history(actor_id,message_id,record) VALUES(?1,?2,?3)",
                        params![member.actor_id, member.id, encode(&notice)?],
                    ))?;
                }
            }
            self.commit(tx)
        })
    }
}
