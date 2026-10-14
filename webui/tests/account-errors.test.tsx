import { afterEach, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ActorApiError } from "../src/lib/actor-api";
import { accountError, accountErrorCode, afterRejectedProof } from "../src/lib/account-errors";
import { AccountError } from "../src/components/account-error";
import i18n from "../src/lib/i18n";

afterEach(async () => { await i18n.changeLanguage("zh-CN"); });

test("a rejected password proof cannot turn a later network failure into a credential error", () => {
  for (const code of ["connection_failed", "login_timeout", "login_relay_unavailable", "login_device_unreachable", "login_protocol_error"]) {
    const error = accountError(code);
    expect(afterRejectedProof(error, ["device"])).toBe(error);
  }
  const exhausted = accountError("login_no_online_device");
  expect(accountErrorCode(afterRejectedProof(exhausted, ["device"]))).toBe("login_invalid_credentials");
  expect(afterRejectedProof(exhausted, [])).toBe(exhausted);
});

test("login UI shows translated short reasons and stable codes without raw diagnostics", async () => {
  const error = new ActorApiError("SECRET password /private/path ws://internal-host", 503, "login_relay_unavailable");
  await i18n.changeLanguage("zh-CN");
  let html = renderToStaticMarkup(<AccountError code={accountErrorCode(error)} />);
  expect(html).toContain("无法连接到公网连接服务");
  expect(html).toContain("login_relay_unavailable");
  expect(html).not.toContain("SECRET");expect(html).not.toContain("internal-host");
  await i18n.changeLanguage("en-US");
  html = renderToStaticMarkup(<AccountError code={accountErrorCode(error)} />);
  expect(html).toContain("Cannot reach the connection service");
  expect(html).toContain("login_relay_unavailable");
});

test("unknown native and crypto errors have a fixed safe fallback", async () => {
  await i18n.changeLanguage("en-US");
  for (const error of [new Error("SECRET"), "private diagnostic", new ActorApiError("SECRET", 500, "unknown_secret_code")]) {
    const code = accountErrorCode(error);
    expect(code).toBe("login_failed");
    const html = renderToStaticMarkup(<AccountError code={code} />);
    expect(html).toContain("Login failed");expect(html).not.toContain("SECRET");
  }
});
