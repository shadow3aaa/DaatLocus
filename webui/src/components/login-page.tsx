import { type FormEvent, useState } from "react";

import { type AuthStatus } from "@/components/app-navigation";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  getStoredDaemonToken,
  storeDaemonToken,
  verifyDaemonToken,
} from "@/lib/daemon-auth";

type LoginState = "idle" | "checking" | "authenticated" | "error";

export function LoginPage({
  onAuthStatusChange,
}: {
  onAuthStatusChange: (status: AuthStatus) => void;
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
      onAuthStatusChange("anonymous");
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
      onAuthStatusChange("authenticated");
      return;
    }

    setLoginState("error");
    setMessage(result.message);
    onAuthStatusChange("anonymous");
  }

  const isChecking = loginState === "checking";
  const isError = loginState === "error";

  return (
    <section
      id="login"
      className="mx-auto grid min-h-[calc(100vh-4rem)] w-full max-w-4xl items-center gap-8 px-6 py-10 md:grid-cols-[1fr_1.4fr]"
    >
      <h1 className="text-5xl font-semibold tracking-tight md:text-6xl">Login</h1>

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
              onAuthStatusChange(event.target.value.trim() ? "saved" : "anonymous");
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

        <span className="sr-only" aria-live="polite">
          {message}
        </span>
      </form>
    </section>
  );
}
