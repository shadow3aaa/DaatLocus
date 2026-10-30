import { t } from "@/lib/i18n";
import type { ToolResult } from "@/lib/actor-api";
import { asObject, approvalNotice, approvalNoticeTitle, readableSummary } from "@/lib/tool-presentation";
import { resultImages, toolContentKind, toolHasContent } from "@/lib/tool-content";
import { StructuredObservation, outputSchema } from "./structured-observation";
import { ToolDomainContent } from "./tool-domain-content";
import { ResourceImage } from "./resource-image";

type ToolDetailProps = {result:ToolResult;onActor?(id:string):void;live?:boolean};
const domains=new Set(["terminal","source","file-change","image","actor","status","web","gui","directory","search","skills","apps","artifact"]);

export function ToolDetail({result,onActor,live}:ToolDetailProps) {
  const schema=outputSchema(result.output_schema);
  const rejection=approvalNotice(result.payload);
  if(rejection)return <section className="dl-tool-details" aria-label={approvalNoticeTitle(rejection.kind)}><p className="dl-tool-note">{approvalNoticeTitle(rejection.kind)}</p><p className="whitespace-pre-wrap break-words">{rejection.reason}</p></section>;
  if(!live&&!toolHasContent(result))return null;
  const payload=asObject(result.payload),images=resultImages(result),kind=toolContentKind(result);
  return <section className="dl-tool-details">
    {result.error&&<p className="dl-operation-error">{schema?.["x-dl-error"]==="generic"?t("tool_details.action_failed"):readableSummary(result.error)||t("tool_details.action_failed")}</p>}
    {kind&&domains.has(kind)?<ToolDomainContent kind={kind} value={payload} live={live}/>:<StructuredObservation onActor={onActor} value={result.payload} schema={schema} fallback={!result.error&&!images.length?readableSummary(result.summary)||t("tool_details.action_completed"):undefined}/>}
    {images.map(image=><ResourceImage key={image.uri} {...image}/>)}
  </section>;
}
