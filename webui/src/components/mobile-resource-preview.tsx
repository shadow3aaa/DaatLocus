import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { isAndroid } from "@/lib/mobile";
import { resourceActorId } from "@/lib/conversation-links";

/** Resources stay in the authenticated WebView; no cookie or credential goes to another app. */
export function MobileResourcePreview() {
  const { t } = useTranslation();
  const [resource, setResource] = useState<string | null>(null);
  useEffect(() => {
    if (!isAndroid()) return;
    function click(event: MouseEvent) {
      const link = event.target instanceof Element ? event.target.closest("a[href]") : null;
      const href = link?.getAttribute("href");
      if (href && resourceActorId(href, location.origin)) {
        event.preventDefault(); event.stopPropagation();
        setResource(new URL(href, location.origin).href);
      }
    }
    document.addEventListener("click", click, true);
    return () => document.removeEventListener("click", click, true);
  }, []);
  return <Dialog open={resource !== null} onOpenChange={open => { if (!open) setResource(null); }}>
    <DialogContent className="dl-mobile-resource" aria-describedby={undefined}>
      <DialogHeader><DialogTitle>{t("mobile.resource")}</DialogTitle></DialogHeader>
      {resource && <iframe src={resource} sandbox="allow-scripts" title={t("mobile.resource")} />}
    </DialogContent>
  </Dialog>;
}
