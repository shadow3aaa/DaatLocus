import { type FormEvent, useState } from "react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/ui/button";
import {
  Field,
  FieldError,
  FieldGroup,
  FieldLabel,
} from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Spinner } from "@/components/ui/spinner";
import {
  clearStoredDaemonToken,
  getStoredDaemonToken,
  storeDaemonToken,
  verifyDaemonToken,
} from "@/lib/daemon-auth";

type LoginState = "idle" | "checking" | "authenticated" | "error";

export function LoginPage({
  onAuthenticated,
}: {
  onAuthenticated: () => void;
}) {
  const { t } = useTranslation();
  const [token, setToken] = useState(() => getStoredDaemonToken());
  const [loginState, setLoginState] = useState<LoginState>("idle");
  const [message, setMessage] = useState("");

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const trimmedToken = token.trim();

    if (!trimmedToken) {
      setLoginState("error");
      setMessage(t("login.enterToken"));
      return;
    }

    setLoginState("checking");
    setMessage(t("login.verifyingToken"));

    const result = await verifyDaemonToken(trimmedToken);
    if (result.ok) {
      storeDaemonToken(trimmedToken);
      setToken(trimmedToken);
      setLoginState("authenticated");
      setMessage(t("login.verifiedToken"));
      onAuthenticated();
      return;
    }

    clearStoredDaemonToken();
    setLoginState("error");
    setMessage(result.message);
  }

  const isChecking = loginState === "checking";
  const isError = loginState === "error";

  return (
    <section
      id="login"
      className="flex min-h-screen w-full flex-col bg-background lg:flex-row"
    >
      <div className="flex flex-col justify-start gap-10 p-8 md:p-12 lg:flex-1 lg:justify-between lg:p-16">
        <span className="text-sm font-semibold tracking-tight">
          Daat Locus
        </span>
        <div className="flex flex-col gap-5">
          <span className="font-mono text-[11px] uppercase tracking-[0.18em] text-muted-foreground">
            {t("document.signIn")}
          </span>
          <h1 className="text-5xl font-medium leading-none tracking-tight md:text-6xl">
            WebUI
          </h1>
          <p className="max-w-md text-lg leading-relaxed text-muted-foreground">
            {t("login.enterToken")}
          </p>
        </div>
        <span className="hidden font-mono text-[11px] uppercase tracking-[0.18em] text-muted-foreground lg:block">
          local daemon access
        </span>
      </div>

      <div className="flex items-center bg-muted/40 p-8 md:p-12 lg:flex-1 lg:p-16">
        <form onSubmit={handleSubmit} className="w-full max-w-sm">
          <FieldGroup>
            <Field data-invalid={isError} data-disabled={isChecking}>
              <FieldLabel htmlFor="daemon-token">
                {t("login.daemonToken")}
              </FieldLabel>
              <Input
                id="daemon-token"
                aria-invalid={isError}
                value={token}
                onChange={(event) => {
                  setToken(event.target.value);
                  setMessage("");
                  if (loginState !== "checking") {
                    setLoginState("idle");
                  }
                }}
                placeholder={t("login.tokenPlaceholder")}
                type="password"
                autoComplete="current-password"
                spellCheck={false}
                disabled={isChecking}
                required
              />
              <FieldError>{isError ? message : null}</FieldError>
            </Field>
            <Button type="submit" disabled={isChecking}>
              {isChecking ? <Spinner data-icon="inline-start" /> : null}
              {isChecking ? t("login.verifying") : t("login.submit")}
            </Button>
          </FieldGroup>
        </form>
      </div>
    </section>
  );
}
