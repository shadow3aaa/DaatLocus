//! Each Actor owns one serial inbox and one set of App instances.
//! Routing and durable state are owned by the store; this loop only executes.

use std::{
    collections::HashSet,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use miette::{IntoDiagnostic, Result, miette};
use serde_json::json;
use tokio::sync::mpsc;

use super::{
    ActorConfig, ActorControl, ActorEvent, ActorRecord, ActorStatus, ActorSystem, MessageRecord,
    MessageStatus, model_context::ModelContext,
};
use crate::{
    app::{AppManager, AppToolExecutionContext},
    builtin_apps,
    context_budget::TokenEstimateBaseline,
    core::{
        AgentTurnRetryPolicy, ModelProgressEvent, ModelProgressSink, ModelProvider,
        ModelRequestOptions, should_retry_agent_turn_error,
    },
    model_protocol::{
        AgentContent, AgentContentPart, AgentMessage, AgentToolCall, AgentTurnRequest,
        AgentTurnResponse, AgentTurnStreamResult,
    },
};

const PROGRESS_INTERVAL: Duration = Duration::from_millis(40);
const CANCELLATION_INTERVAL: Duration = Duration::from_millis(25);
const PROGRESS_CHUNK_BYTES: usize = 4_096;

/// In-flight execution is never automatically retried after cancellation.
pub async fn run_actor(system: ActorSystem, actor_id: String, control: Arc<ActorControl>) -> bool {
    let mut apps: Option<AppManager> = None;
    let mut installed_config: Option<ActorConfig> = None;
    let mut passivated = false;
    let mut idle_deadline = tokio::time::Instant::now() + system.idle_timeout();
    loop {
        // Register before inspecting the inbox so a concurrent enqueue cannot
        // disappear between the empty query and the wait.
        let notified = control.wake.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if control.stop.load(Ordering::Acquire) {
            break;
        }
        if actor_id == crate::stage::shared::ACTOR_ID && !system.core_permitted() {
            passivated = true;
            break;
        }
        let message = match system.store().claim_next(&actor_id) {
            Ok(Some(message)) => message,
            Ok(None) => {
                if system
                    .store()
                    .set_status(&actor_id, ActorStatus::Idle)
                    .is_err()
                {
                    break;
                }
                if tokio::time::timeout_at(idle_deadline, notified)
                    .await
                    .is_err()
                {
                    if apps.as_ref().is_none_or(AppManager::can_passivate) {
                        passivated = true;
                        break;
                    }
                    idle_deadline = tokio::time::Instant::now() + system.idle_timeout();
                }
                continue;
            }
            Err(_) => {
                tracing::error!(actor_id, "Actor inbox could not be read");
                break;
            }
        };
        let result = async {
            let actor = system.execution_actor(&message)?;
            system.store().set_status(&actor_id, ActorStatus::Running)?;
            if actor_id == crate::stage::shared::ACTOR_ID
                || installed_config.as_ref() != Some(&actor.config)
            {
                shutdown_apps(&mut apps).await;
                apps = Some(
                    if actor_id == crate::stage::shared::ACTOR_ID && !system.local_core() {
                        system
                            .shared_core()
                            .ok_or_else(|| miette!("Shared Actor worker is unavailable"))?
                            .model_apps()
                            .await?
                    } else {
                        builtin_apps::create_apps(&actor, system.clone(), &system.config())?
                    },
                );
                installed_config = Some(actor.config.clone());
            }
            execute_message(
                &system,
                &actor,
                &message,
                &control,
                apps.as_mut().expect("Apps installed"),
            )
            .await
        }
        .await;

        // Check once more after the final provider/tool future resolves. A
        // cancellation racing the final chunk must not deliver success.
        if actor_id == crate::stage::shared::ACTOR_ID && !system.core_execution_permitted() {
            shutdown_apps(&mut apps).await;
            installed_config = None;
            continue;
        }
        let interrupted = interruption(&control, &message);
        let (status, reply, error, rebuild) = match (interrupted, result) {
            (Some(status), _) => (
                status,
                None,
                Some("Execution interrupted; completed effects were not rolled back".to_owned()),
                true,
            ),
            (_, Ok(reply)) => (MessageStatus::Completed, reply, None, false),
            (_, Err(error)) => (
                MessageStatus::Failed,
                None,
                Some(
                    error
                        .downcast_ref::<crate::providers::ModelRequestFailure>()
                        .map_or_else(
                            || system.safe_error(&error.to_string()),
                            |error| error.code().into(),
                        ),
                ),
                true,
            ),
        };
        match system
            .store()
            .finish_message(&message.id, status, reply.as_deref(), error.as_deref())
        {
            Ok(Some(result)) => system.wake(&result.actor_id),
            Ok(None) => {}
            Err(_) => {
                tracing::error!(actor_id, "Actor message completion could not be persisted");
                break;
            }
        }
        if actor_id == crate::stage::shared::ACTOR_ID
            && let Some(core) = system.shared_core()
        {
            let _ = core.checkpoint().await;
        }
        control.cancelled.lock().remove(&message.id);
        if rebuild {
            shutdown_apps(&mut apps).await;
            installed_config = None;
        }
        idle_deadline = tokio::time::Instant::now() + system.idle_timeout();
    }
    shutdown_apps(&mut apps).await;
    let _ = system.store().set_status(&actor_id, ActorStatus::Idle);
    passivated
}

async fn shutdown_apps(apps: &mut Option<AppManager>) {
    if let Some(apps) = apps.take()
        && apps.shutdown().await.is_err()
    {
        tracing::warn!("Actor Apps did not complete resource shutdown");
    }
}

fn interruption(control: &ActorControl, message: &MessageRecord) -> Option<MessageStatus> {
    if control.stop.load(Ordering::Acquire) {
        Some(MessageStatus::Interrupted)
    } else if message.cancel_requested || control.cancelled.lock().contains(&message.id) {
        Some(MessageStatus::Cancelled)
    } else {
        None
    }
}

fn check_interruption(
    system: &ActorSystem,
    control: &ActorControl,
    message: &MessageRecord,
) -> Result<()> {
    if interruption(control, message).is_some()
        || message.actor_id == crate::stage::shared::ACTOR_ID && !system.core_execution_permitted()
    {
        Err(miette!("Execution interrupted"))
    } else {
        Ok(())
    }
}

async fn refresh_apps(
    system: &ActorSystem,
    actor: &ActorRecord,
    message: &MessageRecord,
    control: &ActorControl,
    apps: &mut AppManager,
    context: &AppToolExecutionContext,
    refresh: bool,
) -> Result<()> {
    {
        let preparation = apps.prepare_managed(context, refresh);
        tokio::pin!(preparation);
        loop {
            tokio::select! {
                result = &mut preparation => { result?; break; },
                () = tokio::time::sleep(CANCELLATION_INTERVAL) => {
                    check_interruption(system, control, message)?;
                }
            }
        }
    }
    let registry = system.app_registry();
    let replacements = registry
        .entries()
        .into_iter()
        .filter(|entry| entry.enabled && actor.id != crate::stage::shared::ACTOR_ID)
        .map(|entry| {
            Box::new(crate::mcp::McpApp::new(
                entry,
                registry.clone(),
                system.clone(),
            )) as Box<dyn crate::app::App>
        })
        .collect();
    apps.reconcile_managed(replacements).await?;
    registry.record_loaded(&actor.id, apps.managed_versions());
    Ok(())
}

async fn execute_message(
    system: &ActorSystem,
    actor: &ActorRecord,
    message: &MessageRecord,
    control: &ActorControl,
    apps: &mut AppManager,
) -> Result<Option<String>> {
    check_interruption(system, control, message)?;
    let context = system.execution_context(actor, message)?;
    let provider = system.provider(actor)?;
    if actor.id != crate::stage::shared::ACTOR_ID {
        refresh_apps(system, actor, message, control, apps, &context, false).await?;
    }
    let tools = apps.model_tool_specs_for_context(&context)?;
    let resuming = system
        .store()
        .has_history_for_message(&actor.id, &message.id)?;
    if !resuming {
        let input = AgentMessage::user_content(system.message_content(actor, message, &context)?);
        system
            .store()
            .append_history(&actor.id, &message.id, &[input])?;
    }
    let mut window = ModelContext::load(system, actor, &context, tools).await?;
    let config = system.config();
    let model_name = config.actor_model_name(actor.config.model.as_deref());
    let request_timeout = Duration::from_secs(
        config
            .models
            .get(model_name)
            .map_or(300, |model| model.request_timeout_secs)
            .clamp(1, 1_200),
    );
    let mut generation = 0u32;
    let binding = {
        let model = config
            .models
            .get(model_name)
            .ok_or_else(|| miette!("Actor model is unavailable"))?;
        let mut provider =
            serde_json::to_value(config.providers.get(&model.provider)).into_diagnostic()?;
        if let Some(fields) = provider.as_object_mut() {
            for field in ["api_key", "github_token", "auth_file"] {
                fields.remove(field);
            }
        }
        crate::stage::digest(&serde_json::to_vec(&(model_name, model, provider)).into_diagnostic()?)
    };
    let mut call_ids = std::collections::HashSet::new();
    let mut after = 0;
    loop {
        let page = system
            .store()
            .history_page(&actor.id, after, Some(&message.id), 128)?;
        if page.is_empty() {
            break;
        }
        for (seq, _, row) in page {
            after = seq;
            if let AgentMessage::AssistantToolCallProtocol { calls, .. } = row {
                call_ids.extend(calls.into_iter().map(|call| call.id));
            }
        }
    }
    let mut overflow_recoveries = 0;
    let mut handoff_required = false;
    let mut empty_reasoning_responses = 0;
    loop {
        check_interruption(system, control, message)?;
        if actor.id != crate::stage::shared::ACTOR_ID
            && system.app_registry().take_refresh(&actor.id)
        {
            refresh_apps(system, actor, message, control, apps, &context, true).await?;
            let tools = apps.model_tool_specs_for_context(&context)?;
            window = ModelContext::load(system, actor, &context, tools).await?;
        }
        if system.store().turn_yielded(&actor.id, &message.id)? {
            // The successful tool observation is durable and all calls in its
            // protocol group have settled; no final text is sent or inferred.
            system.store().append_history(
                &actor.id,
                &message.id,
                &[AgentMessage::assistant("")],
            )?;
            return Ok(None);
        }
        if actor.id == crate::stage::shared::ACTOR_ID
            && system.store().core_turn_delivered(&message.id)?
        {
            // All observations of the previous group have settled. Persist the
            // turn boundary so takeover does not ask the model to reply again.
            system.store().append_history(
                &actor.id,
                &message.id,
                &[AgentMessage::assistant("")],
            )?;
            return Ok(Some(String::new()));
        }
        Box::pin(receive_incoming(system, actor, message, control, apps)).await?;
        window.refresh_instructions(system, actor, &context).await?;
        let baseline = ModelContext::baseline(system, &actor.id, &binding)?;
        window.reload(system, message, provider.as_ref(), &baseline)?;
        check_interruption(system, control, message)?;
        if actor.id == crate::stage::shared::ACTOR_ID {
            system.checkpoint_core().await?;
        }
        let request = if !handoff_required {
            window.admitted(provider.as_ref(), &baseline)
        } else {
            None
        };
        let Some(request) = request else {
            let request = window.handoff_request(message, provider.as_ref(), &baseline)?;
            let Some(response) = generate(
                system,
                actor,
                message,
                control,
                provider.as_ref(),
                request,
                request_timeout,
                &mut generation,
                &binding,
                &baseline,
                false,
            )
            .await?
            else {
                continue;
            };
            let response = response.protocol();
            if !response.tool_calls.is_empty() || response.raw_stream_follow_up {
                return Err(miette!("Context handoff did not return a complete note"));
            }
            let note = response
                .final_assistant_message
                .or(response.assistant_text)
                .filter(|text| !text.trim().is_empty())
                .ok_or_else(|| miette!("Context handoff note is empty"))?;
            check_interruption(system, control, message)?;
            if system.store().has_queued_input(&actor.id)? {
                continue;
            }
            let recovery = system
                .store()
                .reset_model_context(&actor.id, &message.id, &note)?;
            window.history = vec![recovery];
            if window.admitted(provider.as_ref(), &baseline).is_none() {
                return Err(miette!(
                    "System instructions, App definitions and handoff note exceed the model budget"
                ));
            }
            handoff_required = false;
            continue;
        };
        let response = match generate(
            system,
            actor,
            message,
            control,
            provider.as_ref(),
            request,
            request_timeout,
            &mut generation,
            &binding,
            &baseline,
            true,
        )
        .await
        {
            Ok(Some(response)) => {
                overflow_recoveries = 0;
                response
            }
            Ok(None) => continue,
            Err(error) if context_overflow(&error) && overflow_recoveries < 3 => {
                if !window.can_recover() {
                    return Err(error);
                }
                handoff_required = true;
                overflow_recoveries += 1;
                continue;
            }
            Err(error) => return Err(error),
        };
        if system.store().has_queued_input(&actor.id)? {
            continue;
        }
        let response = response.protocol();
        check_interruption(system, control, message)?;
        if response.tool_calls.is_empty() {
            let reply = response
                .final_assistant_message
                .or(response.assistant_text)
                .filter(|text| !text.trim().is_empty());
            let reasoning_only = reply.is_none()
                && response
                    .reasoning_content
                    .as_ref()
                    .is_some_and(|text| !text.trim().is_empty());
            if response.raw_stream_follow_up || reasoning_only {
                empty_reasoning_responses = if reply.is_none() {
                    empty_reasoning_responses + 1
                } else {
                    0
                };
                if empty_reasoning_responses >= 3 {
                    return Err(miette!(
                        "Model repeatedly returned no final answer or App calls"
                    ));
                }
                let row = if response.reasoning_signature.is_some() {
                    AgentMessage::assistant_tool_call_protocol_with_signed_reasoning(
                        reply,
                        response.reasoning_content,
                        response.reasoning_signature,
                        vec![],
                    )
                    .with_responses_output(response.responses_output)
                } else {
                    AgentMessage::assistant(reply.unwrap_or_default())
                        .with_responses_output(response.responses_output)
                };
                let guidance = AgentMessage::user("Continue the current request to completion.");
                system.store().append_history(
                    &actor.id,
                    &message.id,
                    &[row.clone(), guidance.clone()],
                )?;
                window.history.extend([row, guidance]);
                continue;
            }
            let reply =
                reply.ok_or_else(|| miette!("Model returned no final answer or App calls"))?;
            let final_message = AgentMessage::assistant(reply.clone())
                .with_responses_output(response.responses_output);
            system
                .store()
                .append_history(&actor.id, &message.id, &[final_message])?;
            return Ok(Some(reply));
        }
        empty_reasoning_responses = 0;
        validate_response_calls(&response)?;
        if response
            .tool_calls
            .iter()
            .any(|call| !call_ids.insert(call.id.clone()))
        {
            return Err(miette!(
                "Model reused an App call identifier in this request"
            ));
        }
        let protocol = AgentMessage::assistant_tool_call_protocol_with_signed_reasoning(
            response.assistant_text,
            response.reasoning_content,
            response.reasoning_signature,
            response.tool_calls.clone(),
        )
        .with_responses_output(response.responses_output);
        system
            .store()
            .append_history(&actor.id, &message.id, std::slice::from_ref(&protocol))?;
        window.history.push(protocol);
        let mut images = Vec::new();
        for call in &response.tool_calls {
            match execute_call(
                system,
                actor,
                message,
                control,
                apps,
                call,
                &mut window.history,
            )
            .await
            {
                Ok(parts) => images.extend(parts),
                Err(error) => {
                    // Every advertised call receives an observation, including the
                    // calls not reached after interruption or a persistence error.
                    let done: HashSet<&str> = window
                        .history
                        .iter()
                        .rev()
                        .take_while(|row| {
                            !matches!(row, AgentMessage::AssistantToolCallProtocol { .. })
                        })
                        .filter_map(|row| match row {
                            AgentMessage::Tool { tool_call_id, .. } => Some(tool_call_id.as_str()),
                            _ => None,
                        })
                        .collect();
                    let pending: Vec<_> = response
                        .tool_calls
                        .iter()
                        .filter(|call| !done.contains(call.id.as_str()))
                        .cloned()
                        .collect();
                    for skipped in pending {
                        record_tool_error(
                            system,
                            actor,
                            message,
                            &skipped,
                            "Call was not executed because this request ended before the call could start",
                            &mut window.history,
                        )?;
                    }
                    return Err(error);
                }
            }
        }
        // All tool observations precede image input so providers never see a
        // user message in the middle of an outstanding multi-call response.
        if !images.is_empty() {
            let images = AgentMessage::user_content(AgentContent::multimodal(
                "Images from the preceding observations",
                images,
            ));
            system
                .store()
                .append_history(&actor.id, &message.id, std::slice::from_ref(&images))?;
            window.history.push(images);
        }
    }
}

fn validate_response_calls(response: &AgentTurnResponse) -> Result<()> {
    let mut ids = HashSet::new();
    for call in &response.tool_calls {
        if call.id.trim().is_empty() || !ids.insert(&call.id) {
            return Err(miette!(
                "Model returned missing or duplicate App call identifiers"
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn execute_sudo_call(
    system: &ActorSystem,
    actor: &ActorRecord,
    message: &MessageRecord,
    control: &ActorControl,
    apps: &mut AppManager,
    call: &AgentToolCall,
    context: &AppToolExecutionContext,
) -> Option<Result<crate::app::AppToolExecutionResult>> {
    let request = (|| {
        let (args, mut invocation) =
            crate::sudo_app::SudoApp::request(call, context.execution_cwd.clone())?;
        let target = invocation.call(&call.id);
        let unrestricted = super::sudo::unrestricted_context(context)?;
        apps.validate_tool_call(&target, &unrestricted)?;
        let owner = target.name.split_once("__").unwrap().0;
        if let Some(revision) = apps.managed_versions().get(owner) {
            let entry = system.app_registry().get(owner)?;
            if entry.revision != *revision || !entry.enabled {
                return Err(miette!("Target App changed before Sudo review"));
            }
            invocation.app_revision = Some(*revision);
            invocation.app_source = Some(entry.source.public());
        }
        Ok::<_, miette::Report>((args, invocation))
    })();
    let (args, invocation) = match request {
        Ok(value) => value,
        Err(error) => return Some(Err(error)),
    };
    let reviewed = {
        let future = super::sudo::review(
            system, actor, message, control, context, call, args, invocation,
        );
        tokio::pin!(future);
        loop {
            tokio::select! {
                result = &mut future => break result,
                () = tokio::time::sleep(CANCELLATION_INTERVAL) => {
                    if check_interruption(system, control, message).is_err() { return None; }
                    if let Err(error) = Box::pin(receive_incoming(system, actor, message, control, apps)).await {
                        return Some(Err(error));
                    }
                }
            }
        }
    };
    let (approval_id, invocation) = match reviewed {
        Ok(super::sudo::ReviewOutcome::Execute {
            approval_id,
            invocation,
        }) => (approval_id, invocation),
        Ok(super::sudo::ReviewOutcome::Submitted(result)) => return Some(Ok(result)),
        Err(error) => return Some(Err(error)),
    };
    let prepared = (|| {
        let current = system.execution_actor(message)?;
        let context = system.execution_context(&current, message)?;
        super::approval::ensure_current(system, actor, message, control)?;
        let mut unrestricted = super::sudo::unrestricted_context(&context)?;
        unrestricted.restart_native = invocation.restart_native;
        let target = invocation.call(&call.id);
        apps.validate_tool_call(&target, &unrestricted)?;
        let owner = target.name.split_once("__").unwrap().0;
        if apps.managed_versions().get(owner).copied() != invocation.app_revision {
            return Err(miette!("Target App changed while Sudo review was pending"));
        }
        if let Some(revision) = invocation.app_revision {
            system.app_registry().check(owner, revision)?;
        }
        system
            .store()
            .claim_sudo(&approval_id, &message.id, &call.id, &invocation)?;
        Ok::<_, miette::Report>((target, unrestricted))
    })();
    let (target, mut unrestricted) = match prepared {
        Ok(value) => value,
        Err(error) => return Some(Err(error)),
    };
    if invocation.administrator {
        let elevation = system.sudo_administrator();
        tokio::pin!(elevation);
        loop {
            tokio::select! {
                result=&mut elevation=>match result {
                    Ok(client)=>{unrestricted.sandbox_policy.administrator=Some(client);break;},
                    Err(error)=>return Some(Err(error)),
                },
                ()=tokio::time::sleep(CANCELLATION_INTERVAL)=>{
                    if check_interruption(system, control, message).is_err(){return None;}
                }
            }
        }
    }
    if let Err(error) = super::approval::ensure_current(system, actor, message, control) {
        return Some(Err(error));
    }
    let fresh = match system
        .execution_actor(message)
        .and_then(|actor| system.execution_context(&actor, message))
    {
        Ok(context) => context,
        Err(error) => return Some(Err(error)),
    };
    if let Err(error) = super::sudo::unrestricted_context(&fresh) {
        return Some(Err(error));
    }
    match crate::actors::permissions::canonical_path(&invocation.cwd) {
        Ok(path) if path == invocation.cwd && fresh.execution_cwd == invocation.cwd => {}
        _ => {
            return Some(Err(miette!(
                "Sudo working directory binding changed before execution"
            )));
        }
    }
    unrestricted.actor_call = fresh.actor_call;
    unrestricted.turn_epoch = fresh.turn_epoch;
    let future = apps.execute_tool(&target, &unrestricted);
    tokio::pin!(future);
    loop {
        tokio::select! {
            result = &mut future => return Some(result.map(|mut result| {
                if let Some(payload)=result.payload.as_object_mut() {
                    payload.insert("sudo".into(),serde_json::json!({"tool":target.name,
                        "administrator":invocation.administrator,"approval_id":approval_id}));
                }
                result
            })),
            () = tokio::time::sleep(CANCELLATION_INTERVAL) => {
                if check_interruption(system, control, message).is_err() { return None; }
            }
        }
    }
}

async fn execute_call(
    system: &ActorSystem,
    actor: &ActorRecord,
    message: &MessageRecord,
    control: &ActorControl,
    apps: &mut AppManager,
    call: &AgentToolCall,
    history: &mut Vec<AgentMessage>,
) -> Result<Vec<AgentContentPart>> {
    check_interruption(system, control, message)?;
    let current_actor = system.execution_actor(message)?;
    if current_actor.config != actor.config {
        return Err(miette!("Actor configuration changed during execution"));
    }
    // Do not reuse the grants captured before a long model request. A freshly
    // constructed execution context checks current permission and message
    // ownership immediately before effects.
    let mut context = system.execution_context(&current_actor, message)?;
    check_interruption(system, control, message)?;
    if let Err(error) = apps.validate_tool_call(call, &context) {
        record_tool_error(
            system,
            actor,
            message,
            call,
            &system.safe_error(&error.to_string()),
            history,
        )?;
        return Ok(Vec::new());
    }
    check_interruption(system, control, message)?;
    // Persist the intent before executing effects. Recovery exposes the
    // interrupted call, and does not replay it as an apparently fresh request.
    let result_id = system.store().tool_started_with_authority(
        &actor.id,
        &message.id,
        call,
        current_actor.remote_read_only,
        true,
    )?;
    if actor.id == crate::stage::shared::ACTOR_ID {
        system.checkpoint_core().await?;
    }
    super::runtime_permissions::bind_access_broker(
        &mut context,
        system,
        &current_actor,
        message,
        call,
        control,
    );
    let outcome = if call.name.starts_with("sudo__") {
        execute_sudo_call(system, actor, message, control, apps, call, &context).await
    } else if call.name == "actors__appeal_approval" {
        // Actors is a stateless facade. Keep its review-control future while
        // retaining this Actor's existing stateful Apps for incoming input.
        use crate::app::App;
        let mut facade = crate::actor_app::ActorApp::new(actor.id.clone(), system.clone());
        let control_call = call.with_name("appeal_approval");
        let future = facade.execute_tool(&control_call, &context);
        tokio::pin!(future);
        loop {
            tokio::select! {
                result = &mut future => break Some(result),
                () = tokio::time::sleep(CANCELLATION_INTERVAL) => {
                    if check_interruption(system, control, message).is_err() { break None; }
                    Box::pin(receive_incoming(system, actor, message, control, apps)).await?;
                }
            }
        }
    } else {
        let future = apps.execute_tool(call, &context);
        tokio::pin!(future);
        loop {
            tokio::select! {
                result = &mut future => break Some(result),
                () = tokio::time::sleep(CANCELLATION_INTERVAL) => {
                    if interruption(control,message).is_some() || actor.id==crate::stage::shared::ACTOR_ID && !system.core_execution_permitted(){break None;}
                }
            }
        }
    };
    match outcome {
        None => {
            system
                .store()
                .discard_tool_result_reference(&actor.id, &message.id, &call.id)?;
            record_tool_error(
                system,
                actor,
                message,
                call,
                "Call interrupted during execution; effects may already have occurred. Do not automatically repeat it",
                history,
            )?;
            Err(miette!("Execution interrupted"))
        }
        Some(Err(error)) => {
            if let Some(rejection) = super::approval::rejection(&error) {
                record_tool_rejection(system, actor, message, call, &rejection, history)?;
                check_interruption(system, control, message)?;
                return Ok(Vec::new());
            }
            let mut result = super::runtime_permissions::error_result(system, &error);
            super::runtime_permissions::attach_reference(
                system,
                actor,
                message,
                call,
                &context,
                &result_id,
                &mut result,
                Some(&error),
            )?;
            finish_tool_result(system, actor, message, call, &context, result, history)?;
            check_interruption(system, control, message)?;
            Ok(Vec::new())
        }
        Some(Ok(mut result)) => {
            super::runtime_permissions::attach_reference(
                system,
                actor,
                message,
                call,
                &context,
                &result_id,
                &mut result,
                None,
            )?;
            let images =
                finish_tool_result(system, actor, message, call, &context, result, history)?;
            check_interruption(system, control, message)?;
            Ok(images)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn finish_tool_result(
    system: &ActorSystem,
    actor: &ActorRecord,
    message: &MessageRecord,
    call: &AgentToolCall,
    context: &AppToolExecutionContext,
    mut result: crate::app::AppToolExecutionResult,
    history: &mut Vec<AgentMessage>,
) -> Result<Vec<crate::model_protocol::AgentContentPart>> {
    let images = crate::resources::capture_images(system, &actor.id, result.model_images)?;
    let references =
        crate::resources::bind_image_resources(system, &actor.id, &message.id, &images)?;
    let custom_content = result.model_content.is_some();
    let embedded_references = if !references.is_empty()
        && let Some(payload) = result.payload.as_object_mut()
    {
        payload.insert("resource_references".into(), json!(&references));
        true
    } else {
        false
    };
    if let Some(report) = &result.sandbox
        && let Some(payload) = result.payload.as_object_mut()
    {
        payload.insert("sandbox".into(), json!(report));
    }
    let mut full_text = result
        .model_content
        .unwrap_or_else(|| result.payload.to_string());
    if !references.is_empty() && (custom_content || !embedded_references) {
        full_text.push_str(&format!("\n{}", json!({"resource_references":references})));
    }
    let mut model_content = crate::resources::bound_output(
        system,
        &actor.id,
        &call.id,
        &full_text,
        context.tool_output_max_tokens,
        result.overflow_continuation,
    )?;
    if let Some(reference) = &result.permission_reference {
        reference.append_to(&mut model_content);
    }
    super::runtime_permissions::append_denial_summary(result.sandbox.as_ref(), &mut model_content);
    system.store().tool_finished_with_reference(
        &actor.id,
        &message.id,
        call,
        result.payload,
        &model_content,
        &result.summary,
        result.failure.as_deref(),
        result
            .permission_reference
            .as_ref()
            .map(|reference| reference.tool_result_id.as_str()),
    )?;
    let content = if result.failure.is_some() {
        format!("App error: {model_content}")
    } else {
        model_content
    };
    let tool = AgentMessage::tool(&call.id, &call.name, content);
    history.push(tool);
    Ok(images)
}

fn record_tool_rejection(
    system: &ActorSystem,
    actor: &ActorRecord,
    message: &MessageRecord,
    call: &AgentToolCall,
    rejection: &super::approval::ApprovalRejection,
    history: &mut Vec<AgentMessage>,
) -> Result<()> {
    let content = rejection.model_content();
    system
        .store()
        .discard_tool_result_reference(&actor.id, &message.id, &call.id)?;
    system.store().tool_finished(
        &actor.id,
        &message.id,
        call,
        rejection.payload(),
        &content,
        "App operation was not approved",
        None,
    )?;
    history.push(AgentMessage::tool(&call.id, &call.name, content));
    Ok(())
}

fn record_tool_error(
    system: &ActorSystem,
    actor: &ActorRecord,
    message: &MessageRecord,
    call: &AgentToolCall,
    error: &str,
    history: &mut Vec<AgentMessage>,
) -> Result<()> {
    system.store().tool_finished(
        &actor.id,
        &message.id,
        call,
        json!({"error": error}),
        error,
        "App call failed",
        Some(error),
    )?;
    let tool = AgentMessage::tool(&call.id, &call.name, format!("App error: {error}"));
    history.push(tool);
    Ok(())
}

fn context_overflow(error: &miette::Report) -> bool {
    crate::context_budget::is_context_budget_exceeded(error)
}

/// New messages preempt inference at settled boundaries. A nested input owns
/// its execution authority, replies and lifetime; the interrupted request stays
/// open and resumes from history once the incoming input releases the worker.
async fn receive_incoming(
    system: &ActorSystem,
    actor: &ActorRecord,
    current: &MessageRecord,
    control: &ActorControl,
    apps: &mut AppManager,
) -> Result<bool> {
    let mut received = false;
    while let Some(input) = system.store().claim_incoming(&actor.id, &current.id)? {
        received = true;
        let result = async {
            let incoming_actor = system.execution_actor(&input)?;
            Box::pin(execute_message(
                system,
                &incoming_actor,
                &input,
                control,
                apps,
            ))
            .await
        }
        .await;
        if actor.id == crate::stage::shared::ACTOR_ID && !system.core_execution_permitted() {
            return Err(miette!("Core worker no longer owns execution"));
        }
        let (status, reply, error) = match (interruption(control, &input), result) {
            (Some(status), _) => (
                status,
                None,
                Some("Execution interrupted; completed effects were not rolled back".to_owned()),
            ),
            (_, Ok(reply)) => (MessageStatus::Completed, reply, None),
            (_, Err(error)) => (
                MessageStatus::Failed,
                None,
                Some(
                    error
                        .downcast_ref::<crate::providers::ModelRequestFailure>()
                        .map_or_else(
                            || system.safe_error(&error.to_string()),
                            |error| error.code().into(),
                        ),
                ),
            ),
        };
        if let Some(reply) =
            system
                .store()
                .finish_message(&input.id, status, reply.as_deref(), error.as_deref())?
        {
            system.wake(&reply.actor_id);
        }
        control.cancelled.lock().remove(&input.id);
        if status != MessageStatus::Completed {
            let replacements = if actor.id == crate::stage::shared::ACTOR_ID && !system.local_core()
            {
                system
                    .shared_core()
                    .ok_or_else(|| miette!("Shared Actor worker is unavailable"))?
                    .model_apps()
                    .await?
            } else {
                builtin_apps::create_apps(actor, system.clone(), &system.config())?
            };
            let previous = std::mem::replace(apps, replacements);
            previous.shutdown().await?;
            let context = system.execution_context(actor, current)?;
            refresh_apps(system, actor, current, control, apps, &context, false).await?;
        }
        if actor.id == crate::stage::shared::ACTOR_ID {
            system.checkpoint_core().await?;
        }
        check_interruption(system, control, current)?;
    }
    if received {
        system.store().resume_input(&actor.id, &current.id)?;
    }
    Ok(received)
}

// Keep Actor and request authority explicit at the model execution boundary.
#[allow(clippy::too_many_arguments)]
async fn generate(
    system: &ActorSystem,
    actor: &ActorRecord,
    message: &MessageRecord,
    control: &ActorControl,
    provider: &dyn ModelProvider,
    request: AgentTurnRequest,
    timeout: Duration,
    generation: &mut u32,
    binding: &str,
    baseline: &TokenEstimateBaseline,
    publish_progress: bool,
) -> Result<Option<AgentTurnStreamResult>> {
    let policy = AgentTurnRetryPolicy {
        base_request_timeout: timeout,
        max_request_timeout: timeout.max(Duration::from_secs(1200)),
        ..AgentTurnRetryPolicy::default()
    };
    for attempt in 1..=policy.max_attempts {
        check_interruption(system, control, message)?;
        if system.store().has_queued_input(&actor.id)? {
            return Ok(None);
        }
        if publish_progress {
            *generation = generation.saturating_add(1);
            system.store().append_event(
                &actor.id,
                ActorEvent::GenerationStarted {
                    message_id: message.id.clone(),
                    generation: *generation,
                },
            )?;
        }
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut options =
            ModelRequestOptions::for_agent_turn(provider, &request, Some(actor.id.clone()))?;
        let estimated_input = options.budget.total_input_tokens;
        options.budget = options
            .budget
            .with_conservative_calibrated_input_tokens(baseline);
        if !options.budget.within_context_window() {
            return Err(
                crate::context_budget::ContextBudgetExceededError::for_request(
                    "agent turn",
                    &provider.model_name(),
                    &options.budget,
                    None,
                )
                .into(),
            );
        }
        let options = options.with_progress(publish_progress.then_some(ModelProgressSink::new(tx)));
        crate::logging::write_current_turn_messages_dump(
            system,
            &actor.id,
            &request,
            &options.budget,
            Some(&provider.model_name()),
        )
        .await?;
        check_interruption(system, control, message)?;
        let usage_before = provider.token_usage_info().total_token_usage;
        let future = tokio::time::timeout(
            policy.request_timeout_for_attempt(attempt),
            provider.complete_agent_turn(
                crate::resources::model_request(system, &actor.id, request.clone())
                    .map_err(|error| miette!("{error}"))?,
                options,
            ),
        );
        tokio::pin!(future);
        let mut pending = ProgressBuffer::default();
        let mut progress_open = true;
        let mut timer = tokio::time::interval(PROGRESS_INTERVAL);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let result = loop {
            tokio::select! {
                response = &mut future => break match response {
                    Ok(response) => response,
                    Err(_) => Err(miette!("Model request timed out")),
                },
                progress = rx.recv(), if progress_open => match progress {
                    Some(progress) => handle_progress(system, actor, message, generation, &mut pending, progress)?,
                    None => progress_open = false,
                },
                _ = timer.tick() => {
                    pending.flush(system, actor, message, *generation)?;
                    check_interruption(system,control, message)?;
                    if system.store().has_queued_input(&actor.id)? {
                        let usage = provider.token_usage_info();
                        system.store().record_model_usage(&actor.id, usage.total_token_usage.delta_from(&usage_before), usage.model_context_window)?;
                        return Ok(None);
                    }
                },
                () = tokio::time::sleep(CANCELLATION_INTERVAL) => {
                    check_interruption(system,control, message)?;
                    if system.store().has_queued_input(&actor.id)? {
                        let usage = provider.token_usage_info();
                        system.store().record_model_usage(&actor.id, usage.total_token_usage.delta_from(&usage_before), usage.model_context_window)?;
                        return Ok(None);
                    }
                },
            }
        };
        while let Ok(progress) = rx.try_recv() {
            handle_progress(system, actor, message, generation, &mut pending, progress)?;
        }
        pending.flush(system, actor, message, *generation)?;
        let usage = provider.token_usage_info();
        let delta = usage.total_token_usage.delta_from(&usage_before);
        system
            .store()
            .record_model_usage(&actor.id, delta.clone(), usage.model_context_window)?;
        check_interruption(system, control, message)?;
        match result {
            Ok(response) => {
                crate::logging::write_current_turn_response_dump(
                    system, &actor.id, &response, attempt,
                )
                .await?;
                if delta.input_tokens > 0 && usage.last_token_usage.input_tokens > 0 {
                    system.store().update_token_estimate(
                        &actor.id,
                        binding,
                        TokenEstimateBaseline {
                            estimated_input_tokens: estimated_input,
                            observed_input_tokens: usize::try_from(
                                usage.last_token_usage.input_tokens,
                            )
                            .ok(),
                        },
                    )?;
                }
                return Ok(Some(response));
            }
            Err(error)
                if attempt < policy.max_attempts && should_retry_agent_turn_error(&error) =>
            {
                let detail = system.safe_error(&crate::core::model_request_error_detail(&error));
                crate::logging::write_current_turn_response_error_dump(
                    system, &actor.id, &detail, attempt, true,
                )
                .await?;
                tracing::warn!(actor_id=%actor.id, attempt, backoff_ms=policy.backoff_for_attempt(attempt).as_millis(), error=%detail, "Model request will retry");
                let backoff = tokio::time::sleep(policy.backoff_for_attempt(attempt));
                tokio::pin!(backoff);
                loop {
                    tokio::select! {
                        () = &mut backoff => break,
                        () = tokio::time::sleep(CANCELLATION_INTERVAL) => {
                            check_interruption(system,control, message)?;
                            if system.store().has_queued_input(&actor.id)? { return Ok(None); }
                        },
                    }
                }
            }
            Err(error) => {
                crate::logging::write_current_turn_response_error_dump(
                    system,
                    &actor.id,
                    &crate::core::model_request_error_detail(&error),
                    attempt,
                    false,
                )
                .await?;
                return Err(
                    if error
                        .downcast_ref::<crate::context_budget::ContextBudgetExceededError>()
                        .is_some()
                        || error
                            .downcast_ref::<crate::providers::ModelRequestFailure>()
                            .is_some()
                    {
                        error
                    } else {
                        crate::providers::ModelRequestFailure::report(
                            crate::providers::ModelFailureKind::Failed,
                            crate::core::model_request_error_detail(&error),
                        )
                    },
                );
            }
        }
    }
    unreachable!("retry loop returns on its final attempt")
}

fn handle_progress(
    system: &ActorSystem,
    actor: &ActorRecord,
    message: &MessageRecord,
    generation: &mut u32,
    pending: &mut ProgressBuffer,
    event: ModelProgressEvent,
) -> Result<()> {
    match event {
        ModelProgressEvent::GenerationStarted => {}
        ModelProgressEvent::DraftReset => {
            pending.flush(system, actor, message, *generation)?;
            *generation = generation.saturating_add(1);
            system.store().append_event(
                &actor.id,
                ActorEvent::GenerationStarted {
                    message_id: message.id.clone(),
                    generation: *generation,
                },
            )?;
        }
        ModelProgressEvent::AssistantDelta { text } => pending.assistant.push_str(&text),
        ModelProgressEvent::ReasoningDelta { text } => pending.reasoning.push_str(&text),
    }
    if pending.assistant.len() + pending.reasoning.len() >= PROGRESS_CHUNK_BYTES {
        pending.flush(system, actor, message, *generation)?;
    }
    Ok(())
}

#[derive(Default)]
struct ProgressBuffer {
    assistant: String,
    reasoning: String,
}

impl ProgressBuffer {
    fn flush(
        &mut self,
        system: &ActorSystem,
        actor: &ActorRecord,
        message: &MessageRecord,
        generation: u32,
    ) -> Result<()> {
        for (channel, buffer) in [
            ("assistant", &mut self.assistant),
            ("reasoning", &mut self.reasoning),
        ] {
            if buffer.is_empty() {
                continue;
            }
            let text = std::mem::take(buffer);
            let mut remaining = text.as_str();
            while !remaining.is_empty() {
                let mut end = remaining.len().min(PROGRESS_CHUNK_BYTES);
                while !remaining.is_char_boundary(end) {
                    end -= 1;
                }
                system.store().append_event(
                    &actor.id,
                    ActorEvent::GenerationDelta {
                        message_id: message.id.clone(),
                        generation,
                        channel: channel.to_owned(),
                        text: remaining[..end].to_owned(),
                    },
                )?;
                remaining = &remaining[end..];
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "runner_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "runner_timeout_tests.rs"]
mod timeout_tests;
