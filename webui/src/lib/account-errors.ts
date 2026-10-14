import { ActorApiError } from "./actor-api";
import { apiErrorText } from "./i18n";

const codes = new Set([
  "login_connection_required", "login_relay_unavailable", "login_no_online_device",
  "login_device_unreachable", "login_timeout", "login_invalid_credentials", "login_expired",
  "login_device_revoked", "login_account_changed", "login_protocol_error", "login_import_rejected",
  "login_busy", "login_service_unavailable", "login_client_unavailable", "login_retry_exhausted",
  "login_failed", "login_password_mismatch", "login_password_too_short",
  "login_limited", "registration_pending", "connection_failed", "invalid_request",
  "forbidden", "internal", "not_ready", "account_unavailable",
]);

export function accountError(code: string) {
  const safe = codes.has(code) ? code : "login_failed";
  return new ActorApiError(apiErrorText(safe), 0, safe);
}
export function accountErrorCode(error: unknown): string {
  return error instanceof ActorApiError && codes.has(error.code) ? error.code : "login_failed";
}
/** Only exhausted discovery after a rejected proof can be described as bad credentials. */
export function afterRejectedProof(error: unknown, excluded: string[]): unknown {
  return excluded.length && error instanceof ActorApiError
    && ["login_no_online_device", "login_connection_required"].includes(error.code)
    ? accountError("login_invalid_credentials") : error;
}
