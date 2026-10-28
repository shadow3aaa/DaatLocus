//! Device-local evidence of actual App execution. Never exported with observations.
use super::*;

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct NativeToolResult {
    pub actor_id: String,
    pub message_id: String,
    pub call: AgentToolCall,
    pub read_only: bool,
    started_at_ms: i64,
    authority: String,
    binding: String,
    effect_digest: Option<String>,
    #[serde(default)]
    completed: bool,
}

fn authority(input: &MessageRecord) -> Result<String> {
    // MessageRecord's public serialization intentionally omits private authority.
    Ok(crate::stage::digest(
        &serde_json::to_vec(&serde_json::json!({
            "actor": input.actor_id, "message": input.id, "source": input.source,
            "reply_to": input.reply_to, "kind": input.kind, "in_reply_to": input.in_reply_to,
            "remote": input.remote_authority,
        }))
        .into_diagnostic()?,
    ))
}

fn binding(result: &ToolResultRecord) -> Result<String> {
    Ok(crate::stage::digest(
        &serde_json::to_vec(&serde_json::json!({
            "id":result.id,"actor":result.actor_id,"message":result.message_id,
            "call":result.call_id,"name":result.name,"arguments":result.arguments,
            "started_at_ms":result.started_at_ms,
        }))
        .into_diagnostic()?,
    ))
}

pub(super) fn insert(
    tx: &Transaction<'_>,
    result: &ToolResultRecord,
    read_only: bool,
    effect_digest: Option<String>,
) -> Result<()> {
    let input = message_tx(tx, &result.message_id)?;
    if input.actor_id != result.actor_id
        || input.status != MessageStatus::Running
        || input.cancel_requested
    {
        return Err(ActorError::Forbidden("Tool request is not active".into()).into());
    }
    let native = NativeToolResult {
        actor_id: result.actor_id.clone(),
        message_id: result.message_id.clone(),
        call: AgentToolCall {
            id: result.call_id.clone(),
            name: result.name.clone(),
            arguments: result.arguments.clone(),
        },
        read_only,
        started_at_ms: result
            .started_at_ms
            .ok_or_else(|| ActorError::Invalid("Tool execution was not started".into()))?,
        authority: authority(&input)?,
        binding: binding(result)?,
        effect_digest,
        completed: false,
    };
    sql(tx.execute(
        "INSERT INTO stage_local_tool_results(id,record) VALUES(?1,?2)",
        params![result.id, encode(&native)?],
    ))?;
    Ok(())
}

pub(super) fn complete(tx: &Transaction<'_>, result: &ToolResultRecord, id: &str) -> Result<()> {
    // A remote result's reference belongs to its execution device, not this
    // coordinator observation. Never create or transfer provenance here.
    if id != result.id {
        return Ok(());
    }
    let mut native: NativeToolResult = decode(sql(tx.query_row(
        "SELECT record FROM stage_local_tool_results WHERE id=?1",
        [id],
        |r| r.get(0),
    ))?)?;
    if native.effect_digest.is_some()
        || native.binding != binding(result)?
        || result.finished_at_ms.is_none()
    {
        return Err(ActorError::Forbidden(
            "Completed App result does not match its local execution".into(),
        )
        .into());
    }
    native.completed = true;
    sql(tx.execute(
        "UPDATE stage_local_tool_results SET record=?1 WHERE id=?2",
        params![encode(&native)?, id],
    ))?;
    Ok(())
}

impl ActorStore {
    /// The effect fence and its local provenance must commit before device execution.
    pub(crate) fn shared_tool_started(
        &self,
        actor: &str,
        message: &str,
        call: &AgentToolCall,
        read_only: bool,
        digest: &str,
    ) -> Result<String> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            let result = ToolResultRecord {
                id: Uuid::new_v4().to_string(),
                actor_id: actor.into(),
                message_id: message.into(),
                call_id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
                payload: Value::Null,
                output_schema: None,
                model_content: String::new(),
                summary: String::new(),
                error: None,
                started_at_ms: Some(now()),
                finished_at_ms: None,
            };
            sql(tx.execute(
                "INSERT INTO core_app_effects VALUES(?1,?2,?3,'started',NULL)",
                params![message, call.id, digest],
            ))?;
            tool_provenance::insert(&tx, &result, read_only, Some(digest.into()))?;
            self.commit(tx)?;
            Ok(result.id)
        })
    }

    /// A structured approval decision or interrupted invocation is not a new
    /// result-based permission application, even if its observation UUID is known.
    pub(crate) fn discard_tool_result_reference(
        &self,
        actor: &str,
        message: &str,
        call: &str,
    ) -> Result<()> {
        database_io(|| {
            let mut db = self.db.lock();
            let tx = sql(db.transaction())?;
            sql(tx.execute(
                "DELETE FROM stage_local_tool_results WHERE id IN (SELECT id FROM tools WHERE actor_id=?1 AND message_id=?2 AND call_id=?3)",
                params![actor,message,call],
            ))?;
            self.commit(tx)
        })
    }

    pub(crate) fn native_tool_result(
        &self,
        id: &str,
        actor: &str,
        message: &str,
    ) -> Result<NativeToolResult> {
        database_io(|| {
            let db = self.db.lock();
            let native: NativeToolResult = decode(
                sql(db
                    .query_row(
                        "SELECT record FROM stage_local_tool_results WHERE id=?1",
                        [id],
                        |r| r.get(0),
                    )
                    .optional())?
                .ok_or_else(|| missing("Local tool execution reference"))?,
            )?;
            let input: MessageRecord = decode(sql(db.query_row(
                "SELECT record FROM messages WHERE id=?1",
                [message],
                |r| r.get(0),
            ))?)?;
            if native.actor_id != actor
                || native.message_id != message
                || input.actor_id != actor
                || input.status != MessageStatus::Running
                || input.cancel_requested
                || authority(&input)? != native.authority
            {
                return Err(ActorError::Forbidden(
                    "Tool result reference is outside the active request authority".into(),
                )
                .into());
            }
            if let Some(expected) = &native.effect_digest {
                let effect: Option<(String,String,Option<Vec<u8>>)> = sql(db.query_row(
                    "SELECT digest,state,result FROM core_app_effects WHERE message=?1 AND call=?2",
                    params![message,native.call.id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
                ).optional())?;
                let Some((digest, state, Some(bytes))) = effect else {
                    return Err(
                        ActorError::Forbidden("Device tool result is not complete".into()).into(),
                    );
                };
                let result: crate::app::AppToolExecutionResult =
                    serde_json::from_slice(&bytes).into_diagnostic()?;
                if digest != *expected
                    || state != "completed"
                    || result
                        .permission_reference
                        .as_ref()
                        .is_none_or(|reference| reference.tool_result_id != id)
                {
                    return Err(ActorError::Forbidden(
                        "Device tool result reference does not match its execution".into(),
                    )
                    .into());
                }
            } else {
                let (data, done): (String, bool) = sql(db.query_row(
                    "SELECT record,done FROM tools WHERE id=?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                ))?;
                let result: ToolResultRecord = decode(data)?;
                if !native.completed
                    || !done
                    || result.finished_at_ms.is_none()
                    || binding(&result)? != native.binding
                {
                    return Err(ActorError::Forbidden(
                        "Tool result reference does not match a completed local execution".into(),
                    )
                    .into());
                }
            }
            Ok(native)
        })
    }
}
