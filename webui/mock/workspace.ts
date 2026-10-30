/** An everyday account conversation with several Actors working in parallel. */
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
  const task=exchange("Help me plan a weekend in Kyoto, compare wireless headphones under $200, and plan five weeknight dinners for two.", "I've created three Actors to work on these in parallel:\n\n- **Hermes** is planning the Kyoto weekend.\n- **Metis** is comparing headphones.\n- **Demeter** is putting together the dinners and grocery list.\n\nI'll bring their suggestions back here.",8);
  exchange("Keep the dinners vegetarian, and leave plenty of free time in the itinerary.", "Got it: vegetarian dinners and a relaxed pace for the weekend.",6);
  const collaborators=[metis,...["Demeter","Hermes"].map(name=>createActor({id:name.toLowerCase(),name,parent_id:ROOT_ACTOR_ID,model:aliases[1],workspace:"~/Documents/Plans",apps:[],instructions:""}))];
  const tasks=["Compare wireless headphones under $200 for comfort, noise cancellation and battery life.","Plan five vegetarian weeknight dinners for two, with one grocery list.","Plan a relaxed two-day Kyoto itinerary with plenty of free time. Check opening hours and group nearby stops."];
  for(const [index,actor] of collaborators.entries()) {
    const input=addMessage(actor,{kind:"request",source:{kind:"actor",actor_id:ROOT_ACTOR_ID},reply_to:{kind:"actor",actor_id:ROOT_ACTOR_ID,request_id:task.id},content:tasks[index],status:index===2?"running":"completed"});
    touchMessage(input,{created_at_ms:clock-7*60000,started_at_ms:clock-7*60000+800,finished_at_ms:index===2?null:clock-2*60000,reply:index===2?null:["Three headphones shortlisted under $200, with a comparison of comfort, noise cancellation and battery life.","Five vegetarian dinners planned: chickpea curry, mushroom pasta, tofu stir-fry, bean tacos and a vegetable tray bake. The grocery list combines shared ingredients."][index]});
    emitEvent(actor.summary.id,{type:"message_added",message_id:input.id});
    setActorStatus(actor,index===2?"running":"idle");
  }
  const delivery=addMessage(core,{kind:"delivery",source:{kind:"actor",actor_id:ROOT_ACTOR_ID},reply_to:{kind:"user"},in_reply_to:task.id,content:"",status:"completed"});
  touchMessage(delivery,{created_at_ms:clock-60000,started_at_ms:clock-60000,finished_at_ms:clock-59000,reply:"**The dinner plan and headphone shortlist are ready.**\n\n- **Dinners:** chickpea curry, mushroom pasta, tofu stir-fry, bean tacos and a vegetable tray bake, with one grocery list.\n- **Headphones:** three options under $200, compared for comfort, noise cancellation and battery life.\n\nHermes is checking opening hours for the Kyoto itinerary. Would you rather focus on gardens or food?"});
  emitEvent(core.summary.id,{type:"message_added",message_id:delivery.id});
}
