import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import QRCode from "qrcode";
import { useTranslation } from "react-i18next";
import { CheckIcon, CopyIcon, RefreshCwIcon, ShareIcon } from "lucide-react";

import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Spinner } from "@/components/ui/spinner";
import {
  createShare,
  deleteShare,
  ensureStudySession,
  exchangeSharePin,
  fetchShare,
  type SessionInfo,
  type ShareScopeRequest,
  type ShareSummary,
} from "@/lib/daemon-api";
import {
  currentShareId,
  enableShareMode,
  isShareMode,
  SHARE_DIALOG_EVENT,
  stripShareFragment,
  type ShareDialogRequest,
} from "@/lib/share-mode";
import { cn } from "@/lib/utils";

type ShareStage = "select" | "confirm" | "details";

/**
 * Hosts both share surfaces: the owner-side dialog (opened from the sidebar or
 * a message) and the visitor-side PIN gate that turns `#s=<share_id>` into a
 * session cookie.
 */
export function ShareDialogHost({
  sessions,
  onShareUnlocked,
}: {
  sessions: SessionInfo[];
  onShareUnlocked?: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [initialSessionId, setInitialSessionId] = useState<string | null>(null);
  const [initialUnrestricted, setInitialUnrestricted] = useState(false);
  const [shareModeActive, setShareModeActive] = useState(false);

  useEffect(() => {
    function handleRequest(event: Event) {
      const detail = (event as CustomEvent<ShareDialogRequest>).detail;
      setInitialSessionId(detail?.sessionId ?? null);
      setInitialUnrestricted(detail?.unrestricted ?? false);
      setOpen(true);
    }
    window.addEventListener(SHARE_DIALOG_EVENT, handleRequest);
    return () => window.removeEventListener(SHARE_DIALOG_EVENT, handleRequest);
  }, []);

  const pendingShareId = currentShareId();

  return (
    <>
      {!shareModeActive && pendingShareId ? (
        <ShareModeGate
          shareId={pendingShareId}
          onUnlocked={() => {
            setShareModeActive(true);
            onShareUnlocked?.();
          }}
        />
      ) : null}
      {isShareMode() ? null : (
        <ShareDialog
          open={open}
          sessions={sessions}
          initialSessionId={initialSessionId}
          initialUnrestricted={initialUnrestricted}
          onOpenChange={setOpen}
        />
      )}
    </>
  );
}

const PIN_LENGTH = 4;

function emptyPin(): string[] {
  return Array.from({ length: PIN_LENGTH }, () => "");
}

function PinInput({
  digits,
  onChange,
  disabled,
  invalid,
  label,
}: {
  digits: string[];
  onChange: (digits: string[]) => void;
  disabled?: boolean;
  invalid?: boolean;
  label: string;
}) {
  const inputsRef = useRef<Array<HTMLInputElement | null>>([]);

  function focusInput(index: number) {
    inputsRef.current[Math.max(0, Math.min(index, PIN_LENGTH - 1))]?.focus();
  }

  function setDigit(index: number, raw: string) {
    const digit = raw.replace(/\D/g, "").slice(-1);
    const next = digits.slice();
    next[index] = digit;
    onChange(next);
    if (digit) {
      focusInput(index + 1);
    }
  }

  function handlePaste(event: React.ClipboardEvent<HTMLInputElement>) {
    const pasted = event.clipboardData
      .getData("text")
      .replace(/\D/g, "")
      .slice(0, PIN_LENGTH);
    if (!pasted) {
      return;
    }
    event.preventDefault();
    const next = emptyPin();
    for (let index = 0; index < pasted.length; index += 1) {
      next[index] = pasted[index];
    }
    onChange(next);
    focusInput(pasted.length);
  }

  function handleKeyDown(event: React.KeyboardEvent<HTMLInputElement>, index: number) {
    if (event.key === "Backspace") {
      if (digits[index]) {
        return;
      }
      if (index > 0) {
        event.preventDefault();
        const next = digits.slice();
        next[index - 1] = "";
        onChange(next);
        focusInput(index - 1);
      }
      return;
    }
    if (event.key === "ArrowLeft") {
      event.preventDefault();
      focusInput(index - 1);
    }
    if (event.key === "ArrowRight") {
      event.preventDefault();
      focusInput(index + 1);
    }
  }

  return (
    <div role="group" aria-label={label} className="flex gap-2.5">
      {digits.map((digit, index) => (
        <input
          // eslint-disable-next-line react/no-array-index-key
          key={index}
          ref={(node) => {
            inputsRef.current[index] = node;
          }}
          type="text"
          inputMode="numeric"
          autoComplete={index === 0 ? "one-time-code" : "off"}
          maxLength={1}
          value={digit}
          disabled={disabled}
          aria-label={`${label} ${index + 1}`}
          aria-invalid={invalid}
          autoFocus={index === 0}
          onChange={(event) => setDigit(index, event.target.value)}
          onKeyDown={(event) => handleKeyDown(event, index)}
          onPaste={handlePaste}
          onFocus={(event) => event.target.select()}
          className={cn(
            "h-16 min-w-0 flex-1 rounded-sm border border-input bg-background text-center font-mono text-3xl tabular-nums text-foreground outline-none transition-colors",
            "focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50",
            "disabled:pointer-events-none disabled:opacity-50",
            invalid && "border-destructive aria-invalid:ring-destructive/20",
          )}
        />
      ))}
    </div>
  );
}

function ShareModeGate({
  shareId,
  onUnlocked,
}: {
  shareId: string;
  onUnlocked: () => void;
}) {
  const { t } = useTranslation();
  const [digits, setDigits] = useState<string[]>(emptyPin);
  const [error, setError] = useState<string | null>(null);
  const [isSubmitting, setIsSubmitting] = useState(false);
  const pin = digits.join("");
  const pinComplete = pin.length === PIN_LENGTH;

  async function submit() {
    if (isSubmitting || !pinComplete) {
      return;
    }
    setIsSubmitting(true);
    setError(null);
    try {
      await exchangeSharePin({ shareId, pin });
      enableShareMode();
      const nextHash = stripShareFragment(window.location.hash);
      window.history.replaceState(
        null,
        "",
        `${window.location.pathname}${window.location.search}${nextHash}`,
      );
      onUnlocked();
    } catch (submitError) {
      setError(
        submitError instanceof Error ? submitError.message : String(submitError),
      );
      setDigits(emptyPin());
    } finally {
      setIsSubmitting(false);
    }
  }

  return (
    <main className="flex min-h-screen w-full flex-col bg-background text-foreground lg:flex-row">
      <div className="flex flex-col justify-start gap-10 p-8 md:p-12 lg:flex-1 lg:justify-between lg:p-16">
        <span className="text-sm font-semibold tracking-tight">Daat Locus</span>
        <div className="flex flex-col gap-5">
          <span className="font-mono text-[11px] uppercase tracking-[0.18em] text-muted-foreground">
            Shared session
          </span>
          <h1 className="text-4xl font-medium leading-tight tracking-tight md:text-5xl">
            {t("share.gateTitle")}
          </h1>
          <p className="max-w-md text-lg leading-relaxed text-muted-foreground">
            {t("share.gateDescription")}
          </p>
        </div>
        <span className="hidden font-mono text-[11px] uppercase tracking-[0.18em] text-muted-foreground lg:block">
          {t("share.pinLabel")}
        </span>
      </div>

      <div className="flex items-center bg-muted/40 p-8 md:p-12 lg:flex-1 lg:p-16">
        <form
          className="w-full max-w-sm space-y-6"
          onSubmit={(event) => {
            event.preventDefault();
            void submit();
          }}
        >
          <div className="space-y-3">
            <span className="font-mono text-[11px] uppercase tracking-[0.18em] text-muted-foreground">
              {t("share.pinLabel")}
            </span>
            <PinInput
              digits={digits}
              onChange={(next) => {
                setDigits(next);
                setError(null);
              }}
              disabled={isSubmitting}
              invalid={Boolean(error)}
              label={t("share.pinLabel")}
            />
          </div>
          {error ? (
            <Alert variant="destructive">
              <AlertDescription className="text-xs">{error}</AlertDescription>
            </Alert>
          ) : null}
          <Button
            type="submit"
            className="w-full"
            disabled={!pinComplete || isSubmitting}
          >
            {isSubmitting ? <Spinner data-icon="inline-start" /> : null}
            {t("share.unlock")}
          </Button>
        </form>
      </div>
    </main>
  );
}

function ShareDialog({
  open,
  sessions,
  initialSessionId,
  initialUnrestricted,
  onOpenChange,
}: {
  open: boolean;
  sessions: SessionInfo[];
  initialSessionId: string | null;
  initialUnrestricted: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const { t } = useTranslation();
  const [stage, setStage] = useState<ShareStage>("select");
  const [selected, setSelected] = useState<string[]>([]);
  const [unrestricted, setUnrestricted] = useState(false);
  const [share, setShare] = useState<ShareSummary | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [isCreating, setIsCreating] = useState(false);

  const [ensuredStudySession, setEnsuredStudySession] =
    useState<SessionInfo | null>(null);

  // The fixed study session is only created lazily by the Study page. Ensure it
  // exists when the owner opens this dialog so it can be selected and shared.
  useEffect(() => {
    if (!open || isShareMode()) {
      return;
    }
    let cancelled = false;
    void ensureStudySession()
      .then((session) => {
        if (!cancelled) {
          setEnsuredStudySession(session);
        }
      })
      .catch(() => {
        // A study session is optional: keep the plain list if this fails.
      });
    return () => {
      cancelled = true;
    };
  }, [open]);

  const eligibleSessions = useMemo(() => {
    if (
      !ensuredStudySession ||
      sessions.some(
        (session) => session.session_id === ensuredStudySession.session_id,
      )
    ) {
      return sessions;
    }
    return [...sessions, ensuredStudySession];
  }, [sessions, ensuredStudySession]);

  useEffect(() => {
    if (!open) {
      setStage("select");
      setSelected([]);
      setUnrestricted(false);
      setShare(null);
      setError(null);
      return;
    }
    setUnrestricted(initialUnrestricted);
    setSelected((current) => {
      const ids = new Set(eligibleSessions.map((session) => session.session_id));
      const selectionIsUsable =
        current.length > 0 && current.every((id) => ids.has(id));
      if (selectionIsUsable) {
        return current;
      }
      return initialSessionId
        ? [initialSessionId]
        : eligibleSessions.map((session) => session.session_id);
    });
  }, [open, initialSessionId, initialUnrestricted, eligibleSessions]);

  // Poll the share until the tunnel resolves to ready/failed.
  const shareId = share?.share_id ?? null;
  const state = share?.state ?? null;
  useEffect(() => {
    if (stage !== "details" || !shareId || state !== "preparing") {
      return;
    }
    let cancelled = false;
    const timer = window.setInterval(() => {
      void fetchShare({ shareId })
        .then((next) => {
          if (!cancelled) {
            setShare(next);
          }
        })
        .catch((pollError: unknown) => {
          if (!cancelled) {
            setError(
              pollError instanceof Error ? pollError.message : String(pollError),
            );
          }
        });
    }, 1500);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [stage, shareId, state]);

  async function submit() {
    if (isCreating) {
      return;
    }
    setIsCreating(true);
    setError(null);
    const scope: ShareScopeRequest = unrestricted
      ? { kind: "unrestricted" }
      : { kind: "sessions", session_ids: selected };
    try {
      const created = await createShare({ scope });
      const summary = await fetchShare({ shareId: created.share_id });
      setShare(summary);
      setStage("details");
    } catch (createError) {
      setError(
        createError instanceof Error ? createError.message : String(createError),
      );
    } finally {
      setIsCreating(false);
    }
  }

  async function retry() {
    if (!share) {
      return;
    }
    await deleteShare({ shareId: share.share_id });
    setShare(null);
    setStage("select");
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg" aria-describedby={undefined}>
        <DialogHeader>
          <DialogTitle>{t("share.dialogTitle")}</DialogTitle>
        </DialogHeader>

        {error ? (
          <Alert variant="destructive">
            <AlertDescription className="text-xs">{error}</AlertDescription>
          </Alert>
        ) : null}

        {stage === "select" ? (
          <div className="space-y-3">
            <div className="flex items-center justify-between gap-4 rounded-sm border border-border p-3">
              <p className="text-sm font-medium">{t("share.unrestrictedLabel")}</p>
              <Switch checked={unrestricted} onCheckedChange={setUnrestricted} />
            </div>
            {!unrestricted ? (
              <div className="space-y-2">
                <div className="flex items-center justify-between">
                  <p className="text-sm font-medium">
                    {t("share.selectAllLabel")}
                  </p>
                  <Button
                    type="button"
                    size="sm"
                    variant="ghost"
                    onClick={() =>
                      setSelected(
                        selected.length === eligibleSessions.length
                          ? []
                          : eligibleSessions.map((session) => session.session_id),
                      )
                    }
                  >
                    {selected.length === eligibleSessions.length
                      ? t("share.clearAll")
                      : t("share.selectAll")}
                  </Button>
                </div>
                <div className="max-h-64 divide-y divide-border/60 overflow-y-auto rounded-sm border border-border">
                  {eligibleSessions.map((session, index) => {
                    const checked = selected.includes(session.session_id);
                    return (
                      <button
                        key={session.session_id}
                        type="button"
                        onClick={() =>
                          setSelected((current) =>
                            checked
                              ? current.filter((id) => id !== session.session_id)
                              : [...current, session.session_id],
                          )
                        }
                        className={cn(
                          "flex w-full items-center gap-3 px-3 py-2 text-left text-sm transition-colors",
                          checked ? "bg-primary/5" : "hover:bg-muted/50",
                        )}
                      >
                        <span className="font-mono text-[11px] tabular-nums text-muted-foreground">
                          {String(index + 1).padStart(2, "0")}
                        </span>
                        <span className="min-w-0 flex-1 truncate">
                          {session.title ?? t("share.untitledSession")}
                        </span>
                        {checked ? (
                          <CheckIcon
                            className="h-4 w-4 shrink-0 text-foreground"
                            aria-hidden="true"
                          />
                        ) : null}
                      </button>
                    );
                  })}
                </div>
              </div>
            ) : null}
            <DialogFooter>
              <Button
                type="button"
                disabled={!unrestricted && selected.length === 0}
                onClick={() => setStage("confirm")}
              >
                {t("share.next")}
              </Button>
            </DialogFooter>
          </div>
        ) : null}

        {stage === "confirm" ? (
          <div className="space-y-3">
            <Alert variant="destructive">
              <AlertDescription className="text-sm">
                {t("share.confirmStatement")}
              </AlertDescription>
            </Alert>
            <p className="text-xs text-muted-foreground">
              {unrestricted
                ? t("share.confirmUnrestricted")
                : t("share.confirmCount", { count: selected.length })}
            </p>
            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                onClick={() => setStage("select")}
              >
                {t("share.back")}
              </Button>
              <Button type="button" onClick={() => void submit()} disabled={isCreating}>
                {isCreating ? <Spinner /> : null}
                {t("share.create")}
              </Button>
            </DialogFooter>
          </div>
        ) : null}

        {stage === "details" ? (
          <ShareDetails share={share} onRetry={() => void retry()} />
        ) : null}
      </DialogContent>
    </Dialog>
  );
}

function ShareDetails({
  share,
  onRetry,
}: {
  share: ShareSummary | null;
  onRetry: () => void;
}) {
  const { t } = useTranslation();
  const [qrDataUrl, setQrDataUrl] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [remainingMs, setRemainingMs] = useState(0);
  const copyTimer = useRef<number | undefined>(undefined);

  const url = share?.url ?? null;
  const expiresAt = share?.expires_at_ms ?? null;

  useEffect(() => {
    if (!url) {
      setQrDataUrl(null);
      return;
    }
    let cancelled = false;
    void QRCode.toDataURL(url, { margin: 1, width: 192 })
      .then((dataUrl) => {
        if (!cancelled) {
          setQrDataUrl(dataUrl);
        }
      })
      .catch(() => {
        if (!cancelled) {
          setQrDataUrl(null);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [url]);

  useEffect(() => {
    if (!expiresAt) {
      setRemainingMs(0);
      return;
    }
    function tick() {
      setRemainingMs(Math.max(0, expiresAt! - Date.now()));
    }
    tick();
    const timer = window.setInterval(tick, 1000);
    return () => window.clearInterval(timer);
  }, [expiresAt]);

  const copyLink = useCallback(async () => {
    if (!url) {
      return;
    }
    try {
      await navigator.clipboard.writeText(url);
      setCopied(true);
      window.clearTimeout(copyTimer.current);
      copyTimer.current = window.setTimeout(() => setCopied(false), 1600);
    } catch {
      setCopied(false);
    }
  }, [url]);

  useEffect(
    () => () => window.clearTimeout(copyTimer.current),
    [],
  );

  if (!share) {
    return (
      <div className="flex items-center justify-center py-8">
        <Spinner />
      </div>
    );
  }

  if (share.state === "failed") {
    return (
      <div className="space-y-3">
        <Alert variant="destructive">
          <AlertDescription className="text-sm">
            {share.error?.message ?? t("share.failedFallback")}
          </AlertDescription>
        </Alert>
        <p className="text-xs text-muted-foreground">
          {t("share.failedCode", { code: share.error?.code ?? "internal" })}
        </p>
        <DialogFooter>
          <Button type="button" onClick={onRetry}>
            <RefreshCwIcon className="mr-1 h-4 w-4" />
            {t("share.retry")}
          </Button>
        </DialogFooter>
      </div>
    );
  }

  if (share.state !== "ready" || !share.pin) {
    return (
      <div className="flex items-center justify-center gap-2 py-8 text-sm text-muted-foreground">
        <Spinner />
        {t("share.preparing")}
      </div>
    );
  }

  const minutes = Math.floor(remainingMs / 60_000);
  const seconds = Math.floor((remainingMs % 60_000) / 1000);

  return (
    <div className="space-y-5">
      <div className="grid grid-cols-1 gap-5 sm:grid-cols-[auto_1fr]">
        <div className="flex flex-row items-center gap-5 sm:flex-col sm:items-start">
          <div className="flex flex-col gap-2">
            <span className="font-mono text-[11px] uppercase tracking-[0.18em] text-muted-foreground">
              {t("share.pinHeading")}
            </span>
            <span className="font-mono text-4xl font-medium leading-none tracking-[0.3em] tabular-nums">
              {share.pin}
            </span>
          </div>
          {qrDataUrl ? (
            <img
              src={qrDataUrl}
              alt={t("share.qrAlt")}
              className="h-32 w-32 shrink-0 rounded-sm border border-border bg-white p-2"
            />
          ) : null}
        </div>

        <div className="flex flex-col justify-center gap-3">
          <span className="font-mono text-[11px] uppercase tracking-[0.18em] text-muted-foreground">
            link
          </span>
          <div className="flex items-center gap-2">
            <Input readOnly value={url ?? ""} className="font-mono text-xs" />
            <Button
              type="button"
              variant="outline"
              className="shrink-0 gap-1.5"
              onClick={() => void copyLink()}
            >
              {copied ? (
                <CheckIcon className="h-4 w-4" />
              ) : (
                <CopyIcon className="h-4 w-4" />
              )}
              {copied ? "Copied" : "Copy"}
            </Button>
          </div>
          <p className="font-mono text-[11px] uppercase tracking-[0.18em] tabular-nums text-muted-foreground">
            {t("share.countdown", {
              minutes,
              seconds: String(seconds).padStart(2, "0"),
            })}
          </p>
        </div>
      </div>
      <DialogFooter>
        <Button type="button" variant="ghost" onClick={onRetry}>
          <RefreshCwIcon className="h-4 w-4" />
          {t("share.regenerate")}
        </Button>
      </DialogFooter>
    </div>
  );
}

/** Sidebar / message entry: open the share dialog for an optional session. */
export function ShareEntryButton({
  sessionId,
  className,
  label,
  iconOnly = false,
  iconClassName,
  unrestricted = false,
}: {
  sessionId?: string | null;
  className?: string;
  label?: string;
  /** Render only the icon; `label` still supplies the accessible name. */
  iconOnly?: boolean;
  /** Override the icon size/style (defaults to `h-4 w-4`). */
  iconClassName?: string;
  /** Open with the unrestricted scope pre-selected. */
  unrestricted?: boolean;
}) {
  const { t } = useTranslation();

  // A share visitor must never be able to hand out another share.
  if (isShareMode()) {
    return null;
  }
  return (
    <button
      type="button"
      className={cn(
        "relative inline-flex items-center gap-1 rounded-md px-2 py-1 text-xs text-muted-foreground transition-colors after:absolute after:-inset-x-1 after:-inset-y-2 hover:bg-muted/60 hover:text-foreground",
        className,
      )}
      aria-label={label ?? t("share.open")}
      onClick={() => {
        window.dispatchEvent(
          new CustomEvent<ShareDialogRequest>(SHARE_DIALOG_EVENT, {
            detail: { sessionId: sessionId ?? null, unrestricted },
          }),
        );
      }}
    >
      <ShareIcon className={cn("h-4 w-4", iconClassName)} aria-hidden="true" />
      {!iconOnly && label ? <span>{label}</span> : null}
    </button>
  );
}

/**
 * Floating share entry pinned to the top-right of the main session area. Owns
 * the share dialog just like the sidebar entry, but stays visible regardless
 * of whether a session is focused. Hidden for share visitors.
 */
export function ShareFloatingButton({ className }: { className?: string }) {
  if (isShareMode()) {
    return null;
  }
  return (
    <ShareEntryButton
      iconOnly
      unrestricted
      className={cn(
        "fixed right-4 top-4 z-30 h-9 w-9 justify-center rounded-full border border-border bg-background/80 text-muted-foreground shadow-sm backdrop-blur transition-colors hover:bg-muted/60 hover:text-foreground",
        className,
      )}
    />
  );
}
