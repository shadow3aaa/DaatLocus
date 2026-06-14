import { type FormEvent, useState } from "react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardAction,
  CardContent,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import {
  Field,
  FieldError,
  FieldGroup,
  FieldLabel,
} from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Spinner } from "@/components/ui/spinner";
import {
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
  const [token, setToken] = useState(() => getStoredDaemonToken());
  const [loginState, setLoginState] = useState<LoginState>("idle");
  const [message, setMessage] = useState("");

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const trimmedToken = token.trim();

    if (!trimmedToken) {
      setLoginState("error");
      setMessage("Enter the daemon token.");
      return;
    }

    setLoginState("checking");
    setMessage("Verifying token…");

    const result = await verifyDaemonToken(trimmedToken);
    if (result.ok) {
      storeDaemonToken(trimmedToken);
      setToken(trimmedToken);
      setLoginState("authenticated");
      setMessage("Token verified. Future pages will reuse this token.");
      onAuthenticated();
      return;
    }

    setLoginState("error");
    setMessage(result.message);
  }

  const isChecking = loginState === "checking";
  const isError = loginState === "error";

  return (
    <section
      id="login"
      className="flex min-h-screen w-full items-center justify-center bg-background px-6 py-10"
    >
      <Card className="w-full max-w-md">
        <CardHeader>
          <CardTitle>Daat Locus</CardTitle>
          <CardAction>
            <Badge variant="secondary">WebUI</Badge>
          </CardAction>
        </CardHeader>
        <CardContent>
          <form onSubmit={handleSubmit}>
            <FieldGroup>
              <Field data-invalid={isError} data-disabled={isChecking}>
                <FieldLabel htmlFor="daemon-token">Daemon token</FieldLabel>
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
                  placeholder="Token"
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
                {isChecking ? "Verifying" : "Login"}
              </Button>
            </FieldGroup>
          </form>
        </CardContent>
      </Card>
    </section>
  );
}
