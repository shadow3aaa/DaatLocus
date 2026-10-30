import { useState } from "react";
import { resourceSource } from "@/lib/transport";
import { t } from "@/lib/i18n";
import { Dialog, DialogContent, DialogTitle, DialogTrigger } from "./ui/dialog";

/** Images remain in the authorized WebView, including the full-size overlay. */
export function ResourceImage({uri,name}:{uri:string;name:string}) {
  const [failed,setFailed]=useState(false);
  if(failed)return <span className="dl-tool-note">{t("tool_details.image_preview_unavailable")}</span>;
  return <Dialog><DialogTrigger asChild><button type="button" className="dl-image-thumbnail" aria-label={t("tool_details.open_value0",{value0:name})}>
    <img src={resourceSource(uri)} alt={name} loading="lazy" onError={()=>setFailed(true)}/>
  </button></DialogTrigger><DialogContent className="dl-image-viewer" aria-describedby={undefined}>
    <DialogTitle className="sr-only">{name}</DialogTitle><img src={resourceSource(uri)} alt={name}/>
  </DialogContent></Dialog>;
}
