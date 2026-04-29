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
    match client
        .thinking_budget
        .map(ThinkingBudget::deepseek_thinking_type)
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
    thinking_budget: Option<ThinkingBudget>,
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
    thinking_budget: Option<ThinkingBudget>,
) {
    let Some(budget) = thinking_budget else {
        return;
    };
    let thinking_type = budget.deepseek_thinking_type();
    let Some(object) = payload.as_object_mut() else {
        return;
    };
    object.insert("thinking".to_string(), json!({ "type": thinking_type }));
    if let Some(reasoning_effort) = budget.deepseek_reasoning_effort()
        && thinking_type == "enabled"
    {
        object.insert("reasoning_effort".to_string(), json!(reasoning_effort));
    }
}

pub(super) fn apply_optional_thinking_budget(
    payload: &mut serde_json::Value,
    thinking_budget: Option<ThinkingBudget>,
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
            let Some(effort) = budget.as_chat_reasoning_effort() else {
                return;
            };
            object.insert("reasoning_effort".to_string(), json!(effort));
        }
        ThinkingBudgetMode::NestedReasoningObject => {
            let Some(effort) = budget.as_chat_reasoning_effort() else {
                return;
            };
            object.insert("reasoning".to_string(), json!({ "effort": effort }));
        }
        ThinkingBudgetMode::Unsupported => {}
    }
}
