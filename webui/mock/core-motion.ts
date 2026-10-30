import { addMessage, emitEvent, now, ROOT_ACTOR_ID, setActorStatus, store, touchMessage, type ActorStore } from "./state";
import type { MessageRecord } from "../src/lib/actor-api";

let phase = 0;
const total = 6;
export function seedCoreMotionPreview(core: ActorStore) {
  phase = 0;
  const input = addMessage(core,{kind:"request",source:{kind:"user"},reply_to:{kind:"user"},content:"Please review the page layout.",status:"completed"});
  touchMessage(input,{started_at_ms:now()-2000,finished_at_ms:now()-1000,reply:"I will check the messages, activity records and mobile layout."});
  setActorStatus(core,"idle");
}
export function advanceCoreMotionPreview() {
  const core = store.actors.get(ROOT_ACTOR_ID)!;
  if (phase >= total) return {phase,total,done:true,input:null};
  phase++;
  if (phase === 1 || phase === 3) {
    return {phase,total,done:false,input:phase===1?"Also check transitions for bubbles and delivery status.":"I can add more messages while you think.\nThis message has several lines.\nSending it should not make the page jump."};
  } else if (phase === 2) {
    const input = core.messages[1];
    touchMessage(input,{status:"running",started_at_ms:now()});
    emitEvent(core.summary.id,{type:"message_updated",message_id:input.id,status:"running"});
    setActorStatus(core,"running");
  } else if (phase === 4 || phase === 5) {
    const input = core.messages[phase===4?1:2];
    touchMessage(input,{status:"completed",finished_at_ms:now(),reply:phase===4?"Added transitions for new messages and position changes. Old delivery receipts fade away.":"Understood. New messages appear immediately, with the thinking indicator at the end."});
    emitEvent(core.summary.id,{type:"message_updated",message_id:input.id,status:"completed"});
    if (phase === 5) setActorStatus(core,"idle");
  } else {
    // Disposable mock history demonstrates removal and receipt reappearance.
    core.messages.pop();
    emitEvent(core.summary.id,{type:"conversation_restored",archive_id:"mock-motion"});
  }
  return {phase,total,done:phase===total,input:null};
}
export function acceptCoreMotionInput(message: MessageRecord) {
  touchMessage(message,{status:phase===1?"queued":"running",started_at_ms:phase===1?null:now()});
}
