import { t } from "@/lib/i18n";
import { useState } from "react";
import { FieldGroup } from "@/components/ui/field";
import { CollapsibleContent, useCollapsibleState } from "@/components/ui/collapsible";
import { Button } from "@/components/ui/button";
import { ChevronDownIcon } from "lucide-react";
import type { DiscoveredModel, ModelConfig } from "@/lib/actor-api";
import { reasoningValues } from "@/lib/model-settings";
import { AreaField, Choices, TextField } from "./controls";
import { SettingsRow } from "./structure";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";

function CatalogField({id, label: title, value, options, onChange, disabled, numeric = false}: {
  id: string; label: string; value: string | number | null | undefined; options: {value: string; label: string}[];
  onChange: (value: string | null) => void; disabled?: boolean; numeric?: boolean;
}) {
  const [custom, setCustom] = useState(false);
  const known = value != null && options.some(option => option.value === String(value));
  const selected = custom || value != null && !known ? "custom" : value == null ? "default" : String(value);
  return <SettingsRow id={id} label={title}>
    <div className="dl-catalog-control">
      <Select value={selected} disabled={disabled} onValueChange={next => {setCustom(next === "custom"); if (next !== "custom") onChange(next === "default" ? null : next);}}>
        <SelectTrigger id={id}><SelectValue /></SelectTrigger>
        <SelectContent><SelectGroup>{[{value: "default", label: t("model_editor.default")}, ...options, {value: "custom", label: t("model_editor.custom")}].map(option => <SelectItem key={option.value} value={option.value}>{option.label}</SelectItem>)}</SelectGroup></SelectContent>
      </Select>
      {selected === "custom" && <Input id={`${id}-custom`} aria-label={t("model_editor.custom_value0", {value0: title})} type={numeric ? "number" : "text"} min={numeric ? 1 : undefined} value={value ?? ""} disabled={disabled} onChange={event => onChange(event.target.value || null)} />}
    </div>
  </SettingsRow>;
}

export function ModelEditor({model, metadata, onChange, disabled, onChoose}: {
  model: ModelConfig; metadata?: DiscoveredModel; onChange: (model: ModelConfig) => void; disabled?: boolean; onChoose: () => void;
}) {
  const advanced = useCollapsibleState(false);
  function edit(change: Partial<ModelConfig>) { onChange({...model, ...change}); }
  const numbers = [
    ["temperature", t("model_editor.temperature")], ["rpm", t("model_editor.requests_per_minute")],
    ["request_timeout_secs", t("model_editor.request_timeout_seconds")],
    ["stream_idle_timeout_secs", t("model_editor.stream_idle_timeout_seconds")],
    ["auto_compact_token_limit", t("model_editor.context_reset_threshold")],
    ["effective_context_window_percent", t("model_editor.usable_context")],
    ["tool_output_max_tokens", t("model_editor.maximum_tool_output_tokens")],
  ] as const;
  const reasoningLabel = (value: string) => ({true: t("model_editor.enabled"), false: t("model_editor.disabled"), none: t("app.none"), minimal: t("model_editor.minimal"), low: t("app.low"), medium: t("app.medium"), high: t("app.high"), xhigh: t("app.extra_high"), max: t("model_editor.maximum")} as Record<string,string>)[value] ?? value;
  return <div className="dl-settings-editor">
    <FieldGroup>
      <TextField id="dl-model-name" label={t("model_editor.configuration_name")} value={model.name} disabled={disabled} onChange={event => edit({name: event.target.value})} />
      <AreaField id="dl-model-capabilities" label={t("model_editor.capabilities")} placeholder={t("model_editor.capabilities_placeholder")} value={model.capabilities ?? ""} disabled={disabled} onChange={event => edit({capabilities: event.target.value})} />
      <TextField id="dl-model-id" label={t("model_editor.model_id")} value={model.model_id} disabled={disabled} onChange={event => edit({model_id: event.target.value, capabilities: event.target.value === model.model_id ? model.capabilities : "", name: !model.name || model.name === model.model_id ? event.target.value : model.name})} />
      <CatalogField id="dl-model-context" label={t("model_editor.context_window")} numeric value={model.context_window_tokens} disabled={disabled} options={metadata?.context_window_tokens ? [{value: String(metadata.context_window_tokens),label: metadata.context_window_tokens.toLocaleString()}] : []} onChange={value => edit({context_window_tokens: value === null ? null : Number(value)})} />
      <CatalogField id="dl-model-output" label={t("model_editor.maximum_output")} numeric value={model.max_completion_tokens} disabled={disabled} options={metadata?.max_completion_tokens ? [{value: String(metadata.max_completion_tokens),label: metadata.max_completion_tokens.toLocaleString()}] : []} onChange={value => edit({max_completion_tokens: value === null ? null : Number(value)})} />
      <CatalogField id="dl-model-thinking" label={t("model_editor.reasoning")} value={model.thinking_budget} options={reasoningValues(metadata).map(value => ({value, label: reasoningLabel(value)}))} disabled={disabled} onChange={value => edit({thinking_budget: value})} />
      <Choices id="dl-model-vision" label={t("model_editor.vision")} value={model.supports_vision == null ? "auto" : model.supports_vision ? "yes" : "no"} disabled={disabled} options={[
        {value: "auto", label: t("model_editor.auto")}, {value: "yes", label: t("model_editor.supported")}, {value: "no", label: t("model_editor.unsupported")},
      ]} onChange={value => edit({supports_vision: value === "auto" ? null : value === "yes"})} />
    </FieldGroup>
    <div className="dl-settings-actions"><Button variant="outline" disabled={disabled} onClick={onChoose}>{t("model_editor.choose_another_model")}</Button></div>
    <div><Button variant="ghost" aria-expanded={advanced.open} aria-controls="dl-model-advanced" onClick={advanced.toggle}>{t("model_editor.advanced")}<ChevronDownIcon data-icon="inline-end" /></Button>
      <CollapsibleContent open={advanced.open} className="dl-settings-advanced"><FieldGroup id="dl-model-advanced">
        {numbers.map(([key, title]) => <TextField key={key} id={`dl-model-${key}`} label={title} type="number" step={key === "temperature" ? "any" : "1"} value={model[key] ?? ""} disabled={disabled} placeholder={t("model_editor.default")} onChange={event => edit({[key]: event.target.value === "" ? null : Number(event.target.value)})} />)}
        <Choices id="dl-model-api-style" label={t("model_editor.api_protocol")} value={model.api_style || "default"} disabled={disabled} options={[
          {value: "default", label: t("model_editor.default")}, {value: "responses", label: "Responses"}, {value: "chat_completions", label: t("model_editor.chat_completions")},
        ]} onChange={value => edit({api_style: value === "default" ? null : value})} />
      </FieldGroup></CollapsibleContent>
    </div>
  </div>;
}
