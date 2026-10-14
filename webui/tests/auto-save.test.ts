import {test,expect} from "bun:test";
import {AutoSave} from "../src/lib/auto-save";
test("auto save serializes pending edits and retains the newest edit",async()=>{
  const saved:string[]=[];let release!:()=>void;const blocked=new Promise<void>(resolve=>release=resolve);
  const saver=new AutoSave<string>(async value=>{saved.push(value);if(value==="first")await blocked;},()=>{},undefined,1000);
  saver.queue("first");const pending=saver.flush();saver.queue("second");saver.queue("last");release();await pending;
  expect(saved).toEqual(["first","last"]);
});
test("a failed edit remains available when auto save is flushed again",async()=>{
  let calls=0;let error=false;const saved:string[]=[];
  const saver=new AutoSave<string>(async value=>{if(calls++===0)throw Error("offline");saved.push(value);},()=>error=true,undefined,1000);
  saver.queue("draft");await saver.flush();expect(error).toBe(true);await saver.flush();expect(saved).toEqual(["draft"]);
});
