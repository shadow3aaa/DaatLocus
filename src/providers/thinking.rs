use super::*;

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
    match deepseek_thinking_type(client.thinking_budget.as_deref()) {
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

pub(super) fn deepseek_thinking_type(value: Option<&str>) -> Option<&'static str> {
    let value = value?.trim().to_ascii_lowercase();
    if value.is_empty() {
        return None;
    }
    Some(match value.as_str() {
        "0" | "false" | "off" | "none" | "disable" | "disabled" | "no" => "disabled",
        _ => "enabled",
    })
}

pub(super) fn deepseek_reasoning_effort(value: Option<&str>) -> Option<&'static str> {
    let value = value?.trim().to_ascii_lowercase();
    if value.is_empty() {
        return None;
    }
    match value.as_str() {
        "0" | "false" | "off" | "none" | "disable" | "disabled" | "no" => None,
        "xhigh" | "max" | "maximum" => Some("max"),
        // DeepSeek currently accepts high/max. Treat generic low/medium/high budgets as high.
        _ => Some("high"),
    }
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
    let Some(thinking_type) = deepseek_thinking_type(thinking_budget) else {
        return;
    };
    let Some(object) = payload.as_object_mut() else {
        return;
    };
    object.insert("thinking".to_string(), json!({ "type": thinking_type }));
    if let Some(reasoning_effort) = deepseek_reasoning_effort(thinking_budget)
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
    let Some(thinking_budget) = thinking_budget else {
        return;
    };
    let Some(object) = payload.as_object_mut() else {
        return;
    };
    match mode {
        ThinkingBudgetMode::ReasoningEffortString => {
            object.insert("reasoning_effort".to_string(), json!(thinking_budget));
        }
        ThinkingBudgetMode::NestedReasoningObject => {
            object.insert(
                "reasoning".to_string(),
                json!({ "effort": thinking_budget }),
            );
        }
        ThinkingBudgetMode::Unsupported => {}
    }
}
