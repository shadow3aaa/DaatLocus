import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { isTauri } from "@tauri-apps/api/core";
import { downloadResource, resourceSource } from "@/lib/transport";
import { resourceActorId } from "@/lib/conversation-links";
import { Spinner } from "@/components/ui/spinner";

/** Resources stay in the authenticated WebView; no cookie or credential goes to another app. */
export function NativeResourcePreview() {
  const { t } = useTranslation();
  const [resource, setResource] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    if (!isTauri()) return;
    function click(event: MouseEvent) {
      const link = event.target instanceof Element ? event.target.closest("a[href]") : null;
      const href = link?.getAttribute("href");
      if (href && resourceActorId(href, location.href)) {
        event.preventDefault(); event.stopPropagation();
        setFailed(false);
        setResource(resourceSource(href));
        if (link?.hasAttribute("download")) {
          setSaving(true);
          void downloadResource(href, link.getAttribute("download") || "download")
            .then(() => setResource(null)).catch(() => setFailed(true)).finally(() => setSaving(false));
        }
      }
    }
    document.addEventListener("click", click, true);
    return () => document.removeEventListener("click", click, true);
  }, []);
  return <Dialog open={resource !== null} onOpenChange={open => { if (!open) setResource(null); }}>
    <DialogContent className="dl-mobile-resource" aria-describedby={undefined}>
      <DialogHeader><DialogTitle>{t("mobile.resource")}</DialogTitle></DialogHeader>
      {saving && <Spinner role="status" aria-label={t("mobile.saving")} />}
      {failed && <p role="alert">{t("mobile.save_failed")}</p>}
      {resource && !saving && <iframe src={resource} sandbox="allow-scripts" title={t("mobile.resource")} />}
    </DialogContent>
  </Dialog>;
}
