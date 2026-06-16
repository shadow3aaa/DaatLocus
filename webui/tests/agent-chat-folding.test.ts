import { describe, expect, test } from "bun:test";

import {
  foldCompletedAgentChatActivity,
  type AgentChatFoldBubble,
} from "../src/lib/agent-chat-folding";

type TestBubble = AgentChatFoldBubble & {
  reply?: boolean;
};

function bubble(
  id: string,
  role: TestBubble["role"] = "tool",
  extra: Partial<TestBubble> = {},
): TestBubble {
  return { id, role, ...extra };
}

function fold(bubbles: TestBubble[]) {
  return foldCompletedAgentChatActivity(bubbles, {
    isOutputBoundary: (candidate) => Boolean(candidate.reply),
  });
}

describe("foldCompletedAgentChatActivity", () => {
  test("folds leading completed activity when the input boundary is outside the loaded window", () => {
    const displayItems = fold([
      bubble("read-file"),
      bubble("run-test"),
      bubble("assistant-reply", "assistant", { reply: true }),
    ]);

    expect(displayItems).toEqual([
      {
        kind: "foldedActivityGroup",
        id: "folded-assistant-reply",
        bubbles: [bubble("read-file"), bubble("run-test")],
      },
      {
        kind: "bubble",
        id: "assistant-reply",
        bubble: bubble("assistant-reply", "assistant", { reply: true }),
      },
    ]);
  });

  test("keeps folded group ids stable when older activity is prepended", () => {
    const partialWindow = fold([
      bubble("run-test"),
      bubble("assistant-reply", "assistant", { reply: true }),
    ]);
    const extendedWindow = fold([
      bubble("user-prompt", "user"),
      bubble("read-file"),
      bubble("run-test"),
      bubble("assistant-reply", "assistant", { reply: true }),
    ]);

    expect(partialWindow[0]).toMatchObject({
      kind: "foldedActivityGroup",
      id: "folded-assistant-reply",
    });
    expect(extendedWindow[1]).toMatchObject({
      kind: "foldedActivityGroup",
      id: "folded-assistant-reply",
    });
  });

  test("does not fold leading live activity without a completed output boundary", () => {
    const displayItems = fold([bubble("running-tool", "tool", { live: true })]);

    expect(displayItems).toEqual([
      {
        kind: "bubble",
        id: "running-tool",
        bubble: bubble("running-tool", "tool", { live: true }),
      },
    ]);
  });
});
