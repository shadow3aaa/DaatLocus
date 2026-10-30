import type { ContactMessage, ActorContact, ContactSort } from "../src/lib/contacts";
import type { ActorSummary } from "../src/lib/actor-api";
import { isInternalNotification } from "../src/lib/conversation-messages";
import { configHolder, store } from "./state";

type ContactKey = [number,number,string,string];
const contactKey=(actor:ActorSummary,sort:ContactSort):ContactKey=>[
  Number(sort!=="recent"||actor.status!=="running"),sort==="recent"?-actor.updated_at_ms:0,
  actor.name.replace(/[A-Z]/g,letter=>letter.toLowerCase()),actor.id,
];
function compare(a:ContactKey,b:ContactKey){
  for(let index=0;index<a.length;index++){if(a[index]<b[index])return -1;if(a[index]>b[index])return 1;}return 0;
}
export function contactsPage(url:URL) {
  const query=(url.searchParams.get("q")??"").trim().toLowerCase(),after=url.searchParams.get("after");
  const sort=url.searchParams.get("sort")??"alphabetical";
  const limit=Number(url.searchParams.get("limit")??24);
  const invalid=()=>Response.json({code:"invalid_request"},{status:422});
  if(query.length>512||!Number.isInteger(limit)||limit<1||limit>32||(sort!=="alphabetical"&&sort!=="recent"))return invalid();
  let position:ContactKey|null=null;
  if(after){try{
    const cursor=JSON.parse(new TextDecoder().decode(Uint8Array.from(atob(after.replace(/-/g,"+").replace(/_/g,"/")),char=>char.charCodeAt(0))));
    if(cursor.sort!==sort||!Array.isArray(cursor.key)||cursor.key.length!==4||!cursor.key.every((value:unknown,index:number)=>typeof value===(index<2?"number":"string")))return invalid();
    position=cursor.key;
  }catch{return invalid();}}
  const items:ActorContact[]=[];
  for(const actor of store.actors.values()) {
    const bubbles:ContactMessage[]=[];
    for(const message of actor.messages) {
      if(!isInternalNotification(message)&&message.content&&(actor.summary.id!=="core"||["user","remote_user"].includes(message.source.kind)))bubbles.push({message_id:message.id,kind:"input",content:message.content,result_id:null,generation:null});
      if(message.reply&&(actor.summary.id!=="core"||message.status==="completed"&&(message.reply_to.kind==="user"||message.reply_to.kind==="remote"&&message.reply_to.user)))bubbles.push({message_id:message.id,kind:"reply",content:message.reply,result_id:null,generation:null});
      if(actor.summary.id!=="core") {
        const generations=new Map<number,string>();
        for(const {event} of actor.events)if(event.type==="generation_delta"&&event.message_id===message.id&&event.channel==="assistant")generations.set(event.generation,(generations.get(event.generation)??"")+event.text);
        for(const [generation,content] of generations)if(!(message.status==="completed"&&message.reply&&generation===(message.final_generation??Math.max(...generations.keys()))))bubbles.push({message_id:message.id,kind:"assistant",content,result_id:null,generation});
        for(const {result} of actor.tools)if(result.message_id===message.id&&!result.error){
          const payload=result.payload;
          if(payload&&typeof payload==="object"&&!Array.isArray(payload)){
            const sent=payload.sent_message;
            if(sent&&typeof sent==="object"&&!Array.isArray(sent)&&typeof sent.content==="string")bubbles.push({message_id:message.id,kind:"sent",content:sent.content,result_id:result.id,generation:null});
          }
        }
      }

    }
    const messages=query?bubbles.filter(message=>message.content.toLowerCase().includes(query)).reverse().slice(0,3):[];
    const config=configHolder.current,preference=actor.detail.config?.model;
    const name=config.models.some(model=>model.name===preference)?preference:config.main_model;
    const model=config.models.some(model=>model.name===name)?name??null:null;
    if(!query||actor.summary.name.toLowerCase().includes(query)||messages.length)items.push({actor:actor.summary,model,messages});
  }
  items.sort((a,b)=>compare(contactKey(a.actor,sort),contactKey(b.actor,sort)));
  const remaining=position?items.filter(item=>compare(contactKey(item.actor,sort),position!)>0):items;
  const page=remaining.slice(0,limit);
  const next=remaining.length>limit?btoa(String.fromCharCode(...new TextEncoder().encode(JSON.stringify({sort,key:contactKey(page.at(-1)!.actor,sort)})))).replace(/\+/g,"-").replace(/\//g,"_").replace(/=+$/,""):null;
  return Response.json({items:page,next_cursor:next,event_cursor:store.catalogSeq,unavailable_devices:[]});
}
