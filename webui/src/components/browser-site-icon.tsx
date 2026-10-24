import { useState } from "react";
import { GlobeIcon } from "lucide-react";

export function BrowserSiteIcon({src}:{src?:string|null}){
  const [failed,setFailed]=useState<string|null>(null);
  return src?.startsWith("data:image/png;base64,")&&src.length<48_000&&failed!==src
    ? <img className="dl-browser-favicon" src={src} alt="" draggable={false} onError={()=>setFailed(src)}/>
    : <GlobeIcon/>;
}
