import type { DiscoveredModel, ModelConfig, ProviderConfig, ProviderKind, SetupConfigRequest } from "./actor-api";

function withRemainingModels(config: SetupConfigRequest, models: ModelConfig[]): SetupConfigRequest {
  return {...config, models,
    main_model: models.some(model => model.name === config.main_model) ? config.main_model : models[0]?.name ?? null,
    approval_model: models.some(model => model.name === config.approval_model) ? config.approval_model : null,
  };
}
export function removeProvider(config: SetupConfigRequest, index: number): SetupConfigRequest {
  const name = config.providers[index]?.name;
  return withRemainingModels({...config, providers: config.providers.filter((_, at) => at !== index)},
    config.models.filter(model => model.provider_name !== name));
}
export function removeConfiguredModel(config: SetupConfigRequest, index: number): SetupConfigRequest {
  return withRemainingModels(config, config.models.filter((_, at) => at !== index));
}

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

export function isOpenCodeProvider(kind: ProviderKind): boolean {
  return kind === "open_code_console" || kind === "open_code_go";
}

export function changeProviderKind(provider: ProviderConfig, kind: ProviderKind): ProviderConfig {
  if (provider.kind === kind) return provider;
  if (isOpenCodeProvider(provider.kind) && isOpenCodeProvider(kind)) {
    return { ...provider, kind, base_url: null };
  }
  return { ...provider, kind, base_url: isOpenCodeProvider(provider.kind) || isOpenCodeProvider(kind) ? null : provider.base_url, api_key: null,
    credential_configured: false, credential_name: null, codex_auth_method: null,
    github_auth_method: null, opencode_auth_method: null, codex_auth_file: null };
}
