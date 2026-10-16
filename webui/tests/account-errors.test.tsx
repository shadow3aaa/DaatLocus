import { expect, test } from "bun:test";
import { ActorApiError } from "../src/lib/actor-api";
import { accountError, accountErrorCode, afterRejectedProof } from "../src/lib/account-errors";

test("a rejected password proof cannot turn a later network failure into a credential error", () => {
  for (const code of ["connection_failed", "login_timeout", "login_relay_unavailable", "login_device_unreachable", "login_protocol_error"]) {
    const error = accountError(code);
    expect(afterRejectedProof(error, ["device"])).toBe(error);
  }
  const exhausted = accountError("login_no_online_device");
  expect(accountErrorCode(afterRejectedProof(exhausted, ["device"]))).toBe("login_invalid_credentials");
  expect(afterRejectedProof(exhausted, [])).toBe(exhausted);
});

test("unknown native and crypto errors map to an allowlisted failure code", () => {
  for (const error of [new Error("SECRET"), "private diagnostic", new ActorApiError("SECRET", 500, "unknown_secret_code")]) {
    const code = accountErrorCode(error);
    expect(code).toBe("login_failed");
  }
});
