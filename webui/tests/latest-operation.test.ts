import {expect,test} from "bun:test";
import {latestOperation} from "../src/lib/latest-operation";
test("layout backpressure applies the newest geometry and preserves final detachment",async()=>{
 const seen:(number|null)[]=[];let resume!:()=>void;
 const update=latestOperation<number|null>(async value=>{seen.push(value);if(value===1)await new Promise<void>(resolve=>resume=resolve);});
 const first=update(1);const pending=Array.from({length:100},(_,i)=>update(i+2));const detached=update(null);
 expect(seen).toEqual([1]);resume();await Promise.all([first,...pending,detached]);expect(seen).toEqual([1,null]);
});
test("a failed native update does not prevent the next layout",async()=>{
 const seen:number[]=[];const update=latestOperation<number>(async value=>{seen.push(value);if(value===1)throw new Error("unavailable");});
 const failed=update(1);const recovered=update(2);await expect(failed).rejects.toThrow("unavailable");await recovered;expect(seen).toEqual([1,2]);
});
