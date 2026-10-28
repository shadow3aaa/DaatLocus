import type { ContactMessage, ActorContact } from "../src/lib/contacts";
import { isInternalNotification } from "../src/lib/conversation-messages";
import { store } from "./state";

export function contactsPage(url:URL) {
  const query=(url.searchParams.get("q")??"").trim().toLowerCase(),after=url.searchParams.get("after");
  const limit=Number(url.searchParams.get("limit")??24);
  if(query.length>512||limit<1||limit>32)return Response.json({code:"invalid_request"},{status:422});
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
    if(!query||actor.summary.name.toLowerCase().includes(query)||messages.length)items.push({actor:actor.summary,messages});
  }
  items.sort((a,b)=>a.actor.name.toLowerCase().localeCompare(b.actor.name.toLowerCase())||a.actor.id.localeCompare(b.actor.id));
  const start=after?items.findIndex(item=>item.actor.id===after)+1:0;
  const page=items.slice(start,start+limit);
  return Response.json({items:page,next_cursor:items.length>start+limit?page.at(-1)!.actor.id:null,event_cursor:store.catalogSeq,unavailable_devices:[]});
}
