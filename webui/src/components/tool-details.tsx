import { resourceSource } from "@/lib/transport";
import { t } from "@/lib/i18n";
import { useState } from "react";
import type { ToolResult } from "@/lib/actor-api";
import { resourceActorId } from "@/lib/conversation-links";
import { asObject, asRows, asText, approvalNotice, approvalNoticeTitle, readableSummary } from "@/lib/tool-presentation";
import { StructuredObservation, outputSchema } from "./structured-observation";

type ToolDetailProps = {result:ToolResult;onActor?(id:string):void};

function ImageThumbnail({uri,name}:{uri:string;name:string}) {
  const [failed,setFailed]=useState(false);
  return <a href={uri} target="_blank" rel="noreferrer" className="block min-w-0 max-w-full" aria-label={t("tool_details.open_value0",{value0:name})}>
    {failed?<span className="dl-tool-note">{t("tool_details.image_preview_unavailable")}</span>:<img src={resourceSource(uri)} alt={name} loading="lazy" className="block h-auto max-h-64 max-w-full rounded-xl object-contain" onError={()=>setFailed(true)}/>}
  </a>;
}

export function ToolDetail({result,onActor}:ToolDetailProps) {
  const schema=outputSchema(result.output_schema);
  const rejection=approvalNotice(result.payload);
  if(rejection)return <section className="dl-tool-details" aria-label={approvalNoticeTitle(rejection.kind)}><p className="dl-tool-note">{approvalNoticeTitle(rejection.kind)}</p><p className="whitespace-pre-wrap break-words">{rejection.reason}</p></section>;
  const payload=asObject(result.payload),observation=asObject(payload.observation);
  const origin=typeof location==="undefined"?"http://localhost":location.href;
  const fallback=asText(observation.title)||asText(payload.path).split(/[\\/]/).at(-1)||t("markdown.image");
  const seen=new Set<string>();
  const images=[...asRows(payload.resource_references),{uri:observation.screenshot_url,name:fallback,media_type:"image/"}].flatMap(reference=>{
    const uri=asText(reference.uri);
    if(!asText(reference.media_type).startsWith("image/")||!resourceActorId(uri,origin)||seen.has(uri))return [];
    seen.add(uri);const name=asText(reference.name);
    return [{uri,name:name&&name!=="image"?name:fallback}];
  });
  return <section className="dl-tool-details">
    {result.error&&<p className="dl-operation-error">{schema?.["x-dl-error"]==="generic"?t("tool_details.action_failed"):readableSummary(result.error)||t("tool_details.action_failed")}</p>}
    <StructuredObservation onActor={onActor} value={result.payload} schema={schema} fallback={!result.error&&!images.length?readableSummary(result.summary)||t("tool_details.action_completed"):undefined}/>
    {schema?.["x-dl-content"]==="image"&&!images.length&&<p className="dl-tool-note">{t("tool_details.image_preview_unavailable")}</p>}
    {images.length>0&&<div className="flex min-w-0 flex-wrap items-start gap-3">{images.map(image=><ImageThumbnail key={image.uri} {...image}/>)}</div>}
  </section>;
}
