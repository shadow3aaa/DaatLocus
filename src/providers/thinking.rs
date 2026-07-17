use super::{OpenAIClient, ThinkingBudgetMode, json};

pub(super) const DEEPSEEK_THINKING_MAX_TOKENS: usize = 65_536;

pub(super) fn max_completion_tokens_for_chat_payload(client: &OpenAIClient) -> usize {
    if is_deepseek_thinking_request(client) {
        client
            .max_completion_tokens
            .min(DEEPSEEK_THINKING_MAX_TOKENS)
    } else {
        client.max_completion_tokens
    }
}

pub(super) fn is_deepseek_api_base_url(base_url: &str) -> bool {
    base_url
        .trim_end_matches('/')
        .to_ascii_lowercase()
        .contains("api.deepseek.com")
}

pub(super) fn is_deepseek_thinking_request(client: &OpenAIClient) -> bool {
    if !is_deepseek_api_base_url(&client.base_url) {
        return false;
    }
    match client
        .thinking_budget
        .as_deref()
        .map(deepseek_thinking_type)
    {
        Some("enabled") => return true,
        Some("disabled") => return false,
        _ => {}
    }
    deepseek_model_defaults_to_thinking(&client.model)
}

pub(super) fn deepseek_model_defaults_to_thinking(model_id: &str) -> bool {
    let model = model_id.to_ascii_lowercase();
    matches!(
        model.as_str(),
        "deepseek-reasoner" | "deepseek-v4-flash" | "deepseek-v4-pro"
    ) || model.ends_with("/deepseek-reasoner")
        || model.ends_with("/deepseek-v4-flash")
        || model.ends_with("/deepseek-v4-pro")
}

pub(super) fn apply_provider_thinking_config(
    payload: &mut serde_json::Value,
    client: &OpenAIClient,
    thinking_budget: Option<&str>,
    mode: ThinkingBudgetMode,
) {
    if is_deepseek_api_base_url(&client.base_url) {
        apply_optional_deepseek_thinking(payload, thinking_budget);
    } else {
        apply_optional_thinking_budget(payload, thinking_budget, mode);
    }
}

pub(super) fn apply_optional_deepseek_thinking(
    payload: &mut serde_json::Value,
    thinking_budget: Option<&str>,
) {
    let Some(budget) = thinking_budget else {
        return;
    };
    let thinking_type = deepseek_thinking_type(budget);
    let Some(object) = payload.as_object_mut() else {
        return;
    };
    object.insert("thinking".to_string(), json!({ "type": thinking_type }));
    if let Some(reasoning_effort) = deepseek_reasoning_effort(budget)
        && thinking_type == "enabled"
    {
        object.insert("reasoning_effort".to_string(), json!(reasoning_effort));
    }
}

pub(super) fn apply_optional_thinking_budget(
    payload: &mut serde_json::Value,
    thinking_budget: Option<&str>,
    mode: ThinkingBudgetMode,
) {
    let Some(budget) = thinking_budget else {
        return;
    };
    let Some(object) = payload.as_object_mut() else {
        return;
    };
    match mode {
        ThinkingBudgetMode::ReasoningEffortString => {
            let Some(effort) = chat_reasoning_effort(budget) else {
                return;
            };
            object.insert("reasoning_effort".to_string(), json!(effort));
        }
        ThinkingBudgetMode::NestedReasoningObject => {
            let Some(effort) = chat_reasoning_effort(budget) else {
                return;
            };
            object.insert("reasoning".to_string(), json!({ "effort": effort }));
        }
        ThinkingBudgetMode::Unsupported => {}
    }
}

pub(super) fn chat_reasoning_effort(budget: &str) -> Option<&str> {
    if thinking_budget_is_none(budget) {
        None
    } else {
        Some(budget)
    }
}

pub(super) fn responses_reasoning_effort(budget: &str) -> &str {
    match normalized_thinking_budget(budget).as_str() {
        "none" => "none",
        // This Responses endpoint names the largest OpenAI effort xhigh.
        "max" => "xhigh",
        _ => budget,
    }
}

pub(super) fn deepseek_thinking_type(budget: &str) -> &'static str {
    if thinking_budget_is_none(budget) {
        "disabled"
    } else {
        "enabled"
    }
}

pub(super) fn deepseek_reasoning_effort(budget: &str) -> Option<&'static str> {
    if thinking_budget_is_none(budget) {
        None
    } else if normalized_thinking_budget(budget) == "max" {
        Some("max")
    } else {
        // DeepSeek currently accepts high/max. Treat generic non-max budgets as high.
        Some("high")
    }
}

pub(super) fn thinking_budget_is_none(budget: &str) -> bool {
    normalized_thinking_budget(budget) == "none"
}

fn normalized_thinking_budget(budget: &str) -> String {
    budget.trim().to_ascii_lowercase()
}
