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
    pub(super) instruction_binding: (u64, crate::stage::StageId),
    pub system: AgentMessage,
    pub tools: Vec<AgentToolSpec>,
    pub history: Vec<AgentMessage>,
    pub history_complete: bool,
}

impl ModelContext {
    pub async fn load(
        system: &ActorSystem,
        actor: &ActorRecord,
        context: &AppToolExecutionContext,
        tools: Vec<AgentToolSpec>,
    ) -> Result<Self> {
        Ok(Self {
            instruction_binding: system.instructions().binding(),
            system: AgentMessage::system(
                SystemPrompt::load(system, actor, context).await?.render(),
            ),
            tools,
            history: Vec::new(),
            history_complete: false,
        })
    }

    pub async fn refresh_instructions(
        &mut self,
        system: &ActorSystem,
        actor: &ActorRecord,
        context: &AppToolExecutionContext,
    ) -> Result<()> {
        let binding = system.instructions().binding();
        if binding != self.instruction_binding {
            self.system =
                AgentMessage::system(SystemPrompt::load(system, actor, context).await?.render());
            self.instruction_binding = binding;
        }
        Ok(())
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
        // An interrupted input may have lost its window to a nested reset.
        // Restore only missing history at the tail; older input stays immutable.
        if let Some(recovery) = &anchor
            && recovery.message_id != current.id
            && system
                .store()
                .history_page(actor, start.saturating_sub(1), Some(&current.id), 1)?
                .is_empty()
        {
            let recovery = recovery
                .for_input(&current.id, system.store().task_handoff(actor, &current.id))
                .recovery_message();
            system
                .store()
                .append_history(actor, &current.id, &[recovery])?;
        }
        let mut after = start.saturating_sub(1);
        let mut history = Vec::new();
        self.history_complete = false;
        self.history = loop {
            let page = system.store().history_page(actor, after, None, 128)?;
            if page.is_empty() {
                self.history_complete = true;
                break balanced_history(history);
            }
            for (seq, _, mut row) in page {
                after = seq;
                if seq == start
                    && let Some(recovery) = &anchor
                {
                    // This is historical input. Retargeting it here changes the
                    // entire prefix even when no further reset has occurred.
                    row = recovery.recovery_message();
                }
                history.push(row);
            }
            let balanced = balanced_history(history.clone());
            if !self.fits(provider, &balanced, baseline) {
                self.history_complete = system
                    .store()
                    .history_page(actor, after, None, 1)?
                    .is_empty();
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

    /// Use the old window to write the checkpoint before publishing a new anchor.
    pub fn handoff_request(
        &self,
        input: &MessageRecord,
        provider: &dyn ModelProvider,
        baseline: &TokenEstimateBaseline,
    ) -> Result<AgentTurnRequest> {
        if !self.can_recover() {
            return Err(miette!(
                "Context recovery requires an authorized Actors App history tool"
            ));
        }
        let excerpt: String = input.content.chars().take(4096).collect();
        let build = |count: usize| {
            let mut messages = vec![self.system.clone()];
            messages.extend(balanced_history(self.history[..count].to_vec()));
            messages.push(AgentMessage::user(format!(
                "{}\nCurrent input ID: {}\nCurrent input excerpt: {}\nLoaded history suffix omitted: {}",
                include_str!("../prompts/context_handoff.md"), input.id, excerpt, !self.history_complete || count < self.history.len()
            )));
            AgentTurnRequest {
                messages,
                tools: vec![],
            }
        };
        let fits = |request: &AgentTurnRequest| {
            estimate_agent_turn_request(&request.messages, &[], provider.request_budget_limits())
                .with_conservative_calibrated_input_tokens(baseline)
                .within_context_window()
        };
        // A single oversized input/observation may exceed even the hard limit.
        // Keep a complete prefix only for note generation; durable history is untouched.
        let full = build(self.history.len());
        if fits(&full) {
            return Ok(full);
        }
        let mut low = 0;
        let mut high = self.history.len();
        if !fits(&build(0)) {
            return Err(miette!(
                "Context handoff instructions exceed the model budget"
            ));
        }
        while low + 1 < high {
            let middle = low + (high - low) / 2;
            if fits(&build(middle)) {
                low = middle;
            } else {
                high = middle;
            }
        }
        Ok(build(low))
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
