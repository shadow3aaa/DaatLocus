import { useEffect, useMemo, useState, type ReactNode } from "react";
import {
  BotIcon,
  BrainCircuitIcon,
  CheckCircle2Icon,
  CpuIcon,
  FolderCogIcon,
  KeyRoundIcon,
  LanguagesIcon,
  MessageCircleIcon,
  RefreshCwIcon,
  ServerCogIcon,
  ShieldCheckIcon,
  TriangleAlertIcon,
} from "lucide-react";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Separator } from "@/components/ui/separator";
import {
  fetchSettingsSummary,
  type SettingsCredentialStatus,
  type SettingsCredentialSummary,
  type SettingsModelSummary,
  type SettingsProviderSummary,
  type SettingsSummary,
} from "@/lib/daemon-api";
import { cn } from "@/lib/utils";

const NUMBER_FORMATTER = new Intl.NumberFormat("en-US");

type LoadState = "idle" | "loading" | "error";

type MetricItem = {
  label: string;
  value: ReactNode;
  description?: ReactNode;
};

export function SettingsPage() {
  const [summary, setSummary] = useState<SettingsSummary | null>(null);
  const [loadState, setLoadState] = useState<LoadState>("loading");
  const [loadError, setLoadError] = useState<string | null>(null);

  const providerByName = useMemo(() => {
    const providers = new Map<string, SettingsProviderSummary>();
    for (const provider of summary?.providers ?? []) {
      providers.set(provider.name, provider);
    }
    return providers;
  }, [summary]);

  useEffect(() => {
    const controller = new AbortController();
    void loadSummary(controller.signal);

    return () => controller.abort();
  }, []);

  async function loadSummary(signal?: AbortSignal) {
    setLoadState("loading");
    setLoadError(null);

    try {
      const nextSummary = await fetchSettingsSummary({ signal });
      setSummary(nextSummary);
      setLoadState("idle");
    } catch (error) {
      if (signal?.aborted) {
        return;
      }
      setLoadState("error");
      setLoadError(error instanceof Error ? error.message : String(error));
    }
  }

  const isLoading = loadState === "loading";

  return (
    <section
      id="settings"
      aria-label="Settings"
      className="min-h-screen w-full px-6 pb-10 pt-20 md:pb-12 md:pt-24"
    >
      <div className="mx-auto flex w-full max-w-7xl flex-col gap-4">
        <div className="flex flex-col gap-4 rounded-2xl border border-border/60 bg-card/70 p-5 shadow-sm backdrop-blur supports-[backdrop-filter]:bg-card/60 md:flex-row md:items-end md:justify-between">
          <div className="space-y-2">
            <div className="flex flex-wrap items-center gap-2">
              <Badge variant="outline" className="rounded-full">
                Configuration
              </Badge>
              {summary ? (
                <Badge
                  variant={summary.telegram.has_real_credentials ? "secondary" : "outline"}
                  className="rounded-full"
                >
                  Telegram {summary.telegram.has_real_credentials ? "ready" : "not ready"}
                </Badge>
              ) : null}
            </div>
            <div>
              <h1 className="text-3xl font-semibold tracking-tight md:text-4xl">
                Settings
              </h1>
              <p className="mt-2 max-w-3xl text-sm leading-6 text-muted-foreground">
                Review the active daemon configuration, provider readiness, model
                budgets, runtime services, and integration switches.
              </p>
            </div>
          </div>

          <div className="flex flex-col items-start gap-2 sm:flex-row sm:items-center">
            <div className="text-xs text-muted-foreground">
              {summary ? (
                <>
                  Loaded <time>{formatDateTime(summary.loaded_at_ms)}</time>
                </>
              ) : (
                "Waiting for daemon settings…"
              )}
            </div>
            <Button
              type="button"
              variant="outline"
              onClick={() => void loadSummary()}
              disabled={isLoading}
            >
              <RefreshCwIcon
                className={cn("size-4", isLoading && "animate-spin")}
                aria-hidden="true"
              />
              Refresh
            </Button>
          </div>
        </div>

        {loadError ? (
          <Alert variant="destructive">
            <TriangleAlertIcon className="size-4" aria-hidden="true" />
            <AlertTitle>Unable to load settings</AlertTitle>
            <AlertDescription>{loadError}</AlertDescription>
          </Alert>
        ) : null}

        {summary ? (
          <>
            <div className="grid grid-cols-1 gap-4 lg:grid-cols-[1.15fr_0.85fr]">
              <OverviewCard summary={summary} />
              <RuntimeCard summary={summary} />
            </div>

            <div className="grid grid-cols-1 gap-4 xl:grid-cols-[0.95fr_1.05fr]">
              <ProvidersCard providers={summary.providers} />
              <ModelsCard
                models={summary.models}
                providerByName={providerByName}
              />
            </div>

            <div className="grid grid-cols-1 gap-4 lg:grid-cols-3">
              <JudgeCard summary={summary} />
              <HindsightCard summary={summary} />
              <TelegramCard summary={summary} />
            </div>
          </>
        ) : (
          <SettingsSkeleton />
        )}
      </div>
    </section>
  );
}

function OverviewCard({ summary }: { summary: SettingsSummary }) {
  const mainModel = summary.models.find((model) => model.is_main);

  return (
    <Card className="min-h-full">
      <CardHeader>
        <CardTitle>Overview</CardTitle>
        <CardDescription>
          Active identity and primary model selection.
        </CardDescription>
        <CardAction>
          <LanguagesIcon className="size-4 text-muted-foreground" aria-hidden="true" />
        </CardAction>
      </CardHeader>
      <CardContent className="grid gap-4">
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
          <MetricTile
            icon={<LanguagesIcon className="size-4" aria-hidden="true" />}
            label="Locale"
            value={summary.locale}
            description={summary.locale_label}
          />
          <MetricTile
            icon={<BotIcon className="size-4" aria-hidden="true" />}
            label="Main model"
            value={summary.main_model}
            description={mainModel?.model_id ?? "Model key"}
          />
          <MetricTile
            icon={<KeyRoundIcon className="size-4" aria-hidden="true" />}
            label="Providers"
            value={String(summary.providers.length)}
            description={`${summary.models.length} model definitions`}
          />
        </div>

        <div className="grid gap-3 rounded-xl border bg-muted/20 p-3 text-sm">
          <PathRow label="Home" value={summary.home_path} />
          <PathRow label="Config" value={summary.config_path} />
        </div>
      </CardContent>
    </Card>
  );
}

function RuntimeCard({ summary }: { summary: SettingsSummary }) {
  const portChanged = summary.daemon.configured_port !== summary.daemon.serving_port;

  return (
    <Card className="min-h-full">
      <CardHeader>
        <CardTitle>Runtime</CardTitle>
        <CardDescription>
          Daemon listener and sandbox safety controls.
        </CardDescription>
        <CardAction>
          <ServerCogIcon className="size-4 text-muted-foreground" aria-hidden="true" />
        </CardAction>
      </CardHeader>
      <CardContent className="grid gap-4">
        <div className="grid grid-cols-2 gap-3">
          <MetricTile
            icon={<ServerCogIcon className="size-4" aria-hidden="true" />}
            label="Serving port"
            value={String(summary.daemon.serving_port)}
            description={
              portChanged
                ? `Configured ${summary.daemon.configured_port}`
                : "Matches config"
            }
          />
          <MetricTile
            icon={<ShieldCheckIcon className="size-4" aria-hidden="true" />}
            label="Sandbox"
            value={summary.sandbox.enabled ? "Enabled" : "Disabled"}
            description={`Filesystem ${summary.sandbox.strong_filesystem}`}
          />
        </div>

        <div className="rounded-xl border bg-muted/20 p-3">
          <div className="mb-2 flex items-center justify-between gap-3">
            <span className="text-sm font-medium">Strong filesystem mode</span>
            <Badge variant="outline" className="rounded-full">
              {summary.sandbox.strong_filesystem}
            </Badge>
          </div>
          <p className="text-xs leading-5 text-muted-foreground">
            The sandbox switch controls runtime command isolation. Strong
            filesystem mode indicates whether hardened file access is disabled,
            automatic, or required.
          </p>
        </div>
      </CardContent>
    </Card>
  );
}

function ProvidersCard({ providers }: { providers: SettingsProviderSummary[] }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>Providers</CardTitle>
        <CardDescription>
          Credential readiness without exposing secret values.
        </CardDescription>
        <CardAction>
          <KeyRoundIcon className="size-4 text-muted-foreground" aria-hidden="true" />
        </CardAction>
      </CardHeader>
      <CardContent>
        {providers.length ? (
          <div className="grid gap-3">
            {providers.map((provider) => (
              <ProviderRow key={provider.name} provider={provider} />
            ))}
          </div>
        ) : (
          <EmptyState>No providers configured.</EmptyState>
        )}
      </CardContent>
    </Card>
  );
}

function ProviderRow({ provider }: { provider: SettingsProviderSummary }) {
  return (
    <div className="rounded-xl border bg-muted/15 p-3">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div className="min-w-0 space-y-1">
          <div className="flex flex-wrap items-center gap-2">
            <h3 className="font-medium">{provider.name}</h3>
            <Badge variant="secondary" className="rounded-full">
              {provider.provider_type}
            </Badge>
          </div>
          {provider.base_url ? (
            <p className="break-all text-xs text-muted-foreground">
              {provider.base_url}
            </p>
          ) : provider.auth_file ? (
            <p className="break-all text-xs text-muted-foreground">
              Auth file: {provider.auth_file}
            </p>
          ) : (
            <p className="text-xs text-muted-foreground">No base URL required.</p>
          )}
        </div>
        <CredentialBadge credential={provider.credential} />
      </div>
    </div>
  );
}

function ModelsCard({
  models,
  providerByName,
}: {
  models: SettingsModelSummary[];
  providerByName: Map<string, SettingsProviderSummary>;
}) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>Models</CardTitle>
        <CardDescription>
          Context budget, timeout, and role assignment per model key.
        </CardDescription>
        <CardAction>
          <CpuIcon className="size-4 text-muted-foreground" aria-hidden="true" />
        </CardAction>
      </CardHeader>
      <CardContent>
        {models.length ? (
          <div className="grid gap-3">
            {models.map((model) => (
              <ModelRow
                key={model.name}
                model={model}
                provider={providerByName.get(model.provider) ?? null}
              />
            ))}
          </div>
        ) : (
          <EmptyState>No models configured.</EmptyState>
        )}
      </CardContent>
    </Card>
  );
}

function ModelRow({
  model,
  provider,
}: {
  model: SettingsModelSummary;
  provider: SettingsProviderSummary | null;
}) {
  const roles = [
    model.is_main ? "main" : null,
    model.is_judge ? "judge" : null,
    model.is_hindsight ? "hindsight" : null,
  ].filter(Boolean);

  return (
    <div className="rounded-xl border bg-muted/15 p-3">
      <div className="flex flex-col gap-3">
        <div className="flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between">
          <div className="min-w-0">
            <div className="flex flex-wrap items-center gap-2">
              <h3 className="font-medium">{model.name}</h3>
              {roles.map((role) => (
                <Badge key={role} variant="outline" className="rounded-full">
                  {role}
                </Badge>
              ))}
            </div>
            <p className="mt-1 break-all text-xs text-muted-foreground">
              {model.model_id}
            </p>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <Badge variant="secondary" className="rounded-full">
              {provider?.provider_type ?? model.provider}
            </Badge>
            {model.thinking_budget ? (
              <Badge variant="outline" className="rounded-full">
                thinking {model.thinking_budget}
              </Badge>
            ) : null}
          </div>
        </div>

        <Separator />

        <div className="grid grid-cols-2 gap-3 text-xs md:grid-cols-4">
          <InlineMetric
            label="Context"
            value={formatNumber(model.context_window_tokens)}
            description={`${model.effective_context_window_percent}% effective`}
          />
          <InlineMetric
            label="Auto compact"
            value={formatNumber(model.auto_compact_token_limit)}
            description={`${formatNumber(model.effective_context_window_tokens)} effective`}
          />
          <InlineMetric
            label="Max output"
            value={formatNumber(model.max_completion_tokens)}
            description={`${formatNumber(model.tool_output_max_tokens)} tool output`}
          />
          <InlineMetric
            label="Timeouts"
            value={`${model.request_timeout_secs}s`}
            description={`${model.stream_idle_timeout_secs}s idle`}
          />
        </div>
      </div>
    </div>
  );
}

function JudgeCard({ summary }: { summary: SettingsSummary }) {
  return (
    <ConfigCard
      icon={<BrainCircuitIcon className="size-4 text-muted-foreground" aria-hidden="true" />}
      title="Judge"
      description="Pairwise evaluation settings."
      badge={summary.judge.enabled ? "Enabled" : "Disabled"}
      badgeVariant={summary.judge.enabled ? "secondary" : "outline"}
      items={[
        {
          label: "Effective model",
          value: summary.judge.effective_model,
          description: summary.judge.model ? "Configured explicitly" : "Falls back to main model",
        },
        {
          label: "Candidates",
          value: formatNumber(summary.judge.max_pairwise_candidates),
          description: "Maximum pairwise candidates",
        },
        {
          label: "Cases",
          value: formatNumber(summary.judge.max_pairwise_cases),
          description: "Maximum pairwise cases",
        },
      ]}
    />
  );
}

function HindsightCard({ summary }: { summary: SettingsSummary }) {
  return (
    <ConfigCard
      icon={<FolderCogIcon className="size-4 text-muted-foreground" aria-hidden="true" />}
      title="Hindsight"
      description="Memory sidecar and reflection profile."
      badge={`:${summary.hindsight.port}`}
      badgeVariant="outline"
      items={[
        {
          label: "Profile",
          value: summary.hindsight.profile,
          description: `${summary.hindsight.namespace}/${summary.hindsight.bank_id}`,
        },
        {
          label: "Effective model",
          value: summary.hindsight.effective_model,
          description: summary.hindsight.model
            ? "Configured explicitly"
            : "Falls back to main model",
        },
        {
          label: "Request timeout",
          value: `${summary.hindsight.request_timeout_secs}s`,
          description: "Sidecar operation budget",
        },
      ]}
    />
  );
}

function TelegramCard({ summary }: { summary: SettingsSummary }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>Telegram</CardTitle>
        <CardDescription>
          Remote delivery and polling integration.
        </CardDescription>
        <CardAction>
          <MessageCircleIcon className="size-4 text-muted-foreground" aria-hidden="true" />
        </CardAction>
      </CardHeader>
      <CardContent className="grid gap-4">
        <div className="flex flex-wrap items-center gap-2">
          <Badge
            variant={summary.telegram.enabled ? "secondary" : "outline"}
            className="rounded-full"
          >
            {summary.telegram.enabled ? "Enabled" : "Disabled"}
          </Badge>
          <CredentialBadge credential={summary.telegram.credential} />
        </div>

        <div className="grid gap-3 text-sm">
          <InlineMetric
            label="Poll timeout"
            value={`${summary.telegram.poll_timeout_secs}s`}
            description="Long-poll request duration"
          />
          <InlineMetric
            label="Credential state"
            value={credentialStatusLabel(summary.telegram.credential.status)}
            description={
              summary.telegram.has_real_credentials
                ? "Ready for real Telegram polling"
                : "Token is missing or still a placeholder"
            }
          />
        </div>
      </CardContent>
    </Card>
  );
}

function ConfigCard({
  icon,
  title,
  description,
  badge,
  badgeVariant,
  items,
}: {
  icon: ReactNode;
  title: string;
  description: string;
  badge: string;
  badgeVariant: "outline" | "secondary";
  items: MetricItem[];
}) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>{title}</CardTitle>
        <CardDescription>{description}</CardDescription>
        <CardAction className="flex items-center gap-2">
          <Badge variant={badgeVariant} className="rounded-full">
            {badge}
          </Badge>
          {icon}
        </CardAction>
      </CardHeader>
      <CardContent className="grid gap-3 text-sm">
        {items.map((item) => (
          <InlineMetric
            key={item.label}
            label={item.label}
            value={item.value}
            description={item.description}
          />
        ))}
      </CardContent>
    </Card>
  );
}

function MetricTile({
  icon,
  label,
  value,
  description,
}: {
  icon: ReactNode;
  label: string;
  value: ReactNode;
  description?: ReactNode;
}) {
  return (
    <div className="rounded-xl border bg-muted/20 p-3">
      <div className="mb-3 flex items-center justify-between gap-3">
        <span className="text-xs uppercase tracking-wide text-muted-foreground">
          {label}
        </span>
        <span className="text-muted-foreground">{icon}</span>
      </div>
      <div className="truncate text-lg font-semibold leading-none">{value}</div>
      {description ? (
        <div className="mt-2 truncate text-xs text-muted-foreground">
          {description}
        </div>
      ) : null}
    </div>
  );
}

function InlineMetric({
  label,
  value,
  description,
}: {
  label: string;
  value: ReactNode;
  description?: ReactNode;
}) {
  return (
    <div className="min-w-0 rounded-lg bg-muted/20 p-3">
      <div className="text-xs uppercase tracking-wide text-muted-foreground">
        {label}
      </div>
      <div className="mt-1 truncate font-medium">{value}</div>
      {description ? (
        <div className="mt-1 truncate text-xs text-muted-foreground">
          {description}
        </div>
      ) : null}
    </div>
  );
}

function PathRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="grid gap-1 sm:grid-cols-[5rem_1fr] sm:gap-3">
      <div className="text-xs uppercase tracking-wide text-muted-foreground">
        {label}
      </div>
      <div className="break-all font-mono text-xs">{value}</div>
    </div>
  );
}

function CredentialBadge({
  credential,
}: {
  credential: SettingsCredentialSummary;
}) {
  const tone = credentialTone(credential.status);
  const Icon = credentialIcon(credential.status);

  return (
    <Badge
      variant={tone === "good" ? "secondary" : "outline"}
      className={cn(
        "rounded-full",
        tone === "warn" && "border-destructive/40 text-destructive",
      )}
      title={credential.source ? `Source: ${credential.source}` : undefined}
    >
      <Icon className="size-3" aria-hidden="true" />
      {credentialStatusLabel(credential.status)}
    </Badge>
  );
}

function EmptyState({ children }: { children: ReactNode }) {
  return (
    <div className="rounded-xl border border-dashed bg-muted/10 p-6 text-center text-sm text-muted-foreground">
      {children}
    </div>
  );
}

function SettingsSkeleton() {
  return (
    <div className="grid gap-4">
      <Card>
        <CardContent className="grid gap-3 pt-2">
          <div className="h-5 w-40 animate-pulse rounded bg-muted" />
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
            <div className="h-24 animate-pulse rounded-xl bg-muted" />
            <div className="h-24 animate-pulse rounded-xl bg-muted" />
            <div className="h-24 animate-pulse rounded-xl bg-muted" />
          </div>
        </CardContent>
      </Card>
      <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
        <div className="h-56 animate-pulse rounded-xl bg-muted" />
        <div className="h-56 animate-pulse rounded-xl bg-muted" />
      </div>
    </div>
  );
}

function credentialTone(status: SettingsCredentialStatus) {
  switch (status) {
    case "configured":
    case "env_configured":
    case "oauth_file":
      return "good";
    case "env_missing":
    case "missing":
    case "placeholder":
      return "warn";
  }
}

function credentialIcon(status: SettingsCredentialStatus) {
  switch (status) {
    case "configured":
    case "env_configured":
    case "oauth_file":
      return CheckCircle2Icon;
    case "env_missing":
    case "missing":
    case "placeholder":
      return TriangleAlertIcon;
  }
}

function credentialStatusLabel(status: SettingsCredentialStatus) {
  switch (status) {
    case "configured":
      return "Configured";
    case "env_configured":
      return "Env ready";
    case "env_missing":
      return "Env missing";
    case "missing":
      return "Missing";
    case "placeholder":
      return "Placeholder";
    case "oauth_file":
      return "OAuth file";
  }
}

function formatNumber(value: number) {
  return NUMBER_FORMATTER.format(value);
}

function formatDateTime(timestampMs: number) {
  return new Date(timestampMs).toLocaleString(undefined, {
    dateStyle: "medium",
    timeStyle: "medium",
  });
}
