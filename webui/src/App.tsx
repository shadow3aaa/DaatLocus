import { useState } from "react";

import { AppNavigation } from "@/components/app-navigation";
import { LoginPage } from "@/components/login-page";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { clearStoredDaemonToken, getStoredDaemonToken } from "@/lib/daemon-auth";

export default function App() {
  const [isAuthenticated, setIsAuthenticated] = useState(() =>
    Boolean(getStoredDaemonToken()),
  );

  function handleLogout() {
    clearStoredDaemonToken();
    setIsAuthenticated(false);
  }

  return (
    <main className="min-h-screen bg-background text-foreground">
      <AppNavigation isAuthenticated={isAuthenticated} />
      {isAuthenticated ? (
        <AuthenticatedHome onLogout={handleLogout} />
      ) : (
        <LoginPage onAuthenticated={() => setIsAuthenticated(true)} />
      )}
    </main>
  );
}

function AuthenticatedHome({ onLogout }: { onLogout: () => void }) {
  return (
    <section
      id="status"
      className="mx-auto grid min-h-[calc(100vh-4rem)] w-full max-w-4xl items-center gap-8 px-6 py-10 md:grid-cols-[1fr_1.4fr]"
    >
      <div className="space-y-3">
        <p className="text-sm font-medium text-muted-foreground">Connected</p>
        <h1 className="text-5xl font-semibold tracking-tight md:text-6xl">
          Status
        </h1>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Daemon token verified</CardTitle>
          <CardDescription>
            You are signed in. The upcoming Status, Tasks, and Logs pages can
            now reuse the saved daemon token.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <Button variant="outline" type="button" onClick={onLogout}>
            Log out
          </Button>
        </CardContent>
      </Card>
    </section>
  );
}
