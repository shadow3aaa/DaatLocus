import { useEffect, useState, type ReactNode } from "react";
import { RefreshCwIcon, TriangleAlertIcon } from "lucide-react";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardAction,
  CardContent,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
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
type Tone = "good" | "warn" | "neutral";

type SettingLine = {
  label: string;
  value: ReactNode;
  meta?: ReactNode;
  action?: ReactNode;
  mono?: boolean;
  breakAll?: boolean;
};

export function SettingsPage() {
  const [summary, setSummary] = useState<SettingsSummary | null>(null);
  const [loadState, setLoadState] = useState<LoadState>("loading");
  const [loadError, setLoadError] = useState<string | null>(null);

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
      className="min-h-screen w-full px-6 pb-10 pt-20 md:pb-12 md:pt-8"
    >
      <div className="flex w-full flex-col gap-4">
        {loadError ? (
          <Alert variant="destructive">
            <TriangleAlertIcon className="size-4" aria-hidden="true" />
            <AlertTitle>Unable to load settings</AlertTitle>
            <AlertDescription>{loadError}</AlertDescription>
          </Alert>
        ) : null}

        {summary ? (
          <div className="grid w-full grid-cols-1 items-start gap-4 sm:grid-cols-2 xl:grid-cols-3">
            <CoreCard
              summary={summary}
              isLoading={isLoading}
              onRefresh={() => void loadSummary()}
            />
            <ProvidersCard providers={summary.providers} />
            <ModelsCard models={summary.models} />
          </div>
        ) : (
          <SettingsSkeleton />
        )}
      </div>
    </section>
  );
}

function CoreCard({
  summary,
  isLoading,
  onRefresh,
}: {
  summary: SettingsSummary;
  isLoading: boolean;
  onRefresh: () => void;
}) {
  const portChanged = summary.daemon.configured_port !== summary.daemon.serving_port;

  return (
    <Card className="w-full">
      <CardHeader>
        <CardTitle>Settings</CardTitle>
        <CardAction>
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            aria-label="Refresh settings"
            onClick={onRefresh}
            disabled={isLoading}
          >
            <RefreshCwIcon
              className={cn("size-4", isLoading && "animate-spin")}
              aria-hidden="true"
            />
          </Button>
        </CardAction>
      </CardHeader>
      <CardContent>
        <SettingList
          lines={[
            {
              label: "Main",
              value: summary.main_model,
              action: <BadgeLine labels={modelRoles(summary, summary.main_model)} />,
            },
            {
              label: "Locale",
              value: summary.locale,
              meta: summary.locale_label,
            },
            {
              label: "Daemon",
              value: `${summary.daemon.bind_host}:${summary.daemon.serving_port}`,
              meta: portChanged ? `config :${summary.daemon.configured_port}` : undefined,
            },
            {
              label: "Sandbox",
              value: summary.sandbox.strong_filesystem,
              action: (
                <StatusBadge
                  tone={summary.sandbox.enabled ? "good" : "neutral"}
                  label={summary.sandbox.enabled ? "on" : "off"}
                />
              ),
            },
            {
              label: "Judge",
              value: summary.judge.effective_model,
              action: (
                <StatusBadge
                  tone={summary.judge.enabled ? "good" : "neutral"}
                  label={summary.judge.enabled ? "on" : "off"}
                />
              ),
            },
            {
              label: "Telegram",
              value: `${summary.telegram.poll_timeout_secs}s poll`,
              action: (
                <div className="flex shrink-0 items-center gap-1.5">
                  <StatusBadge
                    tone={summary.telegram.enabled ? "good" : "neutral"}
                    label={summary.telegram.enabled ? "on" : "off"}
                  />
                  <CredentialBadge credential={summary.telegram.credential} />
                </div>
              ),
            },
            {
              label: "Config",
              value: summary.config_path,
              meta: formatDateTime(summary.loaded_at_ms),
              mono: true,
              breakAll: true,
            },
          ]}
        />
      </CardContent>
    </Card>
  );
}

function ProvidersCard({ providers }: { providers: SettingsProviderSummary[] }) {
  return (
    <Card className="w-full">
      <CardHeader>
        <CardTitle>Providers</CardTitle>
        <CardAction>
          <Badge variant="outline" className="rounded-full">
            {providers.length}
          </Badge>
        </CardAction>
      </CardHeader>
      <CardContent>
        {providers.length ? (
          <div className="divide-y divide-border/60">
            {providers.map((provider) => (
              <ProviderLine key={provider.name} provider={provider} />
            ))}
          </div>
        ) : (
          <EmptyState>No providers</EmptyState>
        )}
      </CardContent>
    </Card>
  );
}

function ProviderLine({ provider }: { provider: SettingsProviderSummary }) {
  const endpoint = provider.base_url ?? provider.auth_file;

  return (
    <SettingLineRow
      line={{
        label: provider.provider_type,
        value: provider.name,
        meta: endpoint,
        action: <CredentialBadge credential={provider.credential} />,
        breakAll: Boolean(endpoint),
        mono: Boolean(endpoint),
      }}
    />
  );
}

function ModelsCard({ models }: { models: SettingsModelSummary[] }) {
  return (
    <Card className="w-full sm:col-span-2 xl:col-span-1">
      <CardHeader>
        <CardTitle>Models</CardTitle>
        <CardAction>
          <Badge variant="outline" className="rounded-full">
            {models.length}
          </Badge>
        </CardAction>
      </CardHeader>
      <CardContent>
        {models.length ? (
          <div className="divide-y divide-border/60">
            {models.map((model) => (
              <ModelLine key={model.name} model={model} />
            ))}
          </div>
        ) : (
          <EmptyState>No models</EmptyState>
        )}
      </CardContent>
    </Card>
  );
}

function ModelLine({ model }: { model: SettingsModelSummary }) {
  return (
    <SettingLineRow
      line={{
        label: model.provider,
        value: model.name,
        meta: (
          <>
            <span className="font-mono">{model.model_id}</span>
            <span aria-hidden="true"> · </span>
            <span>{modelFootprint(model)}</span>
          </>
        ),
        action: <BadgeLine labels={modelRolesFromFlags(model)} />,
      }}
    />
  );
}

function SettingList({ lines }: { lines: SettingLine[] }) {
  return (
    <div className="divide-y divide-border/60">
      {lines.map((line) => (
        <SettingLineRow key={`${line.label}-${String(line.value)}`} line={line} />
      ))}
    </div>
  );
}

function SettingLineRow({ line }: { line: SettingLine }) {
  return (
    <div className="flex min-w-0 items-start justify-between gap-3 py-3 first:pt-0 last:pb-0">
      <div className="min-w-0 space-y-1">
        <div className="text-xs uppercase tracking-wide text-muted-foreground">
          {line.label}
        </div>
        <div
          className={cn(
            "truncate text-sm font-medium",
            line.mono && "font-mono text-xs",
            line.breakAll && "whitespace-normal break-all",
          )}
        >
          {line.value}
        </div>
        {line.meta ? (
          <div
            className={cn(
              "truncate text-xs text-muted-foreground",
              line.mono && "font-mono",
              line.breakAll && "whitespace-normal break-all",
            )}
          >
            {line.meta}
          </div>
        ) : null}
      </div>
      {line.action ? <div className="shrink-0">{line.action}</div> : null}
    </div>
  );
}

function BadgeLine({ labels }: { labels: string[] }) {
  if (!labels.length) {
    return null;
  }

  return (
    <div className="flex flex-wrap justify-end gap-1.5">
      {labels.map((label) => (
        <Badge key={label} variant="outline" className="rounded-full">
          {label}
        </Badge>
      ))}
    </div>
  );
}

function StatusBadge({ tone, label }: { tone: Tone; label: string }) {
  return (
    <Badge variant="outline" className={cn("rounded-full", toneClassName(tone))}>
      {label}
    </Badge>
  );
}

function CredentialBadge({
  credential,
}: {
  credential: SettingsCredentialSummary;
}) {
  return (
    <StatusBadge
      tone={credentialTone(credential.status)}
      label={credentialStatusLabel(credential.status)}
    />
  );
}

function EmptyState({ children }: { children: ReactNode }) {
  return <div className="py-6 text-center text-sm text-muted-foreground">{children}</div>;
}

function SettingsSkeleton() {
  return (
    <div className="grid w-full grid-cols-1 items-start gap-4 sm:grid-cols-2 xl:grid-cols-3">
      {Array.from({ length: 3 }, (_, index) => (
        <Card key={index} className="w-full">
          <CardHeader>
            <div className="h-5 w-24 animate-pulse rounded bg-muted" />
          </CardHeader>
          <CardContent className="grid gap-4">
            {Array.from({ length: 6 }, (_, rowIndex) => (
              <div key={rowIndex} className="space-y-2">
                <div className="h-3 w-16 animate-pulse rounded bg-muted" />
                <div className="h-4 w-3/4 animate-pulse rounded bg-muted" />
              </div>
            ))}
          </CardContent>
        </Card>
      ))}
    </div>
  );
}

function modelRoles(summary: SettingsSummary, modelName: string) {
  return [
    summary.main_model === modelName ? "main" : null,
    summary.judge_model === modelName ? "judge" : null,
  ].filter((role): role is string => Boolean(role));
}

function modelRolesFromFlags(model: SettingsModelSummary) {
  return [
    model.is_main ? "main" : null,
    model.is_judge ? "judge" : null,
  ].filter((role): role is string => Boolean(role));
}

function modelFootprint(model: SettingsModelSummary) {
  const context = `${formatNumber(model.context_window_tokens)} ctx`;
  const reserve = `${formatNumber(model.reserved_output_tokens)} reserve`;
  const output = `${formatNumber(model.max_completion_tokens)} out`;
  const timeout = `${model.request_timeout_secs}s`;

  return `${context} · ${reserve} · ${output} · ${timeout}`;
}

function credentialTone(status: SettingsCredentialStatus): Tone {
  switch (status) {
    case "configured":
    case "env_configured":
    case "oauth_file":
      return "good";
    case "env_missing":
    case "missing":
    case "placeholder":
      return "warn";
    default:
      return "neutral";
  }
}

function credentialStatusLabel(status: SettingsCredentialStatus) {
  switch (status) {
    case "configured":
      return "set";
    case "env_configured":
      return "env";
    case "env_missing":
      return "missing env";
    case "missing":
      return "missing";
    case "placeholder":
      return "placeholder";
    case "oauth_file":
      return "oauth";
    default:
      return status;
  }
}

function toneClassName(tone: Tone) {
  switch (tone) {
    case "good":
      return "border-emerald-500/30 text-emerald-600 dark:text-emerald-400";
    case "warn":
      return "border-amber-500/30 text-amber-600 dark:text-amber-400";
    default:
      return "text-muted-foreground";
  }
}

function formatDateTime(value: number) {
  return new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(value));
}

function formatNumber(value: number) {
  return NUMBER_FORMATTER.format(value);
}
