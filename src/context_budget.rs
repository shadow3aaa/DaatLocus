use miette::Report;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::reasoning::runtime::{
    AgentMessage, AgentToolInputSpec, AgentToolSpec, HistoryMessage, PromptRequest,
    estimate_assistant_tool_call_protocol_tokens,
};

pub const APPROX_BYTES_PER_TOKEN: usize = 4;
pub const DEFAULT_CONTEXT_WINDOW_TOKENS: usize = 128_000;
pub const DEFAULT_MAX_COMPLETION_TOKENS: usize = 4_000;
pub const DEFAULT_TOOL_OUTPUT_MAX_TOKENS: usize = 2_000;

#[derive(Clone, Copy, Debug)]
pub struct RequestBudgetLimits {
    pub context_window_tokens: usize,
    pub auto_compact_threshold_tokens: usize,
    pub reserved_output_tokens: usize,
}

#[derive(Clone, Debug)]
pub struct BudgetSection {
    pub name: &'static str,
    pub tokens: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TokenEstimateBaseline {
    pub estimated_input_tokens: usize,
    pub observed_input_tokens: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct RequestBudgetBreakdown {
    pub sections: Vec<BudgetSection>,
    pub total_input_tokens: usize,
    pub reserved_output_tokens: usize,
    pub total_with_reserve_tokens: usize,
    pub context_window_tokens: usize,
    pub auto_compact_threshold_tokens: usize,
}

#[derive(Debug, Error, miette::Diagnostic)]
#[error("{message}")]
#[diagnostic(code(runtime::context_budget_exceeded))]
pub struct ContextBudgetExceededError {
    message: String,
}

impl RequestBudgetLimits {
    pub fn normalized(self) -> Self {
        let context_window_tokens = self.context_window_tokens.max(1);
        let reserved_output_tokens = self.reserved_output_tokens.min(context_window_tokens);
        let auto_compact_threshold_tokens = self
            .auto_compact_threshold_tokens
            .clamp(1, context_window_tokens);
        Self {
            context_window_tokens,
            auto_compact_threshold_tokens,
            reserved_output_tokens,
        }
    }
}

impl TokenEstimateBaseline {
    pub fn calibrated_total_input_tokens(&self, current_estimated_input_tokens: usize) -> usize {
        let Some(observed) = self.observed_input_tokens else {
            return current_estimated_input_tokens;
        };
        if self.estimated_input_tokens == 0 {
            return current_estimated_input_tokens;
        }
        let delta = current_estimated_input_tokens.saturating_sub(self.estimated_input_tokens);
        observed.saturating_add(delta)
    }
}

impl RequestBudgetBreakdown {
    fn new(mut sections: Vec<BudgetSection>, limits: RequestBudgetLimits) -> Self {
        sections.retain(|section| section.tokens > 0);
        let limits = limits.normalized();
        let total_input_tokens = sections.iter().map(|section| section.tokens).sum::<usize>();
        let total_with_reserve_tokens =
            total_input_tokens.saturating_add(limits.reserved_output_tokens);
        Self {
            sections,
            total_input_tokens,
            reserved_output_tokens: limits.reserved_output_tokens,
            total_with_reserve_tokens,
            context_window_tokens: limits.context_window_tokens,
            auto_compact_threshold_tokens: limits.auto_compact_threshold_tokens,
        }
    }

    pub fn with_calibrated_input_tokens(mut self, baseline: &TokenEstimateBaseline) -> Self {
        let calibrated = baseline.calibrated_total_input_tokens(self.total_input_tokens);
        self.total_with_reserve_tokens = calibrated.saturating_add(self.reserved_output_tokens);
        self.total_input_tokens = calibrated;
        self
    }

    pub fn within_context_window(&self) -> bool {
        self.total_with_reserve_tokens <= self.context_window_tokens
    }

    pub fn above_auto_compact_threshold(&self) -> bool {
        let threshold = self.auto_compact_input_threshold_tokens();
        threshold > 0 && self.total_input_tokens >= threshold
    }

    pub fn auto_compact_input_threshold_tokens(&self) -> usize {
        self.auto_compact_threshold_tokens
            .min(self.input_budget_tokens())
    }

    pub fn input_budget_tokens(&self) -> usize {
        self.context_window_tokens
            .saturating_sub(self.reserved_output_tokens)
    }

    pub fn summary_lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!("estimated_input_tokens={}", self.total_input_tokens),
            format!("reserved_output_tokens={}", self.reserved_output_tokens),
            format!(
                "estimated_total_with_reserve={}",
                self.total_with_reserve_tokens
            ),
            format!("context_window_tokens={}", self.context_window_tokens),
            format!(
                "auto_compact_threshold_tokens={}",
                self.auto_compact_threshold_tokens
            ),
            format!(
                "auto_compact_input_threshold_tokens={}",
                self.auto_compact_input_threshold_tokens()
            ),
            format!("input_budget_tokens={}", self.input_budget_tokens()),
            format!(
                "within_context_window={}",
                yes_no(self.within_context_window())
            ),
            format!(
                "above_auto_compact_threshold={}",
                yes_no(self.above_auto_compact_threshold())
            ),
        ];
        lines.extend(
            self.sections
                .iter()
                .map(|section| format!("section.{}={}", section.name, section.tokens)),
        );
        lines
    }
}

impl ContextBudgetExceededError {
    pub fn for_request(
        kind: &str,
        model: &str,
        breakdown: &RequestBudgetBreakdown,
        detail: Option<&str>,
    ) -> Self {
        let mut lines = vec![format!(
            "{kind} context budget exceeded for model `{model}`"
        )];
        lines.extend(breakdown.summary_lines());
        if let Some(detail) = detail
            && !detail.trim().is_empty()
        {
            lines.push(format!("detail={detail}"));
        }
        Self {
            message: lines.join("\n"),
        }
    }
}

pub fn is_context_budget_exceeded(err: &Report) -> bool {
    err.downcast_ref::<ContextBudgetExceededError>().is_some()
}

pub fn approx_token_count(text: &str) -> usize {
    let len = text.len();
    len.saturating_add(APPROX_BYTES_PER_TOKEN.saturating_sub(1)) / APPROX_BYTES_PER_TOKEN
}

pub fn truncate_text_to_token_budget(text: &str, max_tokens: usize) -> String {
    truncate_text_to_token_budget_with_notice(text, max_tokens, "... [truncated for model context]")
}

pub fn truncate_text_to_token_budget_with_notice(
    text: &str,
    max_tokens: usize,
    notice: &str,
) -> String {
    let max_chars = max_tokens.saturating_mul(APPROX_BYTES_PER_TOKEN).max(1);
    let total_chars = text.chars().count();
    if total_chars <= max_chars {
        return text.to_string();
    }

    let kept = text.chars().take(max_chars).collect::<String>();
    format!(
        "{kept}\n{notice} ({} chars omitted)",
        total_chars.saturating_sub(max_chars)
    )
}

pub fn estimate_agent_turn_request(
    messages: &[AgentMessage],
    tools: &[AgentToolSpec],
    limits: RequestBudgetLimits,
) -> RequestBudgetBreakdown {
    let sections = vec![
        BudgetSection {
            name: "system_messages",
            tokens: messages
                .iter()
                .filter_map(|message| match message {
                    AgentMessage::System { .. } => Some(estimate_agent_message_tokens(message)),
                    _ => None,
                })
                .sum(),
        },
        BudgetSection {
            name: "user_messages",
            tokens: messages
                .iter()
                .filter_map(|message| match message {
                    AgentMessage::User { .. } => Some(estimate_agent_message_tokens(message)),
                    _ => None,
                })
                .sum(),
        },
        BudgetSection {
            name: "assistant_messages",
            tokens: messages
                .iter()
                .filter_map(|message| match message {
                    AgentMessage::Assistant { .. } => Some(estimate_agent_message_tokens(message)),
                    AgentMessage::AssistantToolCallProtocol {
                        content,
                        reasoning_content,
                        ..
                    } => Some(
                        content
                            .as_deref()
                            .map(|content| message_token_cost("assistant", content))
                            .unwrap_or(0)
                            .saturating_add(
                                reasoning_content
                                    .as_deref()
                                    .map(approx_token_count)
                                    .unwrap_or(0),
                            ),
                    ),
                    _ => None,
                })
                .sum(),
        },
        BudgetSection {
            name: "assistant_tool_call_protocol",
            tokens: messages
                .iter()
                .filter_map(|message| match message {
                    AgentMessage::AssistantToolCallProtocol { calls, .. } => {
                        Some(estimate_assistant_tool_call_protocol_tokens(
                            calls,
                            estimate_json_value_tokens,
                            approx_token_count,
                        ))
                    }
                    _ => None,
                })
                .sum(),
        },
        BudgetSection {
            name: "tool_messages",
            tokens: messages
                .iter()
                .filter_map(|message| match message {
                    AgentMessage::Tool { .. } => Some(estimate_agent_message_tokens(message)),
                    _ => None,
                })
                .sum(),
        },
        BudgetSection {
            name: "tool_specs",
            tokens: tools.iter().map(estimate_tool_spec_tokens).sum(),
        },
    ];
    RequestBudgetBreakdown::new(sections, limits)
}

pub fn estimate_runtime_request_envelope(
    system_messages: &[String],
    user_message: &str,
    tools: &[AgentToolSpec],
    limits: RequestBudgetLimits,
) -> RequestBudgetBreakdown {
    let sections = vec![
        BudgetSection {
            name: "system_messages",
            tokens: system_messages
                .iter()
                .map(|message| {
                    estimate_history_message_tokens(&HistoryMessage::system(message.as_str()))
                })
                .sum(),
        },
        BudgetSection {
            name: "current_user_message",
            tokens: estimate_history_message_tokens(&HistoryMessage::user(
                user_message.to_string(),
            )),
        },
        BudgetSection {
            name: "tool_specs",
            tokens: tools.iter().map(estimate_tool_spec_tokens).sum(),
        },
    ];
    RequestBudgetBreakdown::new(sections, limits)
}

pub fn estimate_prompt_request(
    request: &PromptRequest,
    limits: RequestBudgetLimits,
) -> RequestBudgetBreakdown {
    let tool_schema_tokens =
        approx_token_count(&request.tool_name) + approx_token_count(&request.tool_description) + 16;
    let sections = vec![
        BudgetSection {
            name: "system_messages",
            tokens: request
                .system_messages
                .iter()
                .map(|message| {
                    estimate_history_message_tokens(&HistoryMessage::system(message.as_str()))
                })
                .sum(),
        },
        BudgetSection {
            name: "memory_messages",
            tokens: request
                .long_term_memory_messages
                .iter()
                .map(estimate_history_message_tokens)
                .sum(),
        },
        BudgetSection {
            name: "history_messages",
            tokens: request
                .history_messages
                .iter()
                .map(estimate_history_message_tokens)
                .sum(),
        },
        BudgetSection {
            name: "current_user_message",
            tokens: estimate_history_message_tokens(&HistoryMessage::user(
                request.current_user_message.clone(),
            )),
        },
        BudgetSection {
            name: "retry_messages",
            tokens: request
                .retry_messages
                .iter()
                .map(estimate_history_message_tokens)
                .sum(),
        },
        BudgetSection {
            name: "output_schema",
            tokens: tool_schema_tokens
                .saturating_add(estimate_json_value_tokens(&request.output_schema)),
        },
    ];
    RequestBudgetBreakdown::new(sections, limits)
}

fn estimate_history_message_tokens(message: &HistoryMessage) -> usize {
    estimate_agent_message_tokens(&message.message)
}

pub(crate) fn estimate_agent_message_tokens(message: &AgentMessage) -> usize {
    match message {
        AgentMessage::System { content } => message_token_cost("system", content),
        AgentMessage::User { content } => message_token_cost("user", content.as_text())
            .saturating_add(content.parts().len() * 1024),
        AgentMessage::Assistant { content } => message_token_cost("assistant", content),
        AgentMessage::AssistantToolCallProtocol {
            content,
            reasoning_content,
            calls,
        } => {
            let content_tokens = content
                .as_deref()
                .map(|content| message_token_cost("assistant", content))
                .unwrap_or(4)
                .saturating_add(
                    reasoning_content
                        .as_deref()
                        .map(approx_token_count)
                        .unwrap_or(0),
                );
            content_tokens.saturating_add(estimate_assistant_tool_call_protocol_tokens(
                calls,
                estimate_json_value_tokens,
                approx_token_count,
            ))
        }
        AgentMessage::Tool {
            tool_call_id,
            name,
            content,
        } => message_token_cost("tool", content)
            .saturating_add(approx_token_count(tool_call_id))
            .saturating_add(approx_token_count(name))
            .saturating_add(8),
    }
}

pub(crate) fn estimate_tool_spec_tokens(tool: &AgentToolSpec) -> usize {
    let input_tokens = match &tool.input_spec {
        AgentToolInputSpec::JsonSchema { schema } => estimate_json_value_tokens(schema),
        AgentToolInputSpec::FreeformGrammar {
            syntax,
            definition,
            fallback_schema,
        } => approx_token_count(syntax)
            .saturating_add(approx_token_count(definition))
            .saturating_add(estimate_json_value_tokens(fallback_schema)),
    };
    approx_token_count(&tool.name)
        .saturating_add(approx_token_count(&tool.description))
        .saturating_add(input_tokens)
        .saturating_add(24)
}

fn estimate_json_value_tokens(value: &Value) -> usize {
    serde_json::to_string(value)
        .ok()
        .map(|text| approx_token_count(&text))
        .unwrap_or_default()
}

fn message_token_cost(role: &str, content: &str) -> usize {
    approx_token_count(role) + approx_token_count(content) + 4
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn breakdown_for(input_tokens: usize, limits: RequestBudgetLimits) -> RequestBudgetBreakdown {
        RequestBudgetBreakdown::new(
            vec![BudgetSection {
                name: "user_messages",
                tokens: input_tokens,
            }],
            limits,
        )
    }

    #[test]
    fn auto_compact_threshold_is_based_on_input_tokens() {
        let limits = RequestBudgetLimits {
            context_window_tokens: 128_000,
            auto_compact_threshold_tokens: 100_000,
            reserved_output_tokens: 4_000,
        };

        assert!(!breakdown_for(99_999, limits).above_auto_compact_threshold());
        assert!(breakdown_for(100_000, limits).above_auto_compact_threshold());
    }

    #[test]
    fn zero_reserved_output_is_valid() {
        let limits = RequestBudgetLimits {
            context_window_tokens: 128_000,
            auto_compact_threshold_tokens: 115_200,
            reserved_output_tokens: 0,
        };
        let breakdown = breakdown_for(115_199, limits);

        assert_eq!(breakdown.reserved_output_tokens, 0);
        assert_eq!(breakdown.input_budget_tokens(), 128_000);
        assert_eq!(breakdown.auto_compact_input_threshold_tokens(), 115_200);
        assert!(breakdown.within_context_window());
        assert!(!breakdown.above_auto_compact_threshold());
    }

    #[test]
    fn large_reserved_output_does_not_force_auto_compaction_for_small_input() {
        let limits = RequestBudgetLimits {
            context_window_tokens: 258_400,
            auto_compact_threshold_tokens: 128_000,
            reserved_output_tokens: 128_000,
        };
        let breakdown = breakdown_for(5_538, limits);

        assert_eq!(breakdown.input_budget_tokens(), 130_400);
        assert_eq!(breakdown.auto_compact_input_threshold_tokens(), 128_000);
        assert!(breakdown.within_context_window());
        assert!(!breakdown.above_auto_compact_threshold());
    }

    #[test]
    fn token_estimate_baseline_returns_estimated_when_no_observed() {
        let baseline = TokenEstimateBaseline::default();
        assert_eq!(baseline.calibrated_total_input_tokens(500_000), 500_000);
    }

    #[test]
    fn token_estimate_baseline_returns_calibrated_when_observed() {
        let baseline = TokenEstimateBaseline {
            estimated_input_tokens: 545_000,
            observed_input_tokens: Some(810_000),
        };
        assert_eq!(baseline.calibrated_total_input_tokens(600_000), 865_000);
    }

    #[test]
    fn token_estimate_baseline_handles_observed_zero() {
        let baseline = TokenEstimateBaseline {
            estimated_input_tokens: 545_000,
            observed_input_tokens: Some(0),
        };
        assert_eq!(baseline.calibrated_total_input_tokens(600_000), 55_000);
    }

    #[test]
    fn token_estimate_baseline_with_calibrated_input_tokens() {
        let baseline = TokenEstimateBaseline {
            estimated_input_tokens: 545_000,
            observed_input_tokens: Some(810_000),
        };
        let limits = RequestBudgetLimits {
            context_window_tokens: 1_000_000,
            auto_compact_threshold_tokens: 900_000,
            reserved_output_tokens: 400_000,
        };
        let breakdown = RequestBudgetBreakdown::new(
            vec![BudgetSection {
                name: "user_messages",
                tokens: 600_000,
            }],
            limits,
        )
        .with_calibrated_input_tokens(&baseline);
        assert_eq!(breakdown.total_input_tokens, 865_000);
        assert_eq!(breakdown.total_with_reserve_tokens, 1_265_000);
    }
}
