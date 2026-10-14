import { t } from "./i18n";

const labels: Record<string, string> = {
  model_authentication_failed: "conversation_turns.model_authentication_failed",
  model_access_denied: "conversation_turns.model_access_denied",
  model_certificate_revoked: "conversation_turns.model_certificate_revoked",
  model_certificate_invalid: "conversation_turns.model_certificate_invalid",
  model_connection_failed: "conversation_turns.model_connection_failed",
  model_request_timeout: "conversation_turns.model_request_timeout",
  model_rate_limited: "conversation_turns.model_rate_limited",
  model_service_unavailable: "conversation_turns.model_service_unavailable",
  model_unavailable: "conversation_turns.model_unavailable",
  model_region_unavailable: "conversation_turns.model_region_unavailable",
  model_request_failed: "conversation_turns.model_request_failed",
};
/** Render failure facts; private provider diagnostics are never conversation prose. */
export function conversationFailureMessage(error: string | null): string {
  if (error === "App call approval was denied: Denied by owner") return t("conversation_turns.action_not_approved");
  const label = error && Object.hasOwn(labels, error) ? labels[error] : null;
  return t(label || "conversation_turns.this_task_is_incomplete_send_a_message_to_continue");
}
