import { CodeBlock } from "./markdown";
import { FileChangeView } from "./file-change";
import { ToolParameters } from "./tool-parameters";
import { fieldLabel, readableSummary } from "@/lib/tool-presentation";
import type { JsonValue } from "@/lib/actor-api";
import { useEffect, useState } from "react";
import { Markdown } from "./markdown";
import { ArtifactObservationView, type ArtifactObservation } from "./artifact-observation";
import { LazyPage } from "./lazy-page";
import { t } from "@/lib/i18n";
import { actorIdFromUrl, resourceActorId, safeLink } from "@/lib/conversation-links";

type Schema = { $ref?:string; $defs?:Record<string,Schema>; properties?:Record<string,Schema>; items?:Schema; anyOf?:Schema[]; oneOf?:Schema[]; title?:string; type?:string; format?:string; "x-dl-content"?:string; "x-dl-error"?:string };
export type SourceObservation = {path:string;content:string;start_line?:number;end_line?:number;total_lines?:number};
export type ChangeObservation = {path:string;change?:JsonValue};
const privateFields=new Set(["private_metadata","model_content","model_transcript","debug","raw_response","resource_references","sandbox","sudo","permission_reference","ref_id","state_id","screenshot_url"]);

/** A type-derived schema selects content primitives; tool names never participate. */
export function StructuredObservation({value,schema,root=schema,fallback,onActor}: {value:JsonValue;schema?:Schema;root?:Schema;fallback?:string;onActor?(id:string):void}) {
  if(value===null)return <ToolParameters value={null}/>;
  for(let depth=0;depth<8&&schema;depth++) {
    if(schema.$ref?.startsWith("#/$defs/")) schema={...root?.$defs?.[schema.$ref.slice(8).replaceAll("~1","/").replaceAll("~0","~")],...schema,$ref:undefined};
    else if(schema.anyOf||schema.oneOf) {
      const options=schema.anyOf??schema.oneOf!;
      const selected=options.find(option=>value&&typeof value==="object"&&!Array.isArray(value)&&option.properties&&Object.keys(option.properties).some(key=>key in value))??options.find(option=>option.type!=="null");
      schema={...schema,...selected,anyOf:undefined,oneOf:undefined};
    } else break;
  }
  if(schema?.["x-dl-content"]==="source") {
    const source=value as unknown as SourceObservation;
    return <div className="dl-file-details"><CodeBlock code={source.content} language={source.path} startLine={source.start_line}/>{source.end_line!=null&&<small>{source.start_line}–{source.end_line} / {source.total_lines}</small>}</div>;
  }
  if(schema?.["x-dl-content"]==="file-change") {
    const change=value as unknown as ChangeObservation;
    return <FileChangeView path={change.path} value={change.change}/>;
  }
  if(schema?.["x-dl-content"]==="artifact")return <ArtifactObservationView value={value as unknown as ArtifactObservation}/>;
  if(schema?.["x-dl-content"]==="image")return null;
  if(schema?.["x-dl-content"]==="markdown"&&typeof value==="string")return <Markdown content={value}/>;
  if(schema?.["x-dl-content"]==="text"&&typeof value==="string")return <pre className="dl-observation-text">{value}</pre>;
  if(schema?.["x-dl-content"]==="status"&&typeof value==="string")return <span>{t(`observation_status.${value}`,{defaultValue:value})}</span>;
  if(schema?.format==="uri"&&typeof value==="string") {
    const uri=resourceActorId(value,typeof location==="undefined"?"http://localhost":location.href)?value:safeLink(value);
    const actor=actorIdFromUrl(value,typeof location==="undefined"?"http://localhost":location.href);
    return uri?<a href={uri} target={actor&&onActor?undefined:"_blank"} rel="noreferrer" onClick={actor&&onActor?event=>{event.preventDefault();onActor(actor);}:undefined}>{value}</a>:null;
  }
  if(value===null||typeof value!=="object") return <ToolParameters value={typeof value==="string"?readableSummary(value):value}/>;
  if(Array.isArray(value)) return <ObservationArray value={value} schema={schema?.items} root={root} onActor={onActor}/>;
  const entries=Object.entries(value).filter(([key,entry])=>!privateFields.has(key)&&(!schema?.properties||key in schema.properties)&&(typeof entry!=="string"||entry===""||readableSummary(entry)||schema?.properties?.[key]?.["x-dl-content"]==="text"));
  if(!entries.length)return fallback?<p className="dl-tool-note">{fallback}</p>:null;
  return <dl className="dl-parameter-fields">{entries.map(([key,entry])=><div className={entry!==null&&typeof entry==="object"?"dl-parameter-row dl-observation-collection":"dl-parameter-row"} key={key}><dt>{schema?.properties?.[key]?.title||fieldLabel(key)}</dt><dd><StructuredObservation value={entry} schema={schema?.properties?.[key]} root={root} onActor={onActor}/></dd></div>)}</dl>;
}

function ObservationArray({value,schema,root,onActor}:{value:JsonValue[];schema?:Schema;root?:Schema;onActor?(id:string):void}) {
  const [limit,setLimit]=useState(30);
  useEffect(()=>setLimit(30),[value]);
  if(!value.length)return <ToolParameters value={value}/>;
  return <div className="dl-observation-items">{value.slice(0,limit).map((item,index)=><div key={index}><StructuredObservation value={item} schema={schema} root={root} onActor={onActor}/></div>)}{value.length>limit&&<LazyPage cursor={limit} onLoad={async()=>setLimit(current=>current+50)} errorMessage={t("work_activity.could_not_load_more_activity")}/>}</div>;
}

export function outputSchema(value:JsonValue|undefined):Schema|undefined {
  return value&&typeof value==="object"&&!Array.isArray(value)?value as unknown as Schema:undefined;
}
