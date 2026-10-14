import type { DiscoveredModel, ModelConfig } from "./actor-api";

export function selectDiscoveredModel(model: ModelConfig, found: DiscoveredModel): ModelConfig {
  return {...model,
    name: !model.name || model.name === model.model_id ? found.id : model.name,
    model_id: found.id, context_window_tokens: found.context_window_tokens ?? null,
    max_completion_tokens: found.max_completion_tokens ?? null,
    supports_vision: found.supports_vision ?? null, thinking_budget: null,
  };
}

export function reasoningValues(model?: DiscoveredModel): string[] {
  const values = [...(model?.thinking_budgets ?? [])];
  for (const option of model?.reasoning_options ?? []) {
    if (option.type === "toggle") values.push("true", "false");
    if (option.type === "effort") values.push(...option.values);
    if (option.type === "budget_tokens") {
      values.push(...[option.min, 1024, 4096, 8192, 16384, option.max]
        .filter((value): value is number => value !== null && value > 0 && value >= option.min && (option.max === null || value <= option.max))
        .map(String));
    }
  }
  return [...new Set(values.filter(Boolean))];
}
