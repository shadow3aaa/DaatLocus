import { useTranslation } from "react-i18next";
import { apiErrorText } from "@/lib/i18n";

export function AccountError({ code }: {code: string | null}) {
  useTranslation();
  return code ? <span data-error-code={code}>{apiErrorText(code)} <small className="text-muted-foreground text-xs">({code})</small></span> : null;
}
