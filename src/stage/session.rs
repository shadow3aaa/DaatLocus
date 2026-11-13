//! Session-scoped native coordination. Only references cross devices, never execution contexts.
use super::{ActorAddress, Result, StageError, StageId, protocol::Body};
use crate::{
    actors::{ActorRecord, ActorRole, ActorStatus, ActorSummary, ActorSystem},
    app::ActorCallContext,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub(crate) fn initialize(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch("CREATE TABLE IF NOT EXISTS actor_session_hosts(actor TEXT PRIMARY KEY REFERENCES actors(id) ON DELETE CASCADE,host TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS session_members(session TEXT NOT NULL,actor TEXT NOT NULL,stage TEXT NOT NULL,name TEXT NOT NULL,request TEXT NOT NULL,record TEXT NOT NULL,PRIMARY KEY(session,actor),UNIQUE(session,name));
        CREATE TABLE IF NOT EXISTS session_lifecycle(host TEXT NOT NULL,session TEXT NOT NULL,revision INTEGER NOT NULL,state TEXT NOT NULL,PRIMARY KEY(host,session));
        CREATE TABLE IF NOT EXISTS session_lifecycle_outbox(peer TEXT NOT NULL,session TEXT NOT NULL,revision INTEGER NOT NULL,state TEXT NOT NULL,PRIMARY KEY(peer,session));
        CREATE TABLE IF NOT EXISTS session_appeal_outbox(id TEXT PRIMARY KEY,peer TEXT NOT NULL,record TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS session_member_facts(session TEXT NOT NULL,actor TEXT NOT NULL,stage TEXT NOT NULL,revision INTEGER NOT NULL,record TEXT NOT NULL,PRIMARY KEY(session,actor));
        CREATE TABLE IF NOT EXISTS session_fact_heads(actor TEXT PRIMARY KEY REFERENCES actors(id) ON DELETE CASCADE,revision INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS session_fact_outbox(actor TEXT PRIMARY KEY REFERENCES actors(id) ON DELETE CASCADE,peer TEXT NOT NULL,revision INTEGER NOT NULL,record TEXT NOT NULL);")?;
    Ok(())
}
pub(crate) fn address(
    db: &Connection,
    actor: &ActorRecord,
    local: StageId,
) -> Result<ActorAddress> {
    let host: Option<String> = db
        .query_row(
            "SELECT host FROM actor_session_hosts WHERE actor=?1",
            [&actor.id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(ActorAddress {
        stage_id: host
            .map(|s| serde_json::from_value(json!(s)))
            .transpose()
            .map_err(|_| StageError::Invalid("Invalid session host"))?
            .unwrap_or(local),
        actor_id: actor.session_id.clone(),
    })
}
pub(crate) fn actor_session(system: &ActorSystem, id: &str) -> Result<ActorAddress> {
    let actor = system.actor(id).map_err(report)?;
    let local = system.store().public_stage_id()?;
    system
        .store()
        .stage_transaction(|tx| address(tx, &actor, local))
}
pub(crate) fn admission(db: &Connection, actor: &ActorRecord, local: StageId) -> Result<()> {
    let session = address(db, actor, local)?;
    let state: Option<String> = db
        .query_row(
            "SELECT state FROM session_lifecycle WHERE host=?1 AND session=?2",
            params![session.stage_id.to_string(), session.actor_id],
            |r| r.get(0),
        )
        .optional()?;
    if state.as_deref().is_some_and(|s| s != "active") {
        return Err(StageError::Conflict("Session is archived or deleted"));
    }
    Ok(())
}
pub(crate) fn lifecycle(tx: &Transaction<'_>, session: &str, state: &str) -> Result<()> {
    let local: String = tx.query_row(
        "SELECT value FROM stage_public_meta WHERE key='stage'",
        [],
        |r| r.get(0),
    )?;
    tx.execute("INSERT INTO session_lifecycle VALUES(?1,?2,1,?3) ON CONFLICT(host,session) DO UPDATE SET revision=revision+1,state=excluded.state",params![local,session,state])?;
    tx.execute("INSERT INTO session_lifecycle_outbox SELECT DISTINCT stage,?1,(SELECT revision FROM session_lifecycle WHERE host=?2 AND session=?1),?3 FROM session_members WHERE session=?1 AND stage<>?2 ON CONFLICT(peer,session) DO UPDATE SET revision=excluded.revision,state=excluded.state",params![session,local,state])?;
    Ok(())
}
pub(crate) fn report(error: miette::Report) -> StageError {
    if let Some(error) = error.downcast_ref::<crate::actors::ActorError>() {
        return match error {
            crate::actors::ActorError::Forbidden(_) => {
                StageError::Forbidden("Session operation is not authorized")
            }
            crate::actors::ActorError::NotFound(_) => {
                StageError::Forbidden("Session resource is unavailable")
            }
            crate::actors::ActorError::Conflict(_) => StageError::Conflict("Session state changed"),
            crate::actors::ActorError::Invalid(_) => {
                StageError::Invalid("Invalid session operation")
            }
            crate::actors::ActorError::Unavailable(_) => {
                StageError::Unavailable("Session operation is unavailable")
            }
        };
    }
    StageError::Storage(std::io::Error::other(error.to_string()))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Scope {
    pub actor: ActorAddress,
    pub session: ActorAddress,
    pub role: ActorRole,
    pub depth: u32,
    pub message: String,
    pub owner_requested: bool,
}
pub(crate) fn scope(system: &ActorSystem, caller: &ActorCallContext) -> Result<Scope> {
    let actor = system
        .record_for_tool(&caller.actor_id, caller)
        .map_err(report)?;
    if actor.remote_read_only {
        return Err(StageError::Forbidden(
            "Read-only input cannot coordinate session operations",
        ));
    }
    let input = system.store().message(&caller.message_id).map_err(report)?;
    system.ensure_delegation_authority(&input).map_err(report)?;
    Ok(Scope {
        actor: ActorAddress {
            stage_id: system.store().public_stage_id()?,
            actor_id: actor.id.clone(),
        },
        session: actor_session(system, &actor.id)?,
        role: actor.role,
        depth: actor.depth,
        message: caller.message_id.clone(),
        owner_requested: input.source == crate::actors::MessageSource::User
            && input.remote_authority.is_none()
            && !crate::conversation_import::is_import(&input),
    })
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Reservation {
    pub id: String,
    pub scope: Scope,
    pub target: StageId,
    pub name: String,
    pub key: String,
    pub digest: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Operation {
    Validate,
    MemberFact {
        revision: i64,
        actor: ActorSummary,
    },
    Reserve {
        target: StageId,
        name: String,
        key: String,
        digest: String,
    },
    Verify {
        reservation: Reservation,
    },
    Create {
        reservation: Reservation,
        config: crate::actors::ActorConfig,
        roots: crate::sandbox::SandboxScopeRequest,
    },
    List {
        offset: usize,
        limit: usize,
    },
    Conversation {
        actor: String,
        cursor: Option<String>,
        limit: usize,
        budget: usize,
    },
    Admin {
        actor: String,
        action: String,
        model: Option<String>,
    },
    AppealRead {
        id: String,
    },
    AppealApply {
        authorization: crate::actors::ApprovalAppealAuthorization,
    },
    AppealNotice {
        id: String,
        content: String,
    },
    Lifecycle {
        session: ActorAddress,
        revision: i64,
        state: String,
    },
    Skills {
        call: crate::model_protocol::AgentToolCall,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub scope: Scope,
    pub operation: Operation,
}

pub(crate) fn target(system: &ActorSystem, stage: Option<&str>) -> Result<StageId> {
    stage
        .map(|s| serde_json::from_value(json!(s)))
        .transpose()
        .map_err(|_| StageError::Invalid("Invalid target device"))?
        .map_or_else(|| system.store().public_stage_id(), Ok)
}
pub(crate) async fn execute(
    system: &ActorSystem,
    caller: &ActorCallContext,
    stage: Option<&str>,
    operation: Operation,
) -> Result<Value> {
    let request = Request {
        scope: scope(system, caller)?,
        operation,
    };
    let value = call(system, target(system, stage)?, request).await?;
    system
        .record_for_tool(&caller.actor_id, caller)
        .map_err(report)?;
    Ok(value)
}
pub(crate) async fn create(
    system: &ActorSystem,
    caller: &ActorCallContext,
    stage: StageId,
    key: String,
    config: crate::actors::ActorConfig,
    roots: crate::sandbox::SandboxScopeRequest,
) -> Result<Value> {
    let scope = scope(system, caller)?;
    let digest = super::digest(
        &serde_json::to_vec(&(&config, &roots))
            .map_err(|_| StageError::Invalid("Invalid creation input"))?,
    );
    let value = call(
        system,
        scope.session.stage_id,
        Request {
            scope: scope.clone(),
            operation: Operation::Reserve {
                target: stage,
                name: config.name.clone(),
                key,
                digest,
            },
        },
    )
    .await?;
    let reservation = serde_json::from_value(value)
        .map_err(|_| StageError::Invalid("Invalid creation reservation"))?;
    system
        .record_for_tool(&caller.actor_id, caller)
        .map_err(report)?;
    call(
        system,
        stage,
        Request {
            scope,
            operation: Operation::Create {
                reservation,
                config,
                roots,
            },
        },
    )
    .await
}

pub(crate) async fn call(system: &ActorSystem, target: StageId, request: Request) -> Result<Value> {
    if target == system.store().public_stage_id()? {
        return Box::pin(dispatch(system.clone(), target, request)).await;
    }
    let runtime = system.device_runtime()?;
    let handle = runtime.handle(target)?;
    let bytes = serde_json::to_vec(&request)
        .map_err(|_| StageError::Invalid("Cannot encode session operation"))?;
    let (content, export) = super::owner_http::export(&runtime.host, target, &bytes)?;
    let Body::SessionReply { content, export } =
        handle.rpc(Body::SessionRequest { content, export }).await?
    else {
        return Err(StageError::Invalid("Invalid session response"));
    };
    let bytes = handle.exported_content(export, content).await?;
    serde_json::from_slice(&bytes).map_err(|_| StageError::Invalid("Invalid session response"))
}
fn same_session(system: &ActorSystem, scope: &Scope, actor: &str) -> Result<ActorRecord> {
    let target = system.actor(actor).map_err(report)?;
    if actor_session(system, actor)? != scope.session {
        return Err(StageError::Forbidden("Actor belongs to another session"));
    }
    system
        .store()
        .ensure_actor_admission(actor)
        .map_err(report)?;
    Ok(target)
}
fn core(scope: &Scope) -> Result<()> {
    if scope.role != ActorRole::Session || scope.actor != scope.session {
        return Err(StageError::Forbidden("Operation requires the session Core"));
    }
    Ok(())
}
fn verify_member(system: &ActorSystem, scope: &Scope) -> Result<()> {
    let local = system.store().public_stage_id()?;
    if scope.session.stage_id != local {
        return Err(StageError::Forbidden("Not the session host"));
    }
    let root = system.actor(&scope.session.actor_id).map_err(report)?;
    if root.role != ActorRole::Session {
        return Err(StageError::Forbidden("Invalid session root"));
    }
    system
        .store()
        .ensure_actor_admission(&root.id)
        .map_err(report)?;
    if scope.actor == scope.session {
        let actor = system.actor(&scope.actor.actor_id).map_err(report)?;
        if actor.role != ActorRole::Session
            || actor.depth != scope.depth
            || scope.role != ActorRole::Session
        {
            return Err(StageError::Forbidden("Invalid session Core origin"));
        }
        if !scope.message.is_empty() {
            let input = system.store().message(&scope.message).map_err(report)?;
            if input.actor_id != actor.id
                || input.status != crate::actors::MessageStatus::Running
                || input.cancel_requested
            {
                return Err(StageError::Forbidden("Session request is no longer active"));
            }
        }
        return core(scope);
    }
    if scope.actor.stage_id == local {
        let actor = same_session(system, scope, &scope.actor.actor_id)?;
        if actor.role != scope.role || actor.depth != scope.depth {
            return Err(StageError::Forbidden("Session member identity changed"));
        }
        if !scope.message.is_empty() {
            let input = system.store().message(&scope.message).map_err(report)?;
            if input.actor_id != actor.id
                || input.status != crate::actors::MessageStatus::Running
                || input.cancel_requested
            {
                return Err(StageError::Forbidden("Session request is no longer active"));
            }
        }
        return Ok(());
    }
    let present=system.store().stage_transaction(|tx|Ok(tx.query_row("SELECT EXISTS(SELECT 1 FROM session_members WHERE session=?1 AND actor=?2 AND stage=?3)",params![scope.session.actor_id,scope.actor.actor_id,scope.actor.stage_id.to_string()],|r|r.get::<_,bool>(0))?))?;
    if !present {
        return Err(StageError::Forbidden(
            "Actor is not a member of this session",
        ));
    }
    Ok(())
}
pub(crate) async fn dispatch(
    system: ActorSystem,
    peer: StageId,
    request: Request,
) -> Result<Value> {
    let local = system.store().public_stage_id()?;
    if peer != local {
        let p = super::ledger::StageLedger::attach(system.store(), local)
            .peer(peer, chrono::Utc::now().timestamp_millis())?;
        if !p.authority.read
            || !p.authority.converse
            || !p.authority.delegate
            || p.authority.read_only
        {
            return Err(StageError::Forbidden(
                "Session operation requires writable account delegation authority",
            ));
        }
        let target_verify = matches!(&request.operation,Operation::Verify{reservation} if reservation.target==peer && reservation.scope.session.stage_id==local);
        let host_origin = request.scope.session.stage_id == peer;
        let validate = matches!(&request.operation, Operation::Validate)
            && request.scope.session.stage_id == local
            && (request.scope.actor.stage_id == local || request.scope.actor.stage_id == peer);
        if request.scope.actor.stage_id != peer
            && request.scope.session.stage_id != peer
            && !target_verify
            && !validate
            && !host_origin
        {
            return Err(StageError::Forbidden(
                "Session origin does not match authenticated peer",
            ));
        }
    }
    let scope = request.scope;
    if !matches!(
        &request.operation,
        Operation::Lifecycle { .. }
            | Operation::MemberFact { .. }
            | Operation::Verify { .. }
            | Operation::Create { .. }
    ) {
        if scope.session.stage_id == local {
            verify_member(&system, &scope)?;
        } else if !matches!(&request.operation, Operation::Validate) {
            Box::pin(call(
                &system,
                scope.session.stage_id,
                Request {
                    scope: scope.clone(),
                    operation: Operation::Validate,
                },
            ))
            .await?;
        } else {
            return Err(StageError::Forbidden("Not the session host"));
        }
    }
    match request.operation {
        Operation::Validate => {
            verify_member(&system, &scope)?;
            Ok(json!(true))
        }
        Operation::MemberFact { revision, actor } => {
            if peer != scope.actor.stage_id
                || scope.actor.actor_id != actor.id
                || scope.session.stage_id != local
                || actor.session_id != scope.session.actor_id
                || actor.role != ActorRole::Worker
            {
                return Err(StageError::Forbidden("Invalid session member fact"));
            }
            system
                .store()
                .receive_session_member_fact(peer, &scope.session.actor_id, revision, &actor)
                .map_err(report)?;
            Ok(json!(true))
        }
        Operation::Reserve {
            target,
            name,
            key,
            digest,
        } => {
            verify_member(&system, &scope)?;
            let name = name.trim().to_owned();
            if name.is_empty() || key.is_empty() {
                return Err(StageError::Invalid("Missing worker name or request key"));
            }
            system.store().stage_transaction(|tx| {
                let previous:Option<String>=tx.query_row("SELECT record FROM session_members WHERE session=?1 AND request=?2",params![scope.session.actor_id,key],|r|r.get(0)).optional()?;
                if let Some(previous)=previous {
                    let previous:Reservation=serde_json::from_str(&previous).map_err(|_|StageError::Invalid("Invalid creation receipt"))?;
                    if previous.digest!=digest || previous.scope.actor!=scope.actor || previous.target!=target {return Err(StageError::Conflict("Worker creation key changed input"));}
                    return Ok(json!(previous));
                }
                let taken:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM actors a LEFT JOIN actor_session_hosts h ON h.actor=a.id WHERE json_extract(a.record,'$.session_id')=?1 AND h.host IS NULL AND lower(trim(json_extract(a.record,'$.config.name')))=?2)",params![scope.session.actor_id,name.to_lowercase()],|r|r.get(0))?;
                if taken {return Err(StageError::Conflict("Actor name is already in use"));}
                let reservation=Reservation{id:uuid::Uuid::new_v4().to_string(),scope,target,name:name.clone(),key:key.clone(),digest};
                let record=serde_json::to_string(&reservation).map_err(|_|StageError::Invalid("Cannot store worker reservation"))?;
                let taken:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM session_members WHERE session=?1 AND name=?2)",params![reservation.scope.session.actor_id,name.to_lowercase()],|r|r.get(0))?;
                if taken{return Err(StageError::Conflict("Actor name is already in use"));}
                tx.execute("INSERT INTO session_members VALUES(?1,?2,?3,?4,?5,?6)",params![reservation.scope.session.actor_id,reservation.id,target.to_string(),name.to_lowercase(),key,record])?;
                Ok(json!(reservation))
            })
        }
        Operation::Verify { reservation } => {
            verify_member(&system, &reservation.scope)?;
            if peer != reservation.target {
                return Err(StageError::Forbidden(
                    "Creation is bound to another target device",
                ));
            }
            let stored: Option<String> = system.store().stage_transaction(|tx| {
                Ok(tx
                    .query_row(
                        "SELECT record FROM session_members WHERE session=?1 AND actor=?2",
                        params![reservation.scope.session.actor_id, reservation.id],
                        |r| r.get(0),
                    )
                    .optional()?)
            })?;
            if stored.as_deref()
                != Some(
                    serde_json::to_string(&reservation)
                        .map_err(|_| StageError::Invalid("Invalid reservation"))?
                        .as_str(),
                )
            {
                return Err(StageError::Forbidden(
                    "Worker creation has no matching session reservation",
                ));
            }
            Ok(json!(true))
        }
        Operation::Create {
            reservation,
            config,
            roots,
        } => {
            if reservation.target != local
                || reservation.scope.actor != scope.actor
                || reservation.scope.session != scope.session
            {
                return Err(StageError::Forbidden("Invalid worker creation binding"));
            }
            if super::digest(
                &serde_json::to_vec(&(&config, &roots))
                    .map_err(|_| StageError::Invalid("Invalid creation input"))?,
            ) != reservation.digest
            {
                return Err(StageError::Conflict(
                    "Worker creation changed after reservation",
                ));
            }
            let mut verifier = scope.clone();
            verifier.actor = ActorAddress {
                stage_id: local,
                actor_id: reservation.id.clone(),
            };
            Box::pin(call(
                &system,
                scope.session.stage_id,
                Request {
                    scope: verifier,
                    operation: Operation::Verify {
                        reservation: reservation.clone(),
                    },
                },
            ))
            .await?;
            let actor = system
                .create_remote_worker(&reservation, config, roots)
                .await
                .map_err(report)?;
            Ok(
                json!({"id":actor.id,"name":actor.config.name,"conversation_url":ActorAddress{stage_id:local,actor_id:actor.id}.conversation_url()}),
            )
        }
        Operation::List { offset, limit } => {
            if limit == 0 || limit > 100 {
                return Err(StageError::Invalid("Invalid Actor page limit"));
            }
            let mut all = system.store().actors().map_err(report)?;
            all.sort_by(|a, b| a.id.cmp(&b.id));
            let actors = all
                .into_iter()
                .filter(|actor| {
                    actor_session(&system, &actor.id).ok().as_ref() == Some(&scope.session)
                })
                .skip(offset)
                .take(limit + 1)
                .map(|actor| {
                    let mut summary = actor.summary();
                    summary.conversation_url = ActorAddress {
                        stage_id: local,
                        actor_id: actor.id,
                    }
                    .conversation_url();
                    summary
                })
                .collect::<Vec<_>>();
            let next = (actors.len() > limit).then_some(offset.saturating_add(limit));
            Ok(
                json!({"actors":actors.into_iter().take(limit).collect::<Vec<_>>(),"next_cursor":next}),
            )
        }
        Operation::Conversation {
            actor,
            cursor,
            limit,
            budget,
        } => {
            same_session(&system, &scope, &actor)?;
            let page = system
                .store()
                .conversation_bubbles(&actor, cursor.as_deref(), limit, budget.min(65536))
                .map_err(report)?;
            Ok(json!(page))
        }
        Operation::Admin {
            actor,
            action,
            model,
        } => {
            core(&scope)?;
            let target = same_session(&system, &scope, &actor)?;
            if target.role != ActorRole::Worker {
                return Err(StageError::Forbidden("Core cannot administer itself"));
            }
            match action.as_str() {
                "stop" => Ok(
                    json!({"actor_id":actor,"cancelled_count":system.stop_remote_worker(&actor).await.map_err(report)?}),
                ),
                "archive" | "unarchive" => {
                    if !scope.owner_requested {
                        return Err(StageError::Forbidden(
                            "Worker archival requires an explicit Owner request",
                        ));
                    }
                    Ok(json!(
                        system
                            .store()
                            .set_archived(&actor, action == "archive")
                            .map_err(report)?
                    ))
                }
                "set_model" => Ok(json!(
                    system
                        .apply_owned_preferences(&actor, Some(model))
                        .await
                        .map_err(report)?
                )),
                _ => Err(StageError::Invalid(
                    "Unknown worker administration operation",
                )),
            }
        }
        Operation::Lifecycle {
            session,
            revision,
            state,
        } => {
            if peer != session.stage_id
                || !["active", "archived", "deleted"].contains(&state.as_str())
            {
                return Err(StageError::Forbidden("Invalid session lifecycle source"));
            }
            system
                .apply_remote_session_lifecycle(&session, revision, &state)
                .await
                .map_err(report)?;
            Ok(json!(true))
        }
        Operation::AppealRead { id } => {
            core(&scope)?;
            let record = system.core_approval_appeal(&id).map_err(report)?;
            same_session(
                &system,
                &scope,
                &record.source_actor(system.store()).map_err(report)?,
            )?;
            Ok(json!(record))
        }
        Operation::AppealApply { authorization } => {
            core(&scope)?;
            let record = system
                .core_approval_appeal(&authorization.approval_id)
                .map_err(report)?;
            same_session(
                &system,
                &scope,
                &record.source_actor(system.store()).map_err(report)?,
            )?;
            Ok(json!(
                system
                    .apply_appeal_authorization(&authorization)
                    .await
                    .map_err(report)?
            ))
        }
        Operation::AppealNotice { id, content } => {
            verify_member(&system, &scope)?;
            system
                .store()
                .receive_session_appeal(&scope, &id, &content)
                .map_err(report)?;
            system.wake(&scope.session.actor_id);
            Ok(json!(true))
        }
        Operation::Skills { mut call } => {
            core(&scope)?;
            let context = system.owner_app_context().map_err(report)?;
            call.arguments["stage_id"] = Value::Null;
            let page = crate::super_skills_app::SuperSkillsApp::new(system)
                .execute_device(&call, &context)
                .await
                .map_err(report)?;
            Ok(json!(page))
        }
    }
}

pub(crate) async fn flush(system: &ActorSystem) -> Result<()> {
    let local = system.store().public_stage_id()?;
    let facts = system.store().stage_transaction(|tx| {
        let mut q = tx.prepare(
            "SELECT actor,peer,revision,record FROM session_fact_outbox ORDER BY actor LIMIT 32",
        )?;
        Ok(q.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?)
    })?;
    for (id, peer, revision, record) in facts {
        let peer = serde_json::from_value(json!(peer))
            .map_err(|_| StageError::Invalid("Invalid fact recipient"))?;
        let request = serde_json::from_str(&record)
            .map_err(|_| StageError::Invalid("Invalid member fact"))?;
        if call(system, peer, request).await.is_ok() {
            system.store().stage_transaction(|tx| {
                tx.execute(
                    "DELETE FROM session_fact_outbox WHERE actor=?1 AND revision=?2",
                    params![id, revision],
                )?;
                Ok(())
            })?;
        }
    }
    let pending=system.store().stage_transaction(|tx| {
        let mut q=tx.prepare("SELECT peer,session,revision,state FROM session_lifecycle_outbox ORDER BY peer,session LIMIT 32")?;
        Ok(q.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?)))?.collect::<std::result::Result<Vec<_>,_>>()?)
    })?;
    for (peer, id, revision, state) in pending {
        let peer: StageId = serde_json::from_value(json!(peer))
            .map_err(|_| StageError::Invalid("Invalid lifecycle recipient"))?;
        let session = ActorAddress {
            stage_id: local,
            actor_id: id.clone(),
        };
        let scope = Scope {
            actor: session.clone(),
            session: session.clone(),
            role: ActorRole::Session,
            depth: 0,
            message: String::new(),
            owner_requested: false,
        };
        if call(
            system,
            peer,
            Request {
                scope,
                operation: Operation::Lifecycle {
                    session,
                    revision,
                    state,
                },
            },
        )
        .await
        .is_ok()
        {
            system.store().stage_transaction(|tx|{tx.execute("DELETE FROM session_lifecycle_outbox WHERE peer=?1 AND session=?2 AND revision=?3",params![peer.to_string(),id,revision])?;Ok(())})?;
        }
    }
    let appeals = system.store().stage_transaction(|tx| {
        let mut q =
            tx.prepare("SELECT id,peer,record FROM session_appeal_outbox ORDER BY id LIMIT 32")?;
        Ok(q.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?)
    })?;
    for (id, peer, record) in appeals {
        let peer = serde_json::from_value(json!(peer))
            .map_err(|_| StageError::Invalid("Invalid appeal recipient"))?;
        let request = serde_json::from_str(&record)
            .map_err(|_| StageError::Invalid("Invalid pending appeal"))?;
        if call(system, peer, request).await.is_ok() {
            system.store().stage_transaction(|tx| {
                tx.execute("DELETE FROM session_appeal_outbox WHERE id=?1", [id])?;
                Ok(())
            })?;
        }
    }
    Ok(())
}

pub(crate) fn record_fact(tx: &Transaction<'_>, id: &str) -> Result<()> {
    let row:Option<(String,String)>=tx.query_row("SELECT a.record,h.host FROM actors a JOIN actor_session_hosts h ON h.actor=a.id WHERE a.id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((record, host)) = row else {
        return Ok(());
    };
    let actor: ActorRecord =
        serde_json::from_str(&record).map_err(|_| StageError::Invalid("Invalid member record"))?;
    let local: String = tx.query_row(
        "SELECT value FROM stage_public_meta WHERE key='stage'",
        [],
        |r| r.get(0),
    )?;
    let local: StageId = serde_json::from_value(json!(local))
        .map_err(|_| StageError::Invalid("Invalid local identity"))?;
    let host: StageId = serde_json::from_value(json!(host))
        .map_err(|_| StageError::Invalid("Invalid host identity"))?;
    let mut summary = actor.summary();
    let running: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages WHERE actor_id=?1 AND status='\"running\"')",
        [id],
        |r| r.get(0),
    )?;
    if summary.status != ActorStatus::Archived {
        summary.status = if running {
            ActorStatus::Running
        } else {
            ActorStatus::Idle
        };
    }
    summary.conversation_url = ActorAddress {
        stage_id: local,
        actor_id: id.into(),
    }
    .conversation_url();
    tx.execute("INSERT INTO session_fact_heads VALUES(?1,1) ON CONFLICT(actor) DO UPDATE SET revision=revision+1",[id])?;
    let revision: i64 = tx.query_row(
        "SELECT revision FROM session_fact_heads WHERE actor=?1",
        [id],
        |r| r.get(0),
    )?;
    let request = Request {
        scope: Scope {
            actor: ActorAddress {
                stage_id: local,
                actor_id: id.into(),
            },
            session: ActorAddress {
                stage_id: host,
                actor_id: actor.session_id,
            },
            role: actor.role,
            depth: actor.depth,
            message: String::new(),
            owner_requested: false,
        },
        operation: Operation::MemberFact {
            revision,
            actor: summary,
        },
    };
    let record =
        serde_json::to_string(&request).map_err(|_| StageError::Invalid("Invalid member fact"))?;
    tx.execute("INSERT INTO session_fact_outbox VALUES(?1,?2,?3,?4) ON CONFLICT(actor) DO UPDATE SET revision=excluded.revision,record=excluded.record",params![id,host.to_string(),revision,record])?;
    Ok(())
}
