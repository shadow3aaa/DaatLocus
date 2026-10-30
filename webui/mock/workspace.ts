/** A normal account conversation about several collaborators preparing a release. */
import { addMessage, configHolder, createActor, emitEvent, now, requestOrigin, ROOT_ACTOR_ID, setActorStatus, touchMessage, type ActorStore } from "./state";

export function seedWorkspacePreview(core:ActorStore,metis:ActorStore) {
  const aliases=["GPT-6 Astra","GPT-6 Mini","Claude Opus 5.5"];
  const modelIds=["gpt-6-astra","gpt-6-mini","claude-opus-5.5"];
  const config=configHolder.current;
  config.models=config.models.map((model,index)=>({...model,name:aliases[index],model_id:modelIds[index]}));
  config.main_model=aliases[0];config.approval_model=aliases[1];
  core.detail.config!.model=aliases[0];metis.detail.config!.model=aliases[1];
  const clock=now();
  function exchange(content:string,reply:string,minutes:number) {
    const input=addMessage(core,{kind:"request",...requestOrigin(core),content,status:"completed"});
    touchMessage(input,{created_at_ms:clock-minutes*60000,started_at_ms:clock-minutes*60000+700,finished_at_ms:clock-minutes*60000+4000,reply,final_generation:1});
    emitEvent(core.summary.id,{type:"message_added",message_id:input.id});
    return input;
  }
  const task=exchange("Prepare the next Atlas release: review sign-up, improve the mobile layout, and write the release notes.", "Three collaborators are on it:\n\n- **Metis** is reviewing sign-up and the API.\n- **Hephaestus** is refining the mobile layout.\n- **Hermes** is drafting the release notes.\n\nI will bring any decisions back here.",8);
  exchange("Keep the current visual style and leave social login out. Show me the changes before publishing.", "Understood. I will finish the changes and checks, then wait for your approval to publish.",6);
  const developers=[metis,...["Hephaestus","Hermes"].map((name,index)=>createActor({id:name.toLowerCase(),name,parent_id:ROOT_ACTOR_ID,model:aliases[index===0?2:1],workspace:"~/Projects/atlas",apps:[],instructions:""}))];
  const tasks=["Review email verification and resend behavior in the sign-up API.","Improve the mobile sign-up layout while preserving the current style.","Draft release notes for the sign-up and mobile changes. Do not publish."];
  for(const [index,actor] of developers.entries()) {
    const input=addMessage(actor,{kind:"request",source:{kind:"actor",actor_id:ROOT_ACTOR_ID},reply_to:{kind:"actor",actor_id:ROOT_ACTOR_ID,request_id:task.id},content:tasks[index],status:index===2?"running":"completed"});
    touchMessage(input,{created_at_ms:clock-7*60000,started_at_ms:clock-7*60000+800,finished_at_ms:index===2?null:clock-2*60000,reply:index===2?null:["Email verification and resending are checked. The regression tests passed.","Checked widths from 320px to desktop. The submit button stays visible above the keyboard."][index]});
    emitEvent(actor.summary.id,{type:"message_added",message_id:input.id});
    setActorStatus(actor,index===2?"running":"idle");
  }
  const delivery=addMessage(core,{kind:"delivery",source:{kind:"actor",actor_id:ROOT_ACTOR_ID},reply_to:{kind:"user"},in_reply_to:task.id,content:"",status:"completed"});
  touchMessage(delivery,{created_at_ms:clock-60000,started_at_ms:clock-60000,finished_at_ms:clock-59000,reply:"**Sign-up and the mobile layout are ready.**\n\n- Email verification stays on the page, with a resend countdown.\n- Checked 320px through desktop; the keyboard leaves the submit button visible.\n- Regression tests passed, and the existing visual style is unchanged.\n\nHermes is finishing the release notes. I will bring you the changes and preview together for review."});
  emitEvent(core.summary.id,{type:"message_added",message_id:delivery.id});
}
