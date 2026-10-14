import { useTranslation } from "react-i18next";
import { apiErrorText, t } from "@/lib/i18n";
import { Badge } from "@/components/ui/badge";
import type { PublicLoginStatus } from "@/lib/stage-api";

const labels = {
  not_configured: "account_editor.public_login_not_configured",
  waiting_for_account: "account_editor.public_login_waiting_for_account",
  connecting: "account_editor.public_login_connecting",
  online: "account_editor.public_login_online",
  reconnecting: "account_editor.public_login_reconnecting",
};
export function PublicLoginStatusView({status}: {status: PublicLoginStatus | null}) {
  useTranslation();
  return <div className="flex flex-col items-end gap-1" role="status">
    <Badge variant="outline">{t(status ? labels[status.state] ?? "account_editor.status_unknown" : "account_editor.status_unknown")}</Badge>
    {status?.error_code && <small className="text-muted-foreground text-xs">{apiErrorText(status.error_code)} ({status.error_code})</small>}
  </div>;
}
