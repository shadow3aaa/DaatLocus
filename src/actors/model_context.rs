//! One transient model window. Durable history and its anchor remain in the store.
use super::{ActorRecord, ActorSystem, MessageRecord, prompt::SystemPrompt};
use crate::{
    app::AppToolExecutionContext,
    context_budget::{TokenEstimateBaseline, estimate_agent_turn_request},
    core::ModelProvider,
    model_protocol::{AgentMessage, AgentToolSpec, AgentTurnRequest},
};
use miette::{Result, miette};

pub(super) struct ModelContext {
    pub system: AgentMessage,
    pub tools: Vec<AgentToolSpec>,
    pub history: Vec<AgentMessage>,
}

impl ModelContext {
    pub async fn load(
        system: &ActorSystem,
        actor: &ActorRecord,
        input: &MessageRecord,
        context: &AppToolExecutionContext,
        tools: Vec<AgentToolSpec>,
    ) -> Result<Self> {
        Ok(Self {
            system: AgentMessage::system(
                SystemPrompt::load(system, actor, context)
                    .await?
                    .render_for_input(&input.id),
            ),
            tools,
            history: Vec::new(),
        })
    }

    pub fn baseline(
        system: &ActorSystem,
        actor: &str,
        binding: &str,
    ) -> Result<TokenEstimateBaseline> {
        let state = system.store().actor(actor)?.context;
        Ok(if state.model_binding == binding {
            state.token_estimate
        } else {
            TokenEstimateBaseline::default()
        })
    }

    pub fn can_recover(&self) -> bool {
        self.tools
            .iter()
            .any(|tool| tool.name.ends_with("actors__read_history"))
    }

    pub fn reload(
        &mut self,
        system: &ActorSystem,
        current: &MessageRecord,
        provider: &dyn ModelProvider,
        baseline: &TokenEstimateBaseline,
    ) -> Result<()> {
        let actor = &current.actor_id;
        let start = system.store().context_start(actor)?;
        let anchor = system.store().actor(actor)?.context.anchor;
        let mut after = start.saturating_sub(1);
        let mut history = Vec::new();
        self.history = loop {
            let page = system.store().history_page(actor, after, None, 128)?;
            if page.is_empty() {
                break balanced_history(history);
            }
            for (seq, _, mut row) in page {
                after = seq;
                if seq == start
                    && let Some(recovery) = &anchor
                {
                    // Resolve the active input's task, never the task of a nested reset.
                    row = recovery
                        .for_input(&current.id, system.store().task_handoff(actor, &current.id))
                        .recovery_message();
                }
                history.push(row);
            }
            let balanced = balanced_history(history.clone());
            if !self.fits(provider, &balanced, baseline) {
                break balanced;
            }
        };
        Ok(())
    }

    fn fits(
        &self,
        provider: &dyn ModelProvider,
        history: &[AgentMessage],
        baseline: &TokenEstimateBaseline,
    ) -> bool {
        // Borrow the window during admission; do not clone schemas on every page.
        let budget = estimate_agent_turn_request(
            std::iter::once(&self.system).chain(history),
            &self.tools,
            provider.request_budget_limits(),
        )
        .with_conservative_calibrated_input_tokens(baseline);
        budget.within_context_window() && !budget.above_auto_compact_threshold()
    }

    pub fn admitted(
        &self,
        provider: &dyn ModelProvider,
        baseline: &TokenEstimateBaseline,
    ) -> Option<AgentTurnRequest> {
        self.fits(provider, &self.history, baseline)
            .then(|| AgentTurnRequest {
                messages: std::iter::once(&self.system)
                    .chain(&self.history)
                    .cloned()
                    .collect(),
                tools: self.tools.clone(),
            })
    }

    pub fn request(
        &mut self,
        system: &ActorSystem,
        input: &MessageRecord,
        provider: &dyn ModelProvider,
        baseline: &TokenEstimateBaseline,
    ) -> Result<AgentTurnRequest> {
        if let Some(request) = self.admitted(provider, baseline) {
            return Ok(request);
        }
        if !self.can_recover() {
            return Err(miette!(
                "Context recovery requires an authorized Actors App history tool"
            ));
        }
        let recovery = system
            .store()
            .reset_model_context(&input.actor_id, &input.id)?;
        self.history = vec![recovery];
        self.admitted(provider, baseline).ok_or_else(|| {
            miette!(
                "System instructions, App definitions and recovery input exceed the model budget"
            )
        })
    }
}

/// Discard only incomplete replay groups from a bounded history query. The
/// durable history is untouched; tool results never become free-standing input.
pub(super) fn balanced_history(history: Vec<AgentMessage>) -> Vec<AgentMessage> {
    // An approval can suspend a protocol group while another input is handled.
    // Present complete groups atomically in settlement order, retaining source
    // order in durable history. Incomplete intents never enter a model request.
    let mut settled = Vec::new();
    let mut pending: Vec<(AgentMessage, Vec<String>, Vec<AgentMessage>)> = Vec::new();
    for row in history {
        match &row {
            AgentMessage::AssistantToolCallProtocol { calls, .. } if !calls.is_empty() => {
                let ids = calls.iter().map(|call| call.id.clone()).collect();
                pending.push((row, ids, Vec::new()));
            }
            AgentMessage::Tool { tool_call_id, .. } => {
                if let Some(index) = pending.iter().rposition(|(_, ids, results)| {
                    ids.contains(tool_call_id)
                        && !results.iter().any(|result| {
                            matches!(result,
                        AgentMessage::Tool { tool_call_id: id, .. } if id == tool_call_id)
                        })
                }) {
                    pending[index].2.push(row);
                    if pending[index].1.len() == pending[index].2.len() {
                        let (protocol, _, results) = pending.remove(index);
                        settled.push(protocol);
                        settled.extend(results);
                    }
                }
            }
            _ => settled.push(row),
        }
    }
    settled
}
