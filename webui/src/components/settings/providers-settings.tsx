import { t } from "@/lib/i18n";
import { useLayoutEffect, useRef, useState } from "react";
import { ArrowLeftIcon, ChevronRightIcon, PlusIcon, Trash2Icon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { FieldGroup } from "@/components/ui/field";
import { Empty, EmptyHeader, EmptyTitle, EmptyDescription } from "@/components/ui/empty";
import type { DiscoveredModel, ModelConfig, ProviderConfig, SetupConfigRequest } from "@/lib/actor-api";
import { removeProvider, removeConfiguredModel, selectDiscoveredModel } from "@/lib/model-settings";
import { ModelEditor } from "./model-editor";
import { ModelDiscovery, useModelDiscovery } from "./model-discovery";
import { ProviderEditor, providerKindLabel } from "./provider-editor";
import { Picker } from "./controls";
import { SettingsGroup, SettingsSection } from "./structure";

type Page = {type: "list"} | {type: "provider"; index: number; adding: boolean} | {type: "model"; index: number; adding: boolean; choosing: boolean};
const uniqueName = (base: string, names: string[]) => {let name = base; let suffix = 2; while (names.includes(name)) name = `${base}-${suffix++}`; return name;};

export function ProvidersSettings({config, onChange, onSave, pending, dirty = false}: {
  config: SetupConfigRequest; onChange: (config: SetupConfigRequest) => void; onSave: () => Promise<boolean>; pending: boolean; dirty?: boolean;
}) {
  const [page, setPage] = useState<Page>({type: "list"});
  const root = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => { const scroll = root.current?.closest(".dl-settings-scroll"); if (scroll) scroll.scrollTop = 0; }, [page.type, page.type === "list" ? null : page.index, page.type === "model" && page.choosing]);
  const previous = useRef<{page: Page; config: SetupConfigRequest}[]>([]);
  const provider = page.type === "provider" ? config.providers[page.index] : undefined;
  const model = page.type === "model" ? config.models[page.index] : undefined;
  const modelProvider = model ? config.providers.find(provider => provider.name === model.provider_name) : undefined;
  const discovery = useModelDiscovery(modelProvider);
  const names = config.models.map(model => model.name).filter(Boolean);

  function addProvider() {
    previous.current = [{page, config}];
    onChange({...config, providers: [...config.providers, {kind: "openai", name: uniqueName("openai", config.providers.map(provider => provider.name)), api_key: null}]});
    setPage({type: "provider", index: config.providers.length, adding: true});
  }
  function addModel(providerName = config.providers[0]?.name ?? "") {
    previous.current.push({page, config});
    onChange({...config, models: [...config.models, {name: "", provider_name: providerName, model_id: ""}]});
    setPage({type: "model", index: config.models.length, adding: true, choosing: true});
  }
  function editProvider(next: ProviderConfig) {
    if (page.type !== "provider" || !provider) return;
    onChange({...config, providers: config.providers.map((item, index) => index === page.index ? next : item),
      models: config.models.map(model => model.provider_name === provider.name ? {...model, provider_name: next.name} : model)});
  }
  function editModel(next: ModelConfig) {
    if (page.type !== "model" || !model) return;
    onChange({...config, models: config.models.map((item, index) => index === page.index ? next : item),
      main_model: !config.main_model || config.main_model === model.name ? next.name : config.main_model,
      approval_model: config.approval_model === model.name ? next.name : config.approval_model});
  }
  function selectModel(found: DiscoveredModel) {
    if (page.type !== "model" || !model) return;
    const next = selectDiscoveredModel(model, found);
    next.name = uniqueName(next.name, config.models.filter((_, index) => index !== page.index).map(item => item.name));
    editModel(next); setPage({...page, choosing: false});
  }
  function removeModel(index: number) {
    onChange(removeConfiguredModel(config, index));
    setPage({type: "list"});
  }
  function back() {
    if (page.type !== "list" && page.adding) {
      const prior = previous.current.pop();
      if (prior) {onChange(prior.config); setPage(prior.page); return;}
    }
    setPage({type: "list"});
  }
  async function save() {if (await onSave()) {previous.current = [];setPage({type: "list"});}}
  const ready = config.models.every(model => model.name.trim() && model.model_id.trim() && config.providers.some(provider => provider.name === model.provider_name));

  return <div ref={root} className="dl-settings-models">
    {page.type !== "list" ? <div className="dl-settings-subpage-heading"><Button variant="ghost" size="icon-sm" disabled={pending} aria-label={t("providers_settings.back_to_providers")} onClick={back}><ArrowLeftIcon /></Button>
      <h3>{page.type === "provider" ? page.adding ? t("providers_settings.add_provider") : provider?.name || t("providers_settings.edit_provider") : page.adding ? t("providers_settings.add_model") : model?.name || t("providers_settings.edit_model")}</h3>
      {page.adding ? null : page.type === "provider" ? <Button variant="ghost" size="icon-sm" disabled={pending} aria-label={t("providers_settings.remove_provider")} onClick={() => {onChange(removeProvider(config, page.index));previous.current = [];setPage({type: "list"});}}><Trash2Icon /></Button> : <Button variant="ghost" size="icon-sm" disabled={pending} aria-label={t("providers_settings.remove_model")} onClick={() => removeModel(page.index)}><Trash2Icon /></Button>}
    </div> : null}
    {page.type === "list" && !config.providers.length ? <Empty className="dl-provider-empty"><EmptyHeader><EmptyTitle>{t("providers_settings.no_providers")}</EmptyTitle><EmptyDescription>{t("providers_settings.add_provider_to_start")}</EmptyDescription></EmptyHeader><Button disabled={pending} onClick={addProvider}><PlusIcon data-icon="inline-start" />{t("providers_settings.add_provider")}</Button></Empty> : page.type === "list" ? <>
      <SettingsSection title={t("providers_settings.providers")} action={<Button variant="ghost" size="sm" disabled={pending} onClick={addProvider}><PlusIcon data-icon="inline-start" />{t("composer.add")}</Button>}>
        {config.providers.length ? <SettingsGroup>{config.providers.map((provider, index) => <Button key={index} className="dl-setting-list-row" variant="ghost" disabled={pending} onClick={() => setPage({type: "provider", index, adding: false})}><span className="dl-setting-list-text"><span>{provider.name}</span><span className="dl-settings-caption">{providerKindLabel(provider.kind)}</span></span><ChevronRightIcon data-icon="inline-end" /></Button>)}</SettingsGroup> : null}
      </SettingsSection>
      <SettingsSection title={t("composer.models")} action={<Button variant="ghost" size="sm" disabled={pending || !config.providers.length} onClick={() => addModel()}><PlusIcon data-icon="inline-start" />{t("composer.add")}</Button>}>
        {config.models.length ? <SettingsGroup>{config.models.map((model, index) => <Button key={index} className="dl-setting-list-row" variant="ghost" disabled={pending} onClick={() => setPage({type: "model", index, adding: false, choosing: !model.model_id})}><span className="dl-setting-list-text"><span>{model.name || t("providers_settings.unnamed_model")}</span><span className="dl-settings-caption">{model.provider_name}</span></span><ChevronRightIcon data-icon="inline-end" /></Button>)}</SettingsGroup> : <p className="dl-settings-caption">{t("providers_settings.model_required")}</p>}
      </SettingsSection>
      {names.length ? <FieldGroup>
        <Picker id="dl-config-main" label={t("composer.default_model")} value={config.main_model ?? ""} disabled={pending} options={names.map(name => ({value: name, label: name}))} onChange={main_model => onChange({...config, main_model})} />
        <Picker id="dl-config-reviewer" label={t("providers_settings.auto_approval_model")} value={config.approval_model || "default"} disabled={pending} options={[{value: "default", label: t("providers_settings.default_model")}, ...names.map(name => ({value: name, label: name}))]} onChange={value => onChange({...config, approval_model: value === "default" ? null : value})} />
      </FieldGroup> : null}
    </> : page.type === "provider" && provider ? <>
      <ProviderEditor key={page.index} provider={provider} onChange={editProvider} disabled={pending} />
      {!page.adding && <Button variant="outline" disabled={pending || !provider.name.trim()} onClick={() => addModel(provider.name)}><PlusIcon data-icon="inline-start" />{t("providers_settings.add_model")}</Button>}
    </> : page.type === "model" && model ? <>
      <FieldGroup><Picker id="dl-model-provider" label={t("providers_settings.providers")} value={model.provider_name} disabled={pending} options={config.providers.filter(provider => provider.name).map(provider => ({value: provider.name, label: provider.name}))} onChange={provider_name => {editModel({...model, name: model.name === model.model_id ? "" : model.name, provider_name, model_id: "", context_window_tokens: null, max_completion_tokens: null, supports_vision: null, thinking_budget: null});setPage({...page, choosing: true});}} /></FieldGroup>
      {page.choosing ? <ModelDiscovery key={model.provider_name} state={discovery} disabled={pending} onSelect={selectModel} onCustom={() => setPage({...page, choosing: false})} /> : <ModelEditor key={`${page.index}:${model.provider_name}`} model={model} metadata={discovery.models.find(found => found.id === model.model_id)} onChange={editModel} disabled={pending} onChoose={() => setPage({...page, choosing: true})} />}
    </> : null}
    {(dirty || config.models.length > 0) && <div className="dl-settings-footer"><span className="dl-settings-save-status" role="status">{dirty ? !ready ? t("providers_settings.model_required") : t("settings_dialog.unsaved_changes") : null}</span><Button disabled={pending || !ready || !dirty} onClick={() => void save()}>{pending && <Spinner data-icon="inline-start" />}{t("app.save")}</Button></div>}
  </div>;
}
