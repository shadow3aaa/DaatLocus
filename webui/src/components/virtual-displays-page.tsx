import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { MonitorIcon, PanelLeftIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Empty, EmptyHeader, EmptyTitle, EmptyMedia } from "@/components/ui/empty";
import { Skeleton } from "@/components/ui/skeleton";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { t } from "@/lib/i18n";
import { observeVirtualDisplays, type DisplayPreview, type VirtualDisplay } from "@/lib/virtual-displays";

const empty: DisplayPreview = { catalog: null, display: null, image: null, failed: false };
export function VirtualDisplaysPage({ onNavigation, onAccessRevoked }: { onNavigation(): void; onAccessRevoked(): void }) {
  useTranslation();
  const [selected, setSelected] = useState<string | null>(null);
  const [preview, setPreview] = useState(empty);
  const [source, setSource] = useState<string | null>(null);
  useEffect(() => {
    let controller: AbortController | undefined;
    function stop() { controller?.abort(); setPreview(old => ({ ...old, image: null })); }
    function visibility() {
      stop();
      if (document.hidden) return;
      controller = new AbortController();
      void observeVirtualDisplays(controller.signal, selected, setPreview, onAccessRevoked);
    }
    visibility();
    document.addEventListener("visibilitychange", visibility);
    window.addEventListener("pageshow", visibility);
    window.addEventListener("pagehide", stop);
    return () => { controller?.abort(); document.removeEventListener("visibilitychange", visibility); window.removeEventListener("pageshow", visibility); window.removeEventListener("pagehide", stop); };
  }, [selected, onAccessRevoked]);
  useEffect(() => {
    const url = preview.image ? URL.createObjectURL(preview.image) : null;
    setSource(url);
    return () => { if (url) URL.revokeObjectURL(url); };
  }, [preview.image]);
  const displays = preview.catalog?.displays ?? [];
  const selectedDisplay = displays.find(display => display.id === selected) ?? preview.display;
  const label = (display: VirtualDisplay) => display.name || t("virtual_displays.display", { number: displays.indexOf(display) + 1 });
  const state = preview.failed ? "unavailable" : preview.catalog?.state;
  return <section className="dl-virtual-displays-page" aria-label={t("virtual_displays.title")}>
    <header className="dl-apps-header dl-virtual-displays-header" data-window-drag-region>
      <div className="dl-apps-title"><Button variant="ghost" size="icon-sm" className="dl-apps-mobile-navigation" aria-label={t("navigation.open_sidebar")} onClick={onNavigation}><PanelLeftIcon /></Button><h1>{t("virtual_displays.title")}</h1></div>
      <div className="dl-virtual-displays-selection">
        {displays.length > 1 && <Select value={selectedDisplay?.id ?? ""} onValueChange={setSelected}><SelectTrigger aria-label={t("virtual_displays.select")}><SelectValue /></SelectTrigger><SelectContent><SelectGroup>{displays.map(display => <SelectItem key={display.id} value={display.id}>{label(display)}</SelectItem>)}</SelectGroup></SelectContent></Select>}
        {selectedDisplay && <span className="dl-virtual-displays-size">{selectedDisplay.width} × {selectedDisplay.height}</span>}
        <span className="dl-virtual-displays-size">{t("virtual_displays.read_only")}</span>
      </div>
    </header>
    <div className="dl-virtual-displays-viewport">
      {source && preview.image ? <img src={source} alt={preview.display ? label(preview.display) : t("virtual_displays.title")} draggable={false} /> : !state ? <Skeleton className="h-full w-full" /> : <Empty><EmptyHeader><EmptyMedia variant="icon"><MonitorIcon /></EmptyMedia><EmptyTitle>{t(`virtual_displays.${state === "ready" ? (preview.display ? "waiting" : "empty") : state}`)}</EmptyTitle></EmptyHeader></Empty>}
    </div>
  </section>;
}
