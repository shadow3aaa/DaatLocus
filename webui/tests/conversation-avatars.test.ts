import { expect, test } from "bun:test";
import { messageAvatar, actorAvatarSeed } from "../src/lib/conversation-avatars";
import { configureConversationDevice } from "../src/lib/conversation-links";
import type { Access, MessageRecord } from "../src/lib/actor-api";

const owner: Access = { kind: "owner", manage: true, account: { account_id: "owner-a", name: "Owner", revision: 1 } };
const message = { actor_id: "iris", source: { kind: "actor", actor_id: "core" }, reply_to: { kind: "actor", actor_id: "atlas", request_id: "task" } } as MessageRecord;

test("message avatars identify the sender rather than the reply recipient", () => {
  expect(messageAvatar(message, "input", owner).seed).toBe("actor:core");
  expect(messageAvatar(message, "reply", owner).seed).toBe("actor:iris");
  for (const source of [{ kind: "user" }, { kind: "remote_user", stage_id: "elsewhere", invocation_id: "invocation" }] as const) {
    expect(messageAvatar({ ...message, source }, "input", owner).seed).toBe("account:owner-a");
  }
});

test("remote conversations qualify device-local senders without changing explicit remote identities", () => {
  expect(messageAvatar(message, "input", owner, "device-a~iris").seed).toBe("actor:device-a~core");
  expect(messageAvatar(message, "reply", owner, "device-a~iris").seed).toBe("actor:device-a~iris");
  const remote = { ...message, source: { kind: "remote_actor", actor_id: "atlas", stage_id: "device-b", invocation_id: "invocation" } } as MessageRecord;
  expect(messageAvatar(remote, "input", owner, "device-a~iris").seed).toBe("actor:device-b~atlas");
  configureConversationDevice("device-a");
  try {
    expect(actorAvatarSeed("device-a~iris")).toBe("actor:iris");
    expect(messageAvatar(message, "reply", owner, "device-a~iris").seed).toBe("actor:iris");
  } finally {
    configureConversationDevice(null);
  }
});
