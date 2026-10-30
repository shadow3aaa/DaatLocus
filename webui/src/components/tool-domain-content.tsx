import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import type { JsonValue } from "@/lib/actor-api";
import { asObject, asRows, asText } from "@/lib/tool-presentation";
import { guiNeedsAttention, terminalText } from "@/lib/tool-content";
import { safeLink, resourceActorId } from "@/lib/conversation-links";
import { t } from "@/lib/i18n";
import { LazyPage } from "./lazy-page";
import { Markdown, CodeBlock } from "./markdown";
import { FileChangeView } from "./file-change";
import { ArtifactObservationView, type ArtifactObservation } from "./artifact-observation";
import { StructuredObservation } from "./structured-observation";

function ResultList({items,render}:{items:Record<string,JsonValue>[];render(item:Record<string,JsonValue>,index:number):ReactNode}) {
  const [limit,setLimit]=useState(30);
  useEffect(()=>setLimit(30),[items.length]);
  return <div className="dl-result-list">{items.slice(0,limit).map((item,index)=><div key={index}>{render(item,index)}</div>)}{limit<items.length&&<LazyPage cursor={limit} onLoad={async()=>setLimit(value=>value+50)} errorMessage={t("work_activity.could_not_load_more_activity")}/>}</div>;
}
function DocumentLink({title,url}:{title:string;url?:string}) {
  const href=url&&(safeLink(url)||resourceActorId(url,typeof location==="undefined"?"http://localhost":location.href)&&url);
  return href?<a className="dl-result-link" href={href} target="_blank" rel="noreferrer">{title||url}</a>:title?<span>{title}</span>:null;
}
export function TerminalContent({value,live=false}:{value:Record<string,JsonValue>;live?:boolean}) {
  const text=terminalText(value),session=asObject(value.session);
  const running=live||value.running===true||session.status==="running";
  const exit=typeof session.exit_code==="number"?session.exit_code:typeof value.exit_code==="number"?value.exit_code:null;
  const lines=text.split("\n"),[limit,setLimit]=useState(160);
  const viewport=useRef<HTMLDivElement>(null),following=useRef(true);
  useLayoutEffect(()=>{const node=viewport.current;if(running&&following.current&&node)node.scrollTop=node.scrollHeight;},[text,running]);
  return <>
    {text?<div className="dl-terminal-output" ref={viewport} onScroll={()=>{const node=viewport.current;if(node)following.current=node.scrollHeight-node.scrollTop-node.clientHeight<24;}}><pre>{running?text:lines.slice(0,limit).join("\n")}</pre>{!running&&lines.length>limit&&<LazyPage cursor={limit} onLoad={async()=>setLimit(count=>count+200)} errorMessage={t("work_activity.could_not_load_more_activity")}/>}</div>:running?<p className="dl-tool-note" role="status">{t("tool_details.waiting_for_output")}</p>:null}
    {!running&&exit!==null&&exit!==0&&<p className="dl-tool-note">{t("tool_details.exit_code",{code:exit})}</p>}
  </>;
}
export function ToolDomainContent({kind,value,live}:{kind:string;value:Record<string,JsonValue>;live?:boolean}) {
  switch(kind) {
    case "terminal":return <TerminalContent value={value} live={live}/>;
    case "source":return <CodeBlock code={asText(value.content)} language={asText(value.path)} startLine={typeof value.start_line==="number"?value.start_line:undefined}/>;
    case "file-change":return <FileChangeView path={asText(value.path)} value={value.change}/>;
    case "artifact":return <ArtifactObservationView value={value as unknown as ArtifactObservation}/>;
    case "directory":return <ResultList items={asRows(value.entries)} render={entry=><span className="dl-result-path">{asText(entry.name)||asText(entry.path)}{entry.kind==="directory"?"/":""}</span>}/>;
    case "search":return <ResultList items={asRows(value.matches)} render={entry=><><small className="dl-result-path">{asText(entry.path)}{typeof entry.line_number==="number"?`:${entry.line_number}`:""}</small><pre className="dl-search-match">{asText(entry.text)||asText(entry.content)}</pre></>}/>;
    case "skills":case "apps":return <ResultList items={kind==="skills"?asRows(value.skills):asRows(value.apps).length?asRows(value.apps):[asObject(value.app)]} render={entry=><><strong>{asText(entry.name)}</strong>{asText(entry.description)&&<p className="dl-tool-note">{asText(entry.description)}</p>}</>}/>;
    case "gui": {
      const observation=asObject(value.observation);
      const items=Object.keys(observation).length?[]:[value.apps,value.windows,value.pages,value.discovered_windows,value.related_windows].flatMap(asRows).filter(item=>item.name||item.title||item.url);
      return <>{guiNeedsAttention(value)&&<p className="dl-tool-note">{t(`observation_status.${value.status}`)}</p>}{items.length>0&&<ResultList items={items} render={entry=><DocumentLink title={asText(entry.title)||asText(entry.name)} url={asText(entry.url)}/>}/>}</>;
    }
    case "web":return <ResultList items={asRows(value.results)} render={entry=>{
      const document=asObject(entry.result);
      return <>{entry.error?<p className="dl-operation-error">{asText(entry.error)}</p>:null}
        <DocumentLink title={asText(document.title)} url={asText(document.url)}/>
        {document.structured_content?<StructuredObservation value={document.structured_content}/>:asText(document.content)&&<pre className="dl-observation-text">{asText(document.content)}</pre>}
        {asRows(document.matches).map((match,index)=><pre className="dl-search-match" key={index}>{asText(match.content)}</pre>)}
      </>;
    }}/>;
    case "status":return typeof value.status==="string"?<p className="dl-tool-note">{t(`observation_status.${value.status}`,{defaultValue:value.status})}</p>:null;
    case "image":case "actor":return null;
    case "markdown":return <Markdown content={asText(value.content)}/>;
    default:return null;
  }
}
