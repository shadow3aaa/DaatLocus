import { useState } from "react";
import { resourceSource } from "@/lib/transport";
import { t } from "@/lib/i18n";
import { Dialog, DialogContent, DialogTitle, DialogTrigger } from "./ui/dialog";

type Observation = {title?:string;window_id?:string;width?:number;height?:number;cursor_x?:number;cursor_y?:number;screenshot_url?:string};

/** Enlarged GUI observation with a client-only cursor marker; never dispatches input. */
export function GuiObservationImage({observation}:{observation:Observation}) {
  const [failed,setFailed]=useState(false);
  const uri=observation.screenshot_url;
  const title=observation.title||t("computer_use.observation");
  if(!uri||failed)return <span className="dl-tool-note">{t("tool_details.image_preview_unavailable")}</span>;
  const width=observation.width??0,height=observation.height??0;
  const cursor=Number.isFinite(observation.cursor_x)&&Number.isFinite(observation.cursor_y)&&width>0&&height>0;
  return <Dialog><DialogTrigger asChild><button type="button" className="dl-gui-observation" aria-label={t("tool_details.open_value0",{value0:title})}>
    <span className="dl-gui-observation-meta"><span>{observation.window_id==="desktop"?t("computer_use.desktop_fallback"):t("computer_use.window_view")}</span>{width>0&&height>0&&<span>{width} × {height}</span>}</span>
     <span className="dl-gui-observation-frame"><span className="dl-gui-image-stage"><img src={resourceSource(uri)} alt={title} loading="lazy" onError={()=>setFailed(true)}/>{cursor&&<span className="dl-gui-virtual-cursor" style={{left:`${(observation.cursor_x!/width)*100}%`,top:`${(observation.cursor_y!/height)*100}%`}} aria-hidden="true"><svg viewBox="0 0 30 32"><path d="M5 3.6 26.5 10.8c2.5.8 2.5 2.4 0 3.3L18.5 17c-.8.3-1.3.8-1.6 1.6l-3 7.9c-1 2.6-2.7 2.6-3.6 0L3.1 5.2C2.4 3.2 3.2 3 5 3.6Z"/></svg></span>}</span></span>
  </button></DialogTrigger><DialogContent className="dl-gui-viewer" aria-describedby={undefined}><DialogTitle className="sr-only">{title}</DialogTitle>
    <div className="dl-gui-viewer-meta"><span>{observation.window_id==="desktop"?t("computer_use.desktop_fallback"):t("computer_use.window_view")}</span>{width>0&&height>0&&<span>{width} × {height}</span>}</div>
     <div className="dl-gui-viewer-frame"><span className="dl-gui-image-stage"><img src={resourceSource(uri)} alt={title} onError={()=>setFailed(true)}/>{cursor&&<span className="dl-gui-virtual-cursor" style={{left:`${(observation.cursor_x!/width)*100}%`,top:`${(observation.cursor_y!/height)*100}%`}} aria-hidden="true"><svg viewBox="0 0 30 32"><path d="M5 3.6 26.5 10.8c2.5.8 2.5 2.4 0 3.3L18.5 17c-.8.3-1.3.8-1.6 1.6l-3 7.9c-1 2.6-2.7 2.6-3.6 0L3.1 5.2C2.4 3.2 3.2 3 5 3.6Z"/></svg></span>}</span></div>
  </DialogContent></Dialog>;
}
