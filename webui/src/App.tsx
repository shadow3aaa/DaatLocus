import { useEffect, useState } from "react";

import { AppNavigation } from "@/components/app-navigation";
import { LoginPage } from "@/components/login-page";
import { LogsPage } from "@/components/logs-page";
import { AgentPage, StatusPage } from "@/components/status-page";
import { getStoredDaemonToken } from "@/lib/daemon-auth";

type AppPage = "agent" | "status" | "logs";

export default function App() {
  const [isAuthenticated, setIsAuthenticated] = useState(() =>
    Boolean(getStoredDaemonToken()),
  );
  const [activePage, setActivePage] = useState(getCurrentPage);

  useEffect(() => {
    function updateActivePage() {
      setActivePage(getCurrentPage());
    }

    updateActivePage();
    window.addEventListener("hashchange", updateActivePage);

    return () => window.removeEventListener("hashchange", updateActivePage);
  }, []);

  return (
    <main className="min-h-screen bg-background text-foreground">
      <AppNavigation isAuthenticated={isAuthenticated} />
      {isAuthenticated ? (
        renderAuthenticatedPage(activePage)
      ) : (
        <LoginPage onAuthenticated={() => setIsAuthenticated(true)} />
      )}
    </main>
  );
}

function renderAuthenticatedPage(activePage: AppPage) {
  switch (activePage) {
    case "status":
      return <StatusPage />;
    case "logs":
      return <LogsPage />;
    case "agent":
    default:
      return <AgentPage />;
  }
}

function getCurrentPage(): AppPage {
  if (typeof window === "undefined") {
    return "agent";
  }

  if (window.location.hash === "#status") {
    return "status";
  }
  if (window.location.hash === "#logs") {
    return "logs";
  }
  return "agent";
}
