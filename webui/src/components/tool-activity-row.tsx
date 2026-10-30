import { useEffect, useRef, useState } from "react";
import { ChevronRightIcon } from "lucide-react";
import { getToolResult, type ToolResult, type ToolSummary } from "@/lib/actor-api";
import { activityLabel, asObject, operationToolName, toolIcon } from "@/lib/tool-presentation";
import { isBuiltinTool, isInternalTool, resultImages, toolContentKind, toolHasContent, toolResultTitle } from "@/lib/tool-content";
import { latestOperation } from "@/lib/latest-operation";
import { t } from "@/lib/i18n";
import { cn } from "@/lib/utils";
import { ToolDetail } from "./tool-details";
import { pageScrollRoot } from "./lazy-page";
import { Button } from "./ui/button";

export function ToolActivityRow({actorId,tool,onActor}:{actorId:string;tool:ToolSummary&{status?:"running"|"completed"|"unknown"};onActor(id:string):void}) {
  const row=useRef<HTMLDivElement>(null),[visible,setVisible]=useState(false),[open,setOpen]=useState(tool.status==="running");
  const [loaded,setLoaded]=useState<{id:string;result:ToolResult}|null>(null),[failed,setFailed]=useState(false);
  const builtin=isBuiltinTool(tool),internal=isInternalTool(tool),domain=operationToolName(tool);
  const inspect=!internal&&(builtin?visible:open)&&!tool.id.startsWith("live:");
  const load=useRef<(revision:number)=>Promise<void>>(async()=>{});
  useEffect(()=>{
    const controller=new AbortController();
    load.current=latestOperation(async(_revision:number)=>{
    if(controller.signal.aborted)return;
    try {
      const result=await getToolResult(actorId,tool.id,{signal:controller.signal});
      if(!controller.signal.aborted){setLoaded({id:tool.id,result});setFailed(false);}
    } catch {if(!controller.signal.aborted)setFailed(true);}
    });
    return()=>controller.abort();
  },[actorId,tool.id]);
  useEffect(()=>{
    const node=row.current;if(!node||internal)return;
    const observer=new IntersectionObserver(entries=>setVisible(entries.some(entry=>entry.isIntersecting)),{root:pageScrollRoot(node),rootMargin:"120px 0px"});
    observer.observe(node);return()=>observer.disconnect();
  },[internal]);
  useEffect(()=>{if(inspect)void load.current(tool.seq);},[inspect,tool.seq,actorId,tool.id]);
  const result=loaded?.id===tool.id?loaded.result:null;
  const text=activityLabel(tool),Icon=toolIcon(tool.name,tool.outcome),outcome=asObject(tool.outcome);
  const detail=result?toolResultTitle(result)||text.detail:text.detail;
  const changed=domain.startsWith("files__")&&/__(write|edit|copy)_file$/.test(domain);
  const inlineImage=Boolean(result&&resultImages(result).length&&["image","actor","gui"].includes(toolContentKind(result)??""));
  const running=tool.status==="running"&&domain.startsWith("terminal__");
  const expandable=!internal&&!inlineImage&&(result?toolHasContent(result):!builtin||running);
  const contents=<><Icon aria-hidden="true"/><span className="dl-step-text">{text.label}{detail&&<> <span className={cn("dl-step-summary",changed&&"dl-step-file")} title={detail}>{changed?detail.replace(/\\/g,"/").split("/").at(-1):detail}</span></>}</span>
    {changed&&typeof outcome.added_lines==="number"&&<span className="dl-step-stat">+{outcome.added_lines}</span>}
    {changed&&typeof outcome.removed_lines==="number"&&<span className="dl-step-stat">−{outcome.removed_lines}</span>}</>;
  return <div className="dl-step" ref={row}>
    {expandable?<button type="button" className="dl-step-label" data-running-command={running||undefined} aria-expanded={open} onClick={()=>setOpen(value=>!value)}>{contents}<ChevronRightIcon aria-hidden="true" className={cn("dl-step-chevron",open&&"dl-activity-open")}/></button>:<div className="dl-step-label" aria-busy={inspect&&!result&&!failed||undefined}>{contents}</div>}
    {(open||inlineImage)&&result&&<div className="dl-step-detail"><ToolDetail result={result} onActor={onActor} live={running}/></div>}
    {open&&running&&!result&&<p className="dl-tool-note" role="status">{t("tool_details.waiting_for_output")}</p>}
    {failed&&<div className="dl-client-error" role="alert"><span>{t("conversation_content.could_not_load_details_try_again")}</span><Button variant="ghost" size="sm" onClick={()=>void load.current(tool.seq)}>{t("app.retry")}</Button></div>}
  </div>;
}
