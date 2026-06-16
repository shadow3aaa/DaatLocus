export type AgentChatFoldBubbleRole =
  | "assistant"
  | "user"
  | "tool"
  | "telegram"
  | "system";

export type AgentChatFoldBubble = {
  id: string;
  role: AgentChatFoldBubbleRole;
  live?: boolean;
};

export type AgentChatFoldDisplayItem<TBubble extends AgentChatFoldBubble> =
  | {
      kind: "bubble";
      id: string;
      bubble: TBubble;
    }
  | {
      kind: "foldedActivityGroup";
      id: string;
      bubbles: TBubble[];
    };

type AgentChatFoldOptions<TBubble extends AgentChatFoldBubble> = {
  isOutputBoundary: (bubble: TBubble) => boolean;
  canFoldWithCompletedWork?: (bubble: TBubble) => boolean;
  isUserInputBoundary?: (bubble: TBubble) => boolean;
};

export function foldCompletedAgentChatActivity<
  TBubble extends AgentChatFoldBubble,
>(
  bubbles: readonly TBubble[],
  options: AgentChatFoldOptions<TBubble>,
): AgentChatFoldDisplayItem<TBubble>[] {
  const canFoldWithCompletedWork =
    options.canFoldWithCompletedWork ?? defaultCanFoldWithCompletedWork;
  const isUserInputBoundary =
    options.isUserInputBoundary ?? defaultIsUserInputBoundary;
  const items: AgentChatFoldDisplayItem<TBubble>[] = [];
  let activeInput: TBubble | null = null;
  let pendingActivity: TBubble[] = [];
  let stillAtLeadingWindowActivity = true;

  function pushBubble(bubble: TBubble) {
    items.push({ kind: "bubble", id: bubble.id, bubble });
    stillAtLeadingWindowActivity = false;
  }

  function flushPendingActivity() {
    for (const bubble of pendingActivity) {
      pushBubble(bubble);
    }
    pendingActivity = [];
  }

  function pushFoldedActivity(outputBubble: TBubble) {
    if (pendingActivity.length === 0) {
      return;
    }

    items.push({
      kind: "foldedActivityGroup",
      id: `folded-${outputBubble.id}`,
      bubbles: pendingActivity,
    });
    pendingActivity = [];
    stillAtLeadingWindowActivity = false;
  }

  for (const bubble of bubbles) {
    if (isUserInputBoundary(bubble)) {
      flushPendingActivity();
      pushBubble(bubble);
      activeInput = bubble;
      continue;
    }

    if (options.isOutputBoundary(bubble)) {
      pushFoldedActivity(bubble);
      pushBubble(bubble);
      activeInput = null;
      continue;
    }

    if (
      canFoldWithCompletedWork(bubble) &&
      (activeInput ||
        stillAtLeadingWindowActivity ||
        pendingActivity.length > 0)
    ) {
      pendingActivity.push(bubble);
      continue;
    }

    flushPendingActivity();
    pushBubble(bubble);
  }

  flushPendingActivity();
  return items;
}

function defaultIsUserInputBoundary(bubble: AgentChatFoldBubble) {
  return bubble.role === "user" || bubble.role === "telegram";
}

function defaultCanFoldWithCompletedWork(bubble: AgentChatFoldBubble) {
  return !bubble.live;
}
