import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  CheckIcon,
  PanelLeftIcon,
  PlusIcon,
  PuzzleIcon,
  SearchIcon,
  SettingsIcon,
  Trash2Icon,
} from "lucide-react";
import { t } from "@/lib/i18n";
import { ActorApiError } from "@/lib/actor-api";
import {
  getApp,
  installApp,
  listApps,
  setAppEnabled,
  setAppTimeouts,
  cancelAppInstallation,
  defaultAppTimeouts,
  type AppTimeouts,
  type AppDetail,
  type InstalledApp,
} from "@/lib/apps-api";
import { LazyPage } from "@/components/lazy-page";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import {
  Field,
  FieldDescription,
  FieldError,
  FieldGroup,
  FieldLabel,
} from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  InputGroup,
  InputGroupAddon,
  InputGroupInput,
} from "@/components/ui/input-group";
import { Textarea } from "@/components/ui/textarea";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { Spinner } from "@/components/ui/spinner";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Empty,
  EmptyHeader,
  EmptyTitle,
  EmptyDescription,
} from "@/components/ui/empty";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";

function AppIcon({ name }: { name: string }) {
  return (
    <span className="dl-installed-app-icon" aria-hidden="true">
      {Array.from(name.trim())[0]?.toUpperCase() || <PuzzleIcon />}
    </span>
  );
}
function failure(error: unknown) {
  return error instanceof ActorApiError && error.status === 409
    ? t("apps.changed")
    : t("apps.failed");
}

function TimeoutFields({
  value,
  disabled,
  onChange,
  onBlur,
}: {
  value: string[];
  disabled?: boolean;
  onChange(value: string[]): void;
  onBlur?(): void;
}) {
  return (
    <FieldGroup>
      {["connect_timeout", "startup_timeout", "request_timeout"].map(
        (key, index) => (
          <Field key={key}>
            <FieldLabel htmlFor={`app-${key}`}>{t(`apps.${key}`)}</FieldLabel>
            <Input
              id={`app-${key}`}
              type="number"
              min="1"
              step="1"
              value={value[index]}
              placeholder={index ? t("apps.no_deadline") : "30000"}
              disabled={disabled}
              onChange={(event) =>
                onChange(
                  value.map((old, i) =>
                    i === index ? event.target.value : old,
                  ),
                )
              }
              onBlur={onBlur}
            />
          </Field>
        ),
      )}
    </FieldGroup>
  );
}
function timeoutValues(timeouts = defaultAppTimeouts) {
  return [
    String(timeouts.connect_timeout_ms),
    timeouts.startup_timeout_ms?.toString() ?? "",
    timeouts.request_timeout_ms?.toString() ?? "",
  ];
}
function parseTimeouts(values: string[]): AppTimeouts {
  const numbers = values.map((value) => (value.trim() ? Number(value) : null));
  if (
    numbers[0] === null ||
    numbers.some(
      (value) => value !== null && (!Number.isSafeInteger(value) || value < 1),
    )
  )
    throw new Error(t("apps.invalid_timeout"));
  return {
    connect_timeout_ms: numbers[0],
    startup_timeout_ms: numbers[1],
    request_timeout_ms: numbers[2],
  };
}
function TimeoutSettings({
  app,
  onChanged,
  onAccessRevoked,
}: {
  app: InstalledApp;
  onChanged(app: InstalledApp): void;
  onAccessRevoked(): void;
}) {
  const [values, setValues] = useState(() =>
    timeoutValues(app.source.timeouts),
  );
  const [error, setError] = useState("");
  const current = useRef(app);
  const saves = useRef(Promise.resolve());
  useEffect(() => {
    current.current = app;
  }, [app]);
  function save() {
    let timeouts: AppTimeouts;
    try {
      timeouts = parseTimeouts(values);
    } catch (err) {
      setError((err as Error).message);
      return;
    }
    saves.current = saves.current.then(async () => {
      try {
        const next = await setAppTimeouts(current.current, timeouts);
        current.current = next;
        onChanged(next);
        setError("");
      } catch (err) {
        if (err instanceof ActorApiError && err.status === 401)
          onAccessRevoked();
        else setError(failure(err));
      }
    });
  }
  return (
    <>
      <TimeoutFields value={values} onChange={setValues} onBlur={save} />
      {error && <FieldError role="alert">{error}</FieldError>}
    </>
  );
}

export function AppsPage({
  onNavigation,
  onAccessRevoked,
}: {
  onNavigation(): void;
  onAccessRevoked(): void;
}) {
  useTranslation();
  const [apps, setApps] = useState<InstalledApp[]>([]),
    [loading, setLoading] = useState(true),
    [error, setError] = useState("");
  const [cursor, setCursor] = useState<string | null>(null),
    [stdio, setStdio] = useState(false),
    [search, setSearch] = useState(""),
    [filter, setFilter] = useState("all");
  const [adding, setAdding] = useState(false),
    [selected, setSelected] = useState<string | null>(null),
    [managing, setManaging] = useState(false);
  const epoch = useRef(0);
  const [mutating, setMutating] = useState<string | null>(null);
  async function toggleApp(app: InstalledApp) {
    setMutating(app.id);
    try {
      changed(await setAppEnabled(app, !app.enabled));
    } catch (err) {
      if (err instanceof ActorApiError && err.status === 401) onAccessRevoked();
      else setError(failure(err));
    } finally {
      setMutating(null);
    }
  }
  const load = useCallback(
    async (after?: string, signal?: AbortSignal) => {
      const revision = ++epoch.current;
      if (!after) { setLoading(true); setCursor(null); setError(""); }
      try {
        const page = await listApps({
          after,
          signal,
          search,
          enabled: filter === "enabled" ? true : undefined,
        });
        if (revision !== epoch.current || signal?.aborted) return;
        setApps((previous) =>
          after ? [...previous, ...page.items] : page.items,
        );
        setCursor(page.next_cursor);
        setStdio(page.stdio_supported);
      } catch (err) {
        if (signal?.aborted || revision !== epoch.current) return;
        if (after && !(err instanceof ActorApiError && err.status === 401)) throw err;
        if (err instanceof ActorApiError && err.status === 401)
          onAccessRevoked();
        else setError(failure(err));
      } finally {
        if (revision === epoch.current && !signal?.aborted) setLoading(false);
      }
    },
    [onAccessRevoked, search, filter],
  );
  useEffect(() => {
    const controller = new AbortController();
    void load(undefined, controller.signal);
    return () => {
      controller.abort();
      epoch.current++;
    };
  }, [load]);
  const changed = useCallback((app: InstalledApp) => {
    setApps((previous) =>
      [...previous.filter((old) => old.id !== app.id), app].sort((a, b) =>
        a.id.localeCompare(b.id),
      ),
    );
  }, []);
  const shown = apps.filter(
    (app) =>
      (filter !== "enabled" || app.enabled) &&
      `${app.name} ${app.description || ""}`
        .toLocaleLowerCase()
        .includes(search.toLocaleLowerCase()),
  );
  return (
    <section className="dl-apps-page" aria-label={t("apps.title")}>
      <div className="dl-apps-content">
        <header className="dl-apps-header" data-window-drag-region>
          <div className="dl-apps-title">
            <Button
              className="dl-apps-mobile-navigation"
              variant="ghost"
              size="icon-sm"
              aria-label={t("navigation.open_sidebar")}
              onClick={onNavigation}
            >
              <PanelLeftIcon />
            </Button>
            <h1>{t("apps.title")}</h1>
          </div>
          <div className="dl-apps-actions">
            <InputGroup className="dl-apps-search">
              <InputGroupAddon>
                <SearchIcon />
              </InputGroupAddon>
              <InputGroupInput
                aria-label={t("apps.search")}
                placeholder={t("apps.search")}
                value={search}
                onChange={(event) => setSearch(event.target.value)}
              />
            </InputGroup>
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  aria-label={t("apps.manage")}
                  aria-pressed={managing}
                  onClick={() => setManaging((value) => !value)}
                >
                  <SettingsIcon />
                </Button>
              </TooltipTrigger>
              <TooltipContent>{t("apps.manage")}</TooltipContent>
            </Tooltip>
            <Button className="dl-apps-add" onClick={() => setAdding(true)}>
              <PlusIcon data-icon="inline-start" />
              {t("apps.add")}
            </Button>
          </div>
        </header>
        <p className="dl-apps-subtitle">{t("apps.subtitle")}</p>
        <ToggleGroup
          type="single"
          value={filter}
          onValueChange={(value) => value && setFilter(value)}
          className="dl-apps-filters"
          aria-label={t("apps.filters")}
        >
          <ToggleGroupItem value="all">{t("apps.all")}</ToggleGroupItem>
          <ToggleGroupItem value="enabled">{t("apps.enabled")}</ToggleGroupItem>
        </ToggleGroup>
        <h2 className="dl-apps-section-title">
          {managing ? t("apps.manage") : t("apps.installed")}
        </h2>
        {error ? (
          <div className="dl-apps-error" role="alert">
            <span>{error}</span>
            <Button variant="outline" onClick={() => void load()}>
              {t("common.retry")}
            </Button>
          </div>
        ) : (
          <>
            <div className="dl-apps-grid" aria-busy={loading}>
              {shown.map((app) => (
                <article key={app.id} className="dl-installed-app">
                  <button
                    type="button"
                    className="dl-installed-app-main"
                    onClick={() => setSelected(app.id)}
                  >
                    <AppIcon name={app.name} />
                    <span className="dl-installed-app-copy">
                      <span className="dl-installed-app-name">{app.name}</span>
                      <span className="dl-installed-app-description">
                        {app.description ||
                          t("apps.tool_count", { count: app.tool_count })}
                      </span>
                    </span>
                  </button>
                  {managing ? (
                    <Button
                      variant="ghost"
                      size="sm"
                      disabled={mutating !== null}
                      onClick={() => void toggleApp(app)}
                    >
                      {mutating === app.id && (
                        <Spinner data-icon="inline-start" />
                      )}
                      {app.enabled ? t("apps.close") : t("apps.enable")}
                    </Button>
                  ) : (
                    <Tooltip>
                      <TooltipTrigger asChild>
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          aria-label={t("apps.details", { name: app.name })}
                          onClick={() => setSelected(app.id)}
                        >
                          {app.enabled ? <CheckIcon /> : <PlusIcon />}
                        </Button>
                      </TooltipTrigger>
                      <TooltipContent>
                        {app.enabled ? t("apps.connected") : t("apps.disabled")}
                      </TooltipContent>
                    </Tooltip>
                  )}
                </article>
              ))}
              {loading &&
                Array.from({ length: 4 }, (_, i) => (
                  <div className="dl-installed-app dl-app-skeleton" key={i}>
                    <Skeleton className="size-9 rounded-lg" />
                    <div className="flex flex-1 flex-col gap-2">
                      <Skeleton className="h-4 w-24" />
                      <Skeleton className="h-4 w-44 max-w-full" />
                    </div>
                  </div>
                ))}
            </div>
            {!loading && !shown.length && (
              <Empty className="dl-apps-empty">
                <EmptyHeader>
                  <EmptyTitle>
                    {search ? t("apps.no_results") : t("apps.empty")}
                  </EmptyTitle>
                  <EmptyDescription>
                    {search
                      ? t("apps.try_search")
                      : t("apps.empty_description")}
                  </EmptyDescription>
                </EmptyHeader>
                {!search && (
                  <Button variant="outline" onClick={() => setAdding(true)}>
                    <PlusIcon data-icon="inline-start" />
                    {t("apps.add")}
                  </Button>
                )}
              </Empty>
            )}
            {cursor && <LazyPage key={`${search}:${filter}`} cursor={cursor} onLoad={signal => load(cursor, signal)} errorMessage={t("lazy_page.load_failed")} />}

          </>
        )}
      </div>
      <AddAppDialog
        open={adding}
        onOpenChange={setAdding}
        stdio={stdio}
        onInstalled={(app) => {
          changed(app);
          setSelected(app.id);
        }}
        onAccessRevoked={onAccessRevoked}
      />
      <AppDetailDialog
        id={selected}
        onClose={() => setSelected(null)}
        onChanged={changed}
        onAccessRevoked={onAccessRevoked}
      />
    </section>
  );
}

function AddAppDialog({
  open,
  onOpenChange,
  stdio,
  onInstalled,
  onAccessRevoked,
}: {
  open: boolean;
  onOpenChange(open: boolean): void;
  stdio: boolean;
  onInstalled(app: InstalledApp): void;
  onAccessRevoked(): void;
}) {
  const [name, setName] = useState(""),
    [description, setDescription] = useState(""),
    [transport, setTransport] = useState("http"),
    [url, setUrl] = useState(""),
    [auth, setAuth] = useState("none"),
    [token, setToken] = useState("");
  const [command, setCommand] = useState(""),
    [argumentsText, setArguments] = useState(""),
    [environment, setEnvironment] = useState<{ key: string; value: string }[]>(
      [],
    ),
    [pending, setPending] = useState(false),
    [error, setError] = useState("");
  const alive = useRef(true);
  const installation = useRef<string | null>(null);
  const [timeoutInputs, setTimeoutInputs] = useState(() => timeoutValues());
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  async function submit(event: React.FormEvent) {
    event.preventDefault();
    setError("");
    if (!name.trim() || !(transport === "http" ? url.trim() : command.trim())) {
      setError(t("apps.required"));
      return;
    }
    const keys = environment
      .filter((item) => item.key.trim())
      .map((item) => item.key.trim());
    if (new Set(keys).size !== keys.length) {
      setError(t("apps.duplicate_variable"));
      return;
    }
    setPending(true);
    try {
      const timeouts = parseTimeouts(timeoutInputs);
      const requestId = crypto.randomUUID();
      installation.current = requestId;
      const app = await installApp({
        request_id: requestId,
        name: name.trim(),
        description: description.trim(),
        source:
          transport === "http"
            ? {
                transport: "http",
                url: url.trim(),
                bearer_token: auth === "token" ? token : null,
                timeouts,
              }
            : {
                transport: "stdio",
                command: command.trim(),
                timeouts,
                arguments: argumentsText
                  .split(/\r?\n/)
                  .filter((line) => line.length > 0),
                environment: Object.fromEntries(
                  environment
                    .filter((item) => item.key.trim())
                    .map((item) => [item.key.trim(), item.value]),
                ),
              },
      });
      if (!alive.current) return;
      onOpenChange(false);
      setName("");
      setDescription("");
      setUrl("");
      setToken("");
      setCommand("");
      setArguments("");
      setEnvironment([]);
      setTimeoutInputs(timeoutValues());
      onInstalled(app);
    } catch (err) {
      if (!alive.current) return;
      if (err instanceof ActorApiError && err.status === 401) onAccessRevoked();
      else setError(failure(err));
    } finally {
      installation.current = null;
      if (alive.current) setPending(false);
    }
  }
  return (
    <Dialog
      open={open}
      onOpenChange={(value) => {
        if (!pending) {
          onOpenChange(value);
          setError("");
        }
      }}
    >
      <DialogContent
        className="dl-add-app-dialog"
        aria-describedby={undefined}
        onEscapeKeyDown={(event) => {
          if (pending) event.preventDefault();
        }}
      >
        <DialogTitle>{t("apps.add_mcp")}</DialogTitle>
        <form
          className="dl-add-app-form"
          onSubmit={(event) => void submit(event)}
        >
          <FieldGroup>
            <Field>
              <FieldLabel htmlFor="app-name">{t("apps.name")}</FieldLabel>
              <Input
                id="app-name"
                placeholder={t("apps.name_placeholder")}
                value={name}
                onChange={(event) => setName(event.target.value)}
                maxLength={100}
                disabled={pending}
                required
              />
            </Field>
            <Field>
              <FieldLabel htmlFor="app-description">
                {t("apps.description")}
              </FieldLabel>
              <Input
                id="app-description"
                placeholder={t("apps.description_placeholder")}
                value={description}
                onChange={(event) => setDescription(event.target.value)}
                maxLength={500}
                disabled={pending}
              />
            </Field>
            <Field>
              <div className="dl-app-connection-label">
                <FieldLabel
                  htmlFor={transport === "http" ? "app-url" : "app-command"}
                >
                  {t("apps.connection")}
                </FieldLabel>
                {stdio && (
                  <ToggleGroup
                    type="single"
                    value={transport}
                    onValueChange={(value) => value && setTransport(value)}
                    disabled={pending}
                    aria-label={t("apps.connection")}
                  >
                    <ToggleGroupItem value="http">
                      {t("apps.server_url")}
                    </ToggleGroupItem>
                    <ToggleGroupItem value="stdio">
                      {t("apps.local_command")}
                    </ToggleGroupItem>
                  </ToggleGroup>
                )}
              </div>
              {transport === "http" ? (
                <Input
                  id="app-url"
                  type="url"
                  placeholder="https://example.com/mcp"
                  value={url}
                  onChange={(event) => setUrl(event.target.value)}
                  disabled={pending}
                  required
                />
              ) : (
                <Input
                  id="app-command"
                  placeholder="/path/to/mcp-server"
                  value={command}
                  onChange={(event) => setCommand(event.target.value)}
                  disabled={pending}
                  required
                />
              )}
            </Field>
            {transport === "http" ? (
              <>
                <Field>
                  <FieldLabel htmlFor="app-auth">
                    {t("apps.authentication")}
                  </FieldLabel>
                  <Select
                    value={auth}
                    onValueChange={setAuth}
                    disabled={pending}
                  >
                    <SelectTrigger id="app-auth">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectGroup>
                        <SelectItem value="none">
                          {t("apps.no_auth")}
                        </SelectItem>
                        <SelectItem value="token">
                          {t("apps.bearer_token")}
                        </SelectItem>
                      </SelectGroup>
                    </SelectContent>
                  </Select>
                </Field>
                {auth === "token" && (
                  <Field>
                    <FieldLabel htmlFor="app-token">
                      {t("apps.bearer_token")}
                    </FieldLabel>
                    <Input
                      id="app-token"
                      type="password"
                      autoComplete="off"
                      value={token}
                      onChange={(event) => setToken(event.target.value)}
                      disabled={pending}
                      required
                    />
                  </Field>
                )}
              </>
            ) : (
              <>
                <Field>
                  <FieldLabel htmlFor="app-arguments">
                    {t("apps.arguments")}
                  </FieldLabel>
                  <Textarea
                    id="app-arguments"
                    rows={3}
                    value={argumentsText}
                    onChange={(event) => setArguments(event.target.value)}
                    disabled={pending}
                  />
                  <FieldDescription>
                    {t("apps.arguments_hint")}
                  </FieldDescription>
                </Field>
                <Field>
                  <FieldLabel>{t("apps.environment")}</FieldLabel>
                  {environment.map((item, index) => (
                    <div className="dl-app-environment-row" key={index}>
                      <Input
                        aria-label={t("apps.variable_name")}
                        placeholder={t("apps.variable_name")}
                        value={item.key}
                        onChange={(event) =>
                          setEnvironment((previous) =>
                            previous.map((old, i) =>
                              i === index
                                ? { ...old, key: event.target.value }
                                : old,
                            ),
                          )
                        }
                        disabled={pending}
                      />
                      <Input
                        type="password"
                        autoComplete="off"
                        aria-label={t("apps.variable_value")}
                        placeholder={t("apps.variable_value")}
                        value={item.value}
                        onChange={(event) =>
                          setEnvironment((previous) =>
                            previous.map((old, i) =>
                              i === index
                                ? { ...old, value: event.target.value }
                                : old,
                            ),
                          )
                        }
                        disabled={pending}
                      />
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon-sm"
                        aria-label={t("apps.remove_variable")}
                        onClick={() =>
                          setEnvironment((previous) =>
                            previous.filter((_, i) => i !== index),
                          )
                        }
                        disabled={pending}
                      >
                        <Trash2Icon />
                      </Button>
                    </div>
                  ))}
                  <Button
                    type="button"
                    variant="ghost"
                    className="dl-app-add-variable"
                    onClick={() =>
                      setEnvironment((previous) => [
                        ...previous,
                        { key: "", value: "" },
                      ])
                    }
                    disabled={pending}
                  >
                    <PlusIcon data-icon="inline-start" />
                    {t("apps.add_variable")}
                  </Button>
                </Field>
              </>
            )}
            <TimeoutFields
              value={timeoutInputs}
              onChange={setTimeoutInputs}
              disabled={pending}
            />
          </FieldGroup>
          {error && <FieldError role="alert">{error}</FieldError>}
          <div className="dl-add-app-footer">
            <Button
              type="button"
              variant="outline"
              onClick={async () => {
                if (installation.current) {
                  try {
                    await cancelAppInstallation(installation.current);
                  } catch (err) {
                    setError(failure(err));
                  }
                } else onOpenChange(false);
              }}
            >
              {t("common.cancel")}
            </Button>
            <Button type="submit" disabled={pending}>
              {pending && <Spinner data-icon="inline-start" />}
              {pending ? t("apps.connecting") : t("apps.create")}
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function AppDetailDialog({
  id,
  onClose,
  onChanged,
  onAccessRevoked,
}: {
  id: string | null;
  onClose(): void;
  onChanged(app: InstalledApp): void;
  onAccessRevoked(): void;
}) {
  const [app, setApp] = useState<AppDetail | null>(null),
    [error, setError] = useState(""),
    [pending, setPending] = useState(false),
    [loading, setLoading] = useState(false),
    [retry, setRetry] = useState(0);
  const selected = useRef(id);
  selected.current = id;
  useEffect(() => {
    setPending(false);
    setApp(null);
    setError("");
    if (!id) return;
    const controller = new AbortController();
    setLoading(true);
    void getApp(id, { signal: controller.signal })
      .then((next) => {
        if (!controller.signal.aborted) {
          setApp(next);
          onChanged(next);
        }
      })
      .catch((err) => {
        if (controller.signal.aborted) return;
        if (err instanceof ActorApiError && err.status === 401)
          onAccessRevoked();
        else setError(failure(err));
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });
    return () => controller.abort();
  }, [id, retry, onAccessRevoked, onChanged]);
  async function toggle() {
    if (!app) return;
    setPending(true);
    setError("");
    try {
      const currentId = app.id;
      const next = await setAppEnabled(app, !app.enabled);
      onChanged(next);
      if (selected.current !== currentId) return;
      setApp((old) => (old ? { ...old, ...next } : old));
    } catch (err) {
      if (selected.current !== app.id) return;
      if (err instanceof ActorApiError && err.status === 401) onAccessRevoked();
      else setError(failure(err));
    } finally {
      if (selected.current === app.id) setPending(false);
    }
  }
  return (
    <Dialog
      open={Boolean(id)}
      onOpenChange={(open) => !open && !pending && onClose()}
    >
      <DialogContent
        className="dl-app-detail-dialog"
        aria-describedby={undefined}
      >
        <DialogTitle className="sr-only">
          {app?.name || t("apps.app_details")}
        </DialogTitle>
        {loading ? (
          <div className="dl-app-detail-loading" role="status">
            <Spinner />
            {t("apps.loading")}
          </div>
        ) : app ? (
          <>
            <header className="dl-app-detail-header">
              <AppIcon name={app.name} />
              <div>
                <h2>{app.name}</h2>
                {app.description && <p>{app.description}</p>}
              </div>
            </header>
            <div className="dl-app-detail-actions">
              <Button
                variant={app.enabled ? "outline" : "default"}
                disabled={pending}
                onClick={() => void toggle()}
              >
                {pending ? (
                  <Spinner data-icon="inline-start" />
                ) : app.enabled ? (
                  <CheckIcon data-icon="inline-start" />
                ) : (
                  <PlusIcon data-icon="inline-start" />
                )}
                {app.enabled ? t("apps.close") : t("apps.enable")}
              </Button>
            </div>
            <dl className="dl-app-connection-details">
              <div>
                <dt>{t("apps.connection")}</dt>
                <dd>
                  {app.source.transport === "http"
                    ? app.source.url
                    : app.source.command}
                </dd>
              </div>
            </dl>
            <TimeoutSettings
              key={app.id}
              app={app}
              onAccessRevoked={onAccessRevoked}
              onChanged={(next) => {
                onChanged(next);
                if (selected.current === next.id)
                  setApp((old) => (old ? { ...old, ...next } : old));
              }}
            />
            <section className="dl-app-tools">
              <h3>{t("apps.tools")}</h3>
              {app.tools.map((tool) => (
                <div className="dl-app-tool" key={tool.name}>
                  <h4>{tool.name}</h4>
                  {tool.description && <p>{tool.description}</p>}
                </div>
              ))}
            </section>
          </>
        ) : null}
        {error && (
          <div className="dl-apps-error" role="alert">
            <span>{error}</span>
            <Button
              variant="outline"
              onClick={() => setRetry((value) => value + 1)}
            >
              {t("common.retry")}
            </Button>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
