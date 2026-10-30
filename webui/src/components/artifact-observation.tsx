import { useState } from "react";
import { ExternalLinkIcon } from "lucide-react";
import { resourceSource } from "@/lib/transport";
import { resourceActorId, safeLink } from "@/lib/conversation-links";
import { t } from "@/lib/i18n";
import { Button } from "./ui/button";
import { ResourceImage } from "./resource-image";

export type ArtifactObservation = {uri:string;title:string;kind:string;description?:string|null};
export function ArtifactObservationView({value}:{value:ArtifactObservation}) {
  const [expanded,setExpanded]=useState(false);
  const local=Boolean(resourceActorId(value.uri,typeof location==="undefined"?"http://localhost":location.href));
  const href=local?value.uri:safeLink(value.uri);
  if(!href)return <p className="dl-tool-note">{t("tool_details.could_not_open_the_result")}</p>;
  if(local&&["image","svg"].includes(value.kind))return <ResourceImage uri={href} name={value.title}/>;
  return <section className="dl-artifact"><div className="dl-artifact-header"><span>{value.title}</span><a href={href} target="_blank" rel="noreferrer" aria-label={t("tool_details.open_value0",{value0:value.title})}><ExternalLinkIcon/></a></div>
    {local&&value.kind==="html"&&<><iframe src={resourceSource(href)} sandbox="allow-scripts" referrerPolicy="no-referrer" title={value.title} className={expanded?"dl-artifact-frame-expanded":undefined}/><Button variant="ghost" size="sm" onClick={()=>setExpanded(!expanded)}>{t(expanded?"tool_details.collapse_preview":"tool_details.expand_preview")}</Button></>}
    {value.description&&<p>{value.description}</p>}
  </section>;
}
