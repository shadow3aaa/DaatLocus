import { expect, test } from "bun:test";
import { sshTargets, validConnection, type SshDraft } from "../src/lib/device-settings";

const row: SshDraft = {key:"one",stage:"device",host:" host.local ",user:"existing-user",ssh_port:"22",stage_port:"45825"};
test("SSH edits preserve the configured user and reject incomplete, duplicate or invalid targets", () => {
  expect(sshTargets([row])).toEqual({device:{host:"host.local",user:"existing-user",ssh_port:22,stage_port:45825}});
  expect(sshTargets([])).toEqual({});
  expect(sshTargets([row,{...row,key:"two"}])).toBeNull();
  for (const change of [{stage:""},{host:" "},{ssh_port:"0"},{ssh_port:"1.5"},{stage_port:"65536"},{stage_port:""}]) {
    expect(sshTargets([row,{...row,key:"two",stage:"other",...change}])).toBeNull();
  }
});
test("connection preferences only save complete credential-free WSS origins", () => {
  for (const value of ["", "wss://signal.example", "wss://signal.example:8443/"]) expect(validConnection(value)).toBe(true);
  for (const value of ["https://signal.example", "wss://user:secret@signal.example", "wss://signal.example/path", "wss://signal.example?secret=x", "wss://"]) expect(validConnection(value)).toBe(false);
});
