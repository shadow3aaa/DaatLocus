import { expect, test } from "bun:test";
import { WebSearchPreferences, type SearchSettings } from "../src/lib/websearch";

const initial: SearchSettings={provider:"tavily",providers:[{id:"tavily",credential_configured:false},{id:"brave",credential_configured:false}]};
test("WebSearch preferences retain a failed key and retry the exact narrow patch",async()=>{
  const writes: unknown[]=[]; let fail=true;
  const preferences=new WebSearchPreferences({get:async()=>initial,patch:async patch=>{writes.push(patch);if(fail)throw Error("offline");return {...initial,providers:[{id:"tavily",credential_configured:true}]};}});
  await preferences.load(); preferences.editKey("draft-key");
  await preferences.save({provider:"tavily",api_key:"draft-key"});
  expect(preferences.getSnapshot().key).toBe("draft-key"); expect(preferences.getSnapshot().error).toContain("offline");
  fail=false; await preferences.retry();
  expect(writes).toEqual([{provider:"tavily",api_key:"draft-key"},{provider:"tavily",api_key:"draft-key"}]);
  expect(preferences.getSnapshot().key).toBe(""); expect(preferences.getSnapshot().error).toBeNull();
  expect(preferences.getSnapshot().settings?.providers[0].credential_configured).toBe(true);
});
test("WebSearch saves serialize and an earlier completion cannot discard a newer key or provider selection",async()=>{
  let release!:()=>void; const blocked=new Promise<void>(resolve=>release=resolve);
  const writes: unknown[]=[];
  const preferences=new WebSearchPreferences({get:async()=>initial,patch:async patch=>{writes.push(patch);if(writes.length===1)await blocked;return {...initial,provider:patch.provider};}});
  await preferences.load(); preferences.editKey("first");
  const first=preferences.save({provider:"tavily",api_key:"first"});
  preferences.editKey("new draft"); const second=preferences.save({provider:"brave"});
  expect(preferences.getSnapshot().settings?.provider).toBe("brave");
  release(); await Promise.all([first,second]);
  expect(writes).toEqual([{provider:"tavily",api_key:"first"},{provider:"brave"}]);
  expect(preferences.getSnapshot().key).toBe("new draft");
  expect(preferences.getSnapshot().settings?.provider).toBe("brave");
  expect(preferences.getSnapshot().pending).toBe(false);
});
