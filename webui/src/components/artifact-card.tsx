import { useState } from "react";
import { useTranslation } from "react-i18next";
import {
  DownloadIcon,
  ExternalLinkIcon,
  FileTextIcon,
  ImageIcon,
  Maximize2Icon,
  Minimize2Icon,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { getDashboardAttachmentUrl } from "@/lib/daemon-api";
import {
  artifactUrlHostname,
  isDaemonRelativeUrl,
  resolveArtifactUrl,
  shouldBlockLoopbackArtifactIframe,
  type SessionActivityArtifactData,
} from "@/lib/artifact";
import { cn } from "@/lib/utils";

export function AgentChatImageAttachment({
  label,
  uri,
  mimeType,
}: {
  label: string;
  uri: string;
  mimeType: string;
}) {
  const imageUrl = getDashboardAttachmentUrl(uri);
  const title = [label, mimeType].filter(Boolean).join(" · ");

  return (
    <figure className="min-w-0 max-w-[min(28rem,100%)] overflow-hidden rounded-lg border border-border/60 bg-muted/20">
      <a href={imageUrl} target="_blank" rel="noreferrer" className="block">
        <img
          src={imageUrl}
          alt={label}
          title={title || label}
          loading="lazy"
          className="max-h-[22rem] w-full object-contain"
        />
      </a>
    </figure>
  );
}

const ARTIFACT_IFRAME_DEFAULT_HEIGHT_CLASS = "h-72";
const ARTIFACT_IFRAME_EXPANDED_HEIGHT_CLASS = "h-[80vh]";

function artifactDownloadName(artifact: SessionActivityArtifactData): string {
  const fromUri = artifact.uri.split(/[?#]/, 1)[0]?.split("/").filter(Boolean).pop();
  if (fromUri) {
    return decodeURIComponent(fromUri);
  }
  return `${artifact.artifactId}-v${artifact.version}`;
}

function artifactKindIcon(kind: string) {
  if (kind === "image" || kind === "svg") {
    return ImageIcon;
  }
  return FileTextIcon;
}

/**
 * Renders a single agent artifact activity as its own block.
 *
 * Security notes:
 * - `svg` artifacts are only ever rendered through `<img src>`, never inlined.
 * - `html`/`url` artifacts use a sandboxed iframe that deliberately omits
 *   `allow-same-origin` so agent-authored markup cannot reach the WebUI origin.
 * - File-backed `/artifacts/...` URLs are self-authorizing through their `sig`
 *   query parameter, so the daemon token is never appended to them (unlike
 *   `/dashboard/attachments/...` URLs, which still go through
 *   `getDashboardAttachmentUrl`).
 */
export function ArtifactCard({
  artifact,
  isLatest = false,
  hasOlderVersion = false,
  webUiHostname,
}: {
  artifact: SessionActivityArtifactData;
  isLatest?: boolean;
  hasOlderVersion?: boolean;
  webUiHostname?: string | null;
}) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  const title = artifact.title.trim() || t("chat.artifactUntitled");
  const KindIcon = artifactKindIcon(artifact.kind);
  const baseHref =
    typeof window !== "undefined" ? window.location.href : undefined;
  // Default the WebUI host from the browser so loopback/remote detection works
  // at every call site (remote/tunnel access must not embed loopback URLs).
  const webUiHost =
    webUiHostname ??
    (typeof window !== "undefined" ? window.location.hostname : null);
  const openUrl = resolveArtifactUrl(artifact.uri, baseHref);
  const isFileBacked = isDaemonRelativeUrl(artifact.uri);
  const canEmbedIframe =
    (artifact.kind === "html" || artifact.kind === "url") &&
    Boolean(openUrl) &&
    !shouldBlockLoopbackArtifactIframe({ url: openUrl, webUiHostname: webUiHost });
  const isLoopbackBlocked =
    artifact.kind === "url" &&
    Boolean(openUrl) &&
    shouldBlockLoopbackArtifactIframe({ url: openUrl, webUiHostname: webUiHost });
  const iframeTitle = t("chat.artifactIframeTitle", { title });
  const showVersion = artifact.version > 1 || hasOlderVersion;
  const showLatest = isLatest && hasOlderVersion;
  const showDescription = artifact.kind === "html" || artifact.kind === "url";

  return (
    <section
      data-agent-artifact-id={artifact.artifactId}
      data-agent-artifact-kind={artifact.kind}
      className="mx-2 flex min-w-0 max-w-full flex-col gap-3 rounded-xl border border-border/60 bg-muted/20 p-3 sm:mx-3"
    >
      <header className="flex min-w-0 flex-wrap items-center gap-2">
        <span className="inline-flex shrink-0 items-center text-muted-foreground">
          <KindIcon aria-hidden="true" className="size-4" />
        </span>
        <span className="min-w-0 flex-1 break-words text-sm font-semibold text-foreground">
          {title}
        </span>
        {showVersion ? (
          <Badge variant="secondary">v{artifact.version}</Badge>
        ) : null}
        {showLatest ? (
          <Badge variant="default">{t("chat.artifactLatest")}</Badge>
        ) : null}
        <div className="ml-auto flex shrink-0 items-center gap-1">
          {openUrl ? (
            <Button asChild variant="ghost" size="icon-sm">
              <a
                href={openUrl}
                target="_blank"
                rel="noreferrer"
                aria-label={t("chat.artifactOpenInNewTab", { title })}
                title={t("chat.artifactOpenInNewTab", { title })}
              >
                <ExternalLinkIcon aria-hidden="true" />
              </a>
            </Button>
          ) : null}
          {isFileBacked && openUrl ? (
            <Button asChild variant="ghost" size="icon-sm">
              <a
                href={openUrl}
                download={artifactDownloadName(artifact)}
                aria-label={t("chat.artifactDownload", { title })}
                title={t("chat.artifactDownload", { title })}
              >
                <DownloadIcon aria-hidden="true" />
              </a>
            </Button>
          ) : null}
        </div>
      </header>

      {showDescription && artifact.description ? (
        <p className="min-w-0 break-words text-xs leading-5 text-muted-foreground">
          {artifact.description}
        </p>
      ) : null}

      {artifact.kind === "image" || artifact.kind === "svg" ? (
        openUrl ? (
          <AgentChatImageAttachment
            label={title}
            uri={artifact.uri}
            mimeType={artifact.mimeType}
          />
        ) : null
      ) : null}

      {artifact.kind === "html" || artifact.kind === "url" ? (
        isLoopbackBlocked ? (
          <div className="flex min-w-0 flex-col gap-2 rounded-lg border border-amber-500/40 bg-amber-500/10 p-3 text-xs leading-5 text-amber-900 dark:text-amber-200">
            <p className="break-words">
              {t("chat.artifactLoopbackNotice", {
                host: artifactUrlHostname(openUrl) ?? "",
              })}
            </p>
            {openUrl ? (
              <a
                href={openUrl}
                target="_blank"
                rel="noreferrer"
                className="break-all font-medium text-primary underline-offset-4 hover:underline"
              >
                {openUrl}
              </a>
            ) : null}
          </div>
        ) : canEmbedIframe ? (
          <div className="flex min-w-0 flex-col gap-2">
            <div className="flex min-w-0 items-center gap-1">
              <Button
                type="button"
                variant="ghost"
                size="xs"
                onClick={() => setExpanded((current) => !current)}
              >
                {expanded ? (
                  <Minimize2Icon data-icon="inline-start" aria-hidden="true" />
                ) : (
                  <Maximize2Icon data-icon="inline-start" aria-hidden="true" />
                )}
                {expanded
                  ? t("chat.artifactCollapse")
                  : t("chat.artifactExpand")}
              </Button>
              {artifact.kind === "url" && openUrl ? (
                <Button asChild variant="secondary" size="xs">
                  <a href={openUrl} target="_blank" rel="noreferrer">
                    <ExternalLinkIcon data-icon="inline-start" aria-hidden="true" />
                    {t("chat.artifactOpenExternal")}
                  </a>
                </Button>
              ) : null}
            </div>
            <iframe
              src={openUrl}
              sandbox="allow-scripts allow-forms allow-modals allow-popups"
              referrerPolicy="no-referrer"
              loading="lazy"
              title={iframeTitle}
              className={cn(
                "w-full min-w-0 rounded-lg border border-border/60 bg-background",
                expanded
                  ? ARTIFACT_IFRAME_EXPANDED_HEIGHT_CLASS
                  : ARTIFACT_IFRAME_DEFAULT_HEIGHT_CLASS,
              )}
            />
          </div>
        ) : null
      ) : null}

      {artifact.kind !== "image" &&
      artifact.kind !== "svg" &&
      artifact.kind !== "html" &&
      artifact.kind !== "url" ? (
        openUrl ? (
          <a
            href={openUrl}
            target="_blank"
            rel="noreferrer"
            className="break-all text-primary underline-offset-4 hover:underline"
          >
            {openUrl}
          </a>
        ) : (
          <p className="break-words text-xs text-muted-foreground">
            {t("chat.artifactUnavailable")}
          </p>
        )
      ) : null}
    </section>
  );
}
