import { type FormEvent, useState } from "react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
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
      className="mx-auto grid min-h-screen w-full max-w-4xl items-center gap-8 px-6 py-10 md:grid-cols-[1fr_1.4fr]"
    >
      <h1 className="text-5xl font-semibold tracking-tight md:text-6xl">Login</h1>

      <div className="flex w-full flex-col gap-3">
        <form className="flex w-full flex-col gap-3 sm:flex-row" onSubmit={handleSubmit}>
          <Input
            aria-label="Daemon token"
            aria-invalid={isError}
            className="h-11 flex-1"
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

          <Button className="h-11 px-8" type="submit" disabled={isChecking}>
            {isChecking ? "Verifying…" : "Login"}
          </Button>
        </form>

        {message ? (
          <p
            className={`text-sm ${
              isError ? "text-destructive" : "text-muted-foreground"
            }`}
            role={isError ? "alert" : "status"}
            aria-live="polite"
          >
            {message}
          </p>
        ) : null}
      </div>
    </section>
  );
}
