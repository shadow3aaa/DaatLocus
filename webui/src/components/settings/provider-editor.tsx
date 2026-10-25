import { changeProviderKind, isOpenCodeProvider } from "@/lib/model-settings";
import { t } from "@/lib/i18n";
import { useState } from "react";
import { CheckIcon, CopyIcon, ExternalLinkIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { FieldGroup } from "@/components/ui/field";
import { Spinner } from "@/components/ui/spinner";
import {
  completeProviderAuth, runProviderAuth, startProviderAuth,
  type ProviderAuthStart, type ProviderConfig,
} from "@/lib/actor-api";
import { Choices, errorText, Picker, StatusNote, TextField } from "./controls";

const PROVIDER_KINDS: { value: ProviderConfig["kind"]; label: string }[] = [
  { value: "openai", label: "OpenAI" }, { value: "openai_compatible", label: "OpenAI Compatible" },
  { value: "anthropic_compatible", label: "Anthropic Compatible" }, { value: "openai_codex_oauth", label: "Codex OAuth" },
  { value: "github_copilot", label: "GitHub Copilot" }, { value: "ollama", label: "Ollama" },
  { value: "ollama_cloud", label: "Ollama Cloud" }, { value: "open_code_console", label: "OpenCode" },
  { value: "open_code_go", label: "OpenCode Go" },
];

export function providerKindLabel(kind: ProviderConfig["kind"]) {
  if(kind === "openai_compatible") return t("provider_editor.openai_compatible");
  if(kind === "anthropic_compatible") return t("provider_editor.anthropic_compatible");
  if(kind === "ollama_cloud") return t("provider_editor.ollama_cloud");
  if(kind === "openai_codex_oauth") return t("provider_editor.codex_oauth");
  return PROVIDER_KINDS.find(option=>option.value===kind)?.label ?? kind;
}

export function ProviderEditor({ provider, onChange, disabled }: {
  provider: ProviderConfig; onChange: (provider: ProviderConfig) => void;
  disabled?: boolean;
}) {
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [flow, setFlow] = useState<ProviderAuthStart | null>(null);
  const [copied, setCopied] = useState(false);
  const locked = disabled || Boolean(busy);
  const codex = provider.kind === "openai_codex_oauth";
  const github = provider.kind === "github_copilot";
  const opencode = isOpenCodeProvider(provider.kind);
  const method = codex ? (provider.codex_auth_method ?? "browser_login") : github ? (provider.github_auth_method ?? "manual_token") : opencode ? (provider.opencode_auth_method ?? "device_login") : "api_key";
  const methods = codex ? [
    { value: "browser_login", label: t("provider_editor.browser_sign_in") },
    { value: "device_login", label: t("provider_editor.device_code") },
    { value: "import_local_codex", label: t("provider_editor.import_local_codex") },
    { value: "import_auth_file", label: t("provider_editor.import_auth_json") },
    { value: "existing_auth_file", label: t("provider_editor.existing_auth_file") },
  ] : github ? [
    { value: "device_login", label: t("provider_editor.device_code") },
    { value: "manual_token", label: t("provider_editor.access_token") },
    { value: "env_token", label: t("provider_editor.environment_variable") },
  ] : [
    { value: "device_login", label: t("provider_editor.device_code") },
    { value: "existing_auth_file", label: t("provider_editor.existing_auth_file") },
  ];

  function edit(change: Partial<ProviderConfig>) {
    setFlow(null); setNotice(null); setError(null);
    onChange({ ...provider, ...change });
  }
  async function authorize(complete = false) {
    setBusy("auth"); setError(null); setNotice(null);
    try {
      if (method === "device_login" && !complete) {
        setFlow(await startProviderAuth(provider));
      } else {
        const result = complete && flow ? await completeProviderAuth(provider, flow.flow_id) : await runProviderAuth(provider);
        onChange({ ...provider,
          ...(result.api_key ? { api_key: result.api_key, credential_configured: true } : {}),
          ...(opencode && result.auth_file ? { opencode_auth_method: "existing_auth_file" as const, credential_configured: true } : {}),
          ...(codex && result.auth_file ? { codex_auth_file: result.auth_file, codex_auth_method: "existing_auth_file" } : {}),
        });
        setFlow(null); setNotice( t("provider_editor.authorized"));
      }
    } catch (cause) { setError(errorText(cause)); }
    finally { setBusy(null); }
  }
  async function copyCode() {
    try { await navigator.clipboard.writeText(flow!.user_code); setCopied(true); }
    catch (cause) { setError(errorText(cause)); }
  }

  const showKey = !codex && !opencode && method !== "device_login";
  const baseRequired = provider.kind === "openai_compatible" || provider.kind === "anthropic_compatible";
  return <div className="dl-settings-editor">
    <FieldGroup>
      <TextField id="dl-provider-name" label={t("account_editor.name")} value={provider.name} disabled={locked} onChange={(event) => edit({ name: event.target.value })} />
      <Picker id="dl-provider-kind" label={t("provider_editor.provider_type")} value={provider.kind} options={PROVIDER_KINDS.map(option => ({...option,label: providerKindLabel(option.value)}))} disabled={locked}
        onChange={(value) => edit(changeProviderKind(provider, value as ProviderConfig["kind"]))} />
      {(codex || github || opencode) ? <Choices id="dl-provider-auth-method" label={t("provider_editor.authentication")} value={method} options={methods} disabled={locked}
        onChange={(value) => edit(codex ? { codex_auth_method: value as ProviderConfig["codex_auth_method"] } : github ? { github_auth_method: value as ProviderConfig["github_auth_method"] } : { opencode_auth_method: value as ProviderConfig["opencode_auth_method"] })} /> : null}
      {!github ? <TextField id="dl-provider-url" label={t("provider_editor.base_url")} type="url" required={baseRequired} value={provider.base_url ?? ""} disabled={locked} placeholder={opencode ? provider.kind === "open_code_go" ? "https://opencode.ai/inference/go/openai/v1" : "https://opencode.ai/inference/openai/v1" : baseRequired ? "https://…" : t("model_editor.default")} onChange={(event) => edit({ base_url: event.target.value || null })} /> : null}
      {showKey ? <TextField id="dl-provider-key" label={method === "env_token" ? t("provider_editor.token_environment_variable") : github ? t("provider_editor.access_token") : t("provider_editor.api_key")}
        type={method === "env_token" ? "text" : "password"} autoComplete="off" value={provider.api_key ?? ""} disabled={locked}
        placeholder={provider.credential_configured ? t("provider_editor.configured_leave_blank_to_keep") : method === "env_token" ? "${TOKEN_VARIABLE}" : ""}
        onChange={(event) => edit({ api_key: event.target.value || null })} /> : null}
      {codex && ["import_auth_file", "existing_auth_file"].includes(method) ? <TextField id="dl-provider-auth-file" label={t("provider_editor.auth_file_path")} value={provider.codex_auth_file ?? ""} disabled={locked} onChange={(event) => edit({ codex_auth_file: event.target.value || null })} /> : null}
      {provider.kind === "ollama" ? <TextField id="dl-provider-keep-alive" label={t("provider_editor.keep_alive")} value={provider.keep_alive ?? ""} disabled={locked} onChange={(event) => edit({ keep_alive: event.target.value || null })} /> : null}
      {(codex || opencode || (github && method === "device_login")) ? <div className="dl-settings-actions"><Button type="button" variant="outline" disabled={locked} onClick={() => void authorize()}>{busy === "auth" ? <Spinner data-icon="inline-start" /> : <ExternalLinkIcon data-icon="inline-start" />}{method === "device_login" ? t("provider_editor.start_authorization") : method === "browser_login" ? t("provider_editor.sign_in_with_browser") : t("provider_editor.read_auth_file")}</Button></div> : null}
      <StatusNote text={error} error /><StatusNote text={notice} />
      {flow ? <div className="dl-settings-auth-flow">
        <div className="flex items-center gap-2"><code>{flow.user_code}</code><Button type="button" variant="ghost" size="icon-sm" aria-label={t("provider_editor.copy_device_code")} onClick={() => void copyCode()}>{copied ? <CheckIcon /> : <CopyIcon />}</Button></div>
        <div className="flex flex-wrap gap-2"><Button asChild variant="outline"><a href={flow.verification_url} target="_blank" rel="noreferrer"><ExternalLinkIcon data-icon="inline-start" />{t("provider_editor.open_authorization_page")}</a></Button>
          <Button type="button" disabled={locked || Date.now() >= flow.expires_at_ms} onClick={() => void authorize(true)}>{t("provider_editor.i_have_authorized")}</Button></div>
      </div> : null}
    </FieldGroup>
  </div>;
}
