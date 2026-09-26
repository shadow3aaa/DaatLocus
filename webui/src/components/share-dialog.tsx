import { useCallback, useEffect, useRef, useState } from "react";
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
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Spinner } from "@/components/ui/spinner";
import {
  createShare,
  deleteShare,
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
      <ShareDialog
        open={open}
        sessions={sessions}
        initialSessionId={initialSessionId}
        initialUnrestricted={initialUnrestricted}
        onOpenChange={setOpen}
      />
    </>
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
  const [pin, setPin] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [isSubmitting, setIsSubmitting] = useState(false);

  async function submit() {
    if (isSubmitting) {
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
    } finally {
      setIsSubmitting(false);
    }
  }

  return (
    <main className="flex min-h-screen items-center justify-center bg-background p-6 text-foreground">
      <form
        className="w-full max-w-sm space-y-4 rounded-lg border border-border p-6"
        onSubmit={(event) => {
          event.preventDefault();
          void submit();
        }}
      >
        <h1 className="text-lg font-semibold">{t("share.gateTitle")}</h1>
        <p className="text-sm text-muted-foreground">
          {t("share.gateDescription")}
        </p>
        <div className="space-y-2">
          <Label htmlFor="share-pin">{t("share.pinLabel")}</Label>
          <Input
            id="share-pin"
            inputMode="numeric"
            autoComplete="one-time-code"
            maxLength={4}
            value={pin}
            onChange={(event) =>
              setPin(event.target.value.replace(/\D/g, "").slice(0, 4))
            }
            className="text-center text-2xl tracking-[0.5em]"
          />
        </div>
        {error ? (
          <Alert variant="destructive">
            <AlertDescription className="text-xs">{error}</AlertDescription>
          </Alert>
        ) : null}
        <Button type="submit" className="w-full" disabled={pin.length !== 4 || isSubmitting}>
          {isSubmitting ? <Spinner /> : null}
          {t("share.unlock")}
        </Button>
      </form>
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

  const eligibleSessions = sessions;

  useEffect(() => {
    if (!open) {
      setStage("select");
      setSelected([]);
      setUnrestricted(false);
      setShare(null);
      setError(null);
      return;
    }
    setSelected(
      initialSessionId
        ? [initialSessionId]
        : eligibleSessions.map((session) => session.session_id),
    );
    setUnrestricted(initialUnrestricted);
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
            <div className="flex items-center justify-between rounded-md border border-border p-3">
              <div>
                <p className="text-sm font-medium">{t("share.unrestrictedLabel")}</p>
              </div>
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
                <div className="max-h-64 space-y-1 overflow-y-auto">
                  {eligibleSessions.map((session) => {
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
                          "flex w-full items-center justify-between rounded-md border px-3 py-2 text-left text-sm",
                          checked
                            ? "border-primary/60 bg-primary/5"
                            : "border-border",
                        )}
                      >
                        <span className="truncate">
                          {session.title ?? t("share.untitledSession")}
                        </span>
                        {checked ? <CheckIcon className="h-4 w-4" /> : null}
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
    <div className="space-y-4">
      <div className="flex flex-col items-center gap-2">
        <p className="text-xs uppercase tracking-wide text-muted-foreground">
          {t("share.pinHeading")}
        </p>
        <p className="font-mono text-5xl font-semibold tracking-[0.3em]">
          {share.pin}
        </p>
      </div>
      {qrDataUrl ? (
        <div className="flex justify-center">
          <img
            src={qrDataUrl}
            alt={t("share.qrAlt")}
            className="h-48 w-48 rounded-md border border-border bg-white p-2"
          />
        </div>
      ) : null}
      <div className="flex items-center gap-2">
        <Input readOnly value={url ?? ""} className="font-mono text-xs" />
        <Button type="button" size="icon" variant="outline" onClick={() => void copyLink()}>
          {copied ? <CheckIcon className="h-4 w-4" /> : <CopyIcon className="h-4 w-4" />}
        </Button>
      </div>
      <p className="text-center text-xs text-muted-foreground">
        {t("share.countdown", {
          minutes,
          seconds: String(seconds).padStart(2, "0"),
        })}
      </p>
      <DialogFooter>
        <Button type="button" variant="ghost" onClick={onRetry}>
          <RefreshCwIcon className="mr-1 h-4 w-4" />
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
  return (
    <button
      type="button"
      className={cn(
        "inline-flex items-center gap-1 rounded-md px-2 py-1 text-xs text-muted-foreground transition-colors hover:bg-muted/60 hover:text-foreground",
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
