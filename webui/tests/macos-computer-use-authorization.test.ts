import { expect, test } from "bun:test";
import { refreshMacAuthorization } from "../src/lib/macos-computer-use-authorization";
import type { ComputerUsePermissions } from "../src/lib/macos-computer-use-display";

const authorized: ComputerUsePermissions = {enabled:true,accessibility:true,screen_recording:true,display_supported:true,display_ready:false,display:{width:1920,height:1080,scale:1}};
test("returning after the last permission continues an already enabled virtual display", async () => {
  let accessibility = false;
  const calls: boolean[] = [];
  const bridge = {
    status: async () => ({...authorized,accessibility}),
    configure: async (enabled: boolean) => {calls.push(enabled);return {...authorized,display_ready:true};},
  };
  expect((await refreshMacAuthorization(bridge)).accessibility).toBe(false);
  expect(calls).toEqual([]);
  accessibility = true;
  await refreshMacAuthorization(bridge);
  expect(calls).toEqual([true]);
});
test("permission refresh cannot enable a disabled, unapproved or unsupported display", async () => {
  for (const patch of [{enabled:false},{accessibility:false},{screen_recording:false},{display_supported:false},{display_ready:true}]) {
    let configured = false;
    await refreshMacAuthorization({status:async () => ({...authorized,...patch}),configure:async () => {configured=true;throw Error("Unexpected configuration");}});
    expect(configured).toBe(false);
  }
});
test("failed display initialization reaches the settings error without automatic retries", async () => {
  let calls = 0;
  await expect(refreshMacAuthorization({status:async () => authorized,configure:async () => {calls++;throw Error("display unavailable");}})).rejects.toThrow("display unavailable");
  expect(calls).toBe(1);
});
test("a permission read finishing after disable or navigation cannot initialize a display", async () => {
  let release!: (status: ComputerUsePermissions) => void;
  let current = true;
  let configured = false;
  const refresh = refreshMacAuthorization({
    status:() => new Promise(resolve => {release=resolve;}),
    canConfigure:() => current,
    configure:async () => {configured=true;return authorized;},
  });
  current=false;
  release(authorized);
  await refresh;
  expect(configured).toBe(false);
});
