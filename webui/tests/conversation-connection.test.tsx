import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ConversationConnection } from "../src/components/conversation-connection";
import type { ConversationConnectionMessage } from "../src/lib/stage-api";

const render = (message: ConversationConnectionMessage | null) => renderToStaticMarkup(
  <ConversationConnection message={message} />,
);

test("execution wait exposes an accessible inline status without a modal wait", () => {
  const html = render("conversation_pane.reconnecting");
  expect(html).not.toContain('inert=""');
  expect(html).not.toContain('aria-hidden="true"');
  expect(html.match(/role="status"/g)).toHaveLength(1);
  expect(html).toContain('aria-atomic="true"');
  const recovered = render(null);
  expect(recovered).not.toContain('inert=""');
  expect(recovered).not.toContain('aria-hidden="true"');
  expect(recovered).not.toContain('role="status"');
});
