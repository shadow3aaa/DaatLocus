import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { CopyIcon, MinusIcon, SquareIcon, XIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { controlWindow, desktopPlatform, type DesktopPlatform, type WindowAction } from "@/lib/desktop-window";

export function DesktopTitlebar({ platform = desktopPlatform() }: { platform?: DesktopPlatform | null }) {
  const { t } = useTranslation();
  const [maximized, setMaximized] = useState(false);
  useEffect(() => {
    if (!platform) return;
    let live = true;
    let timer: ReturnType<typeof setTimeout>;
    const update = () => { clearTimeout(timer); timer = setTimeout(() => { void controlWindow("state").then(state => { if (live) setMaximized(state.maximized); }).catch(() => {}); }, 80); };
    update(); window.addEventListener("resize", update);
    return () => { live = false; clearTimeout(timer); window.removeEventListener("resize", update); };
  }, [platform]);
  useEffect(() => {
    if (!platform) return;
    function drag(event: globalThis.MouseEvent) {
      if (event.defaultPrevented || event.button !== 0 || !(event.target instanceof Element)) return;
      const bar = document.querySelector(".dl-window-titlebar")?.getBoundingClientRect();
      const inTitlebar = bar && event.clientY >= bar.top && event.clientY < bar.bottom;
      if ((!inTitlebar && !event.target.closest("[data-window-drag-region]")) || event.target.closest("button,a,input,textarea,select,label,[role=button],[role=combobox],[contenteditable],[data-slot=input-group],[data-window-no-drag]")) return;
      event.preventDefault();
      void controlWindow(event.detail === 2 ? "toggle_maximize" : "drag").catch(() => {});
    }
    window.addEventListener("mousedown", drag);
    return () => window.removeEventListener("mousedown", drag);
  }, [platform]);
  if (!platform) return null;
  function act(action: WindowAction) { void controlWindow(action).then(state => setMaximized(state.maximized)).catch(() => {}); }
  const maximizeLabel = maximized ? t("desktop_window.restore") : t("desktop_window.maximize");
  return <header className="dl-window-titlebar" aria-label={t("desktop_window.window_controls")}>
    <div className="dl-window-drag-area" data-window-drag-region />
    {platform === "windows" && <div className="dl-window-controls">
      <Button variant="ghost" size="icon-sm" aria-label={t("desktop_window.minimize")} title={t("desktop_window.minimize")} onClick={() => act("minimize")}><MinusIcon /></Button>
      <Button variant="ghost" size="icon-sm" aria-label={maximizeLabel} title={maximizeLabel} onClick={() => act("toggle_maximize")}>{maximized ? <CopyIcon /> : <SquareIcon />}</Button>
      <Button variant="ghost" size="icon-sm" aria-label={t("desktop_window.close")} title={t("desktop_window.close")} onClick={() => act("close")}><XIcon /></Button>
    </div>}
  </header>;
}
