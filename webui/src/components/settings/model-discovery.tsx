import { t } from "@/lib/i18n";
import { useEffect, useState } from "react";
import { ChevronRightIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Spinner } from "@/components/ui/spinner";
import { LazyPage } from "@/components/lazy-page";
import { discoverModels, type DiscoveredModel, type ProviderConfig } from "@/lib/actor-api";
import { errorText, StatusNote } from "./controls";

export function useModelDiscovery(provider?: ProviderConfig) {
  const [attempt, setAttempt] = useState(0);
  const [state, setState] = useState<{provider?: ProviderConfig; models: DiscoveredModel[]; loading: boolean; error: string | null; source?: string}>({models: [], loading: false, error: null});
  useEffect(() => {
    if (!provider) return;
    const controller = new AbortController();
    const keyRequired = ["openai", "openai_compatible", "anthropic_compatible", "ollama_cloud"].includes(provider.kind);
    const needsKey = keyRequired && !provider.api_key?.trim() && !provider.credential_configured;
    const needsUrl = ["openai_compatible", "anthropic_compatible"].includes(provider.kind) && !provider.base_url?.trim();
    if (needsKey || needsUrl) {
      setState({provider, models: [], loading: false, error: t(needsUrl ? "model_discovery.base_url_required" : "model_discovery.api_key_required")});
      return;
    }
    setState({provider, models: [], loading: true, error: null});
    void discoverModels(provider, {signal: controller.signal}).then(result => {
      if (!controller.signal.aborted) setState({provider, ...result, loading: false, error: null});
    }).catch(cause => {
      if (!controller.signal.aborted) setState({provider, models: [], loading: false, error: errorText(cause)});
    });
    return () => controller.abort();
  }, [provider, attempt]);
  return {...(state.provider === provider ? state : {models: [], loading: Boolean(provider), error: null, source: undefined}), retry: () => setAttempt(value => value + 1)};
}

export function ModelDiscovery({state, onSelect, onCustom, disabled}: {
  state: ReturnType<typeof useModelDiscovery>; onSelect: (model: DiscoveredModel) => void; onCustom: () => void; disabled?: boolean;
}) {
  const [query, setQuery] = useState("");
  const [limit, setLimit] = useState(15);
  const filtered = state.models.filter(model => model.id.toLowerCase().includes(query.toLowerCase()));
  return <div className="dl-settings-editor">
    <Input aria-label={t("model_discovery.search_models")} placeholder={t("model_discovery.search_models")} value={query} onChange={event => {setQuery(event.target.value); setLimit(15);}} />
    {state.loading ? <div className="dl-settings-loading" role="status"><Spinner />{t("model_discovery.loading_models")}</div> : null}
    <StatusNote text={state.error} error />
    {state.error && <Button variant="outline" disabled={disabled || state.loading} onClick={state.retry}>{t("app.retry")}</Button>}
    {state.source === "catalog" ? <p className="dl-settings-caption">{t("model_discovery.catalog_models")}</p> : null}
    <div className="dl-settings-discovery">
      {filtered.slice(0, limit).map(model => <Button key={model.id} variant="ghost" disabled={disabled} onClick={() => onSelect(model)}><span className="truncate">{model.id}</span><ChevronRightIcon data-icon="inline-end" /></Button>)}
      {!state.loading && !state.error && !filtered.length ? <p className="dl-settings-caption">{t("model_discovery.no_matching_models")}</p> : null}
      {filtered.length > limit && <LazyPage key={query} cursor={limit} enabled={!disabled} onLoad={async () => setLimit(value => value + 30)} errorMessage={t("lazy_page.load_failed")} />}
      <Button variant="outline" disabled={disabled} onClick={onCustom}>{t("model_discovery.custom_model")}</Button>
    </div>
  </div>;
}
