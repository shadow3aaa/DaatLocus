import { useEffect, useState } from "react";
import { TriangleAlertIcon } from "lucide-react";

import {
  AgentPersonalizationEditor,
  ModelAccessEditor,
  agentPersonalizationEditorValueToSetupRequest,
  createDefaultAgentPersonalizationEditorValue,
  createDefaultModelAccessEditorValue,
  modelAccessEditorValueToSetupRequest,
  setupConfigRequestToAgentPersonalizationEditorValue,
  setupConfigRequestToModelAccessEditorValue,
  type AgentPersonalizationEditorValue,
  type ModelAccessEditorValue,
} from "@/components/setup-page";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Skeleton } from "@/components/ui/skeleton";
import {
  fetchSetupConfig,
  saveSetupConfig,
  type ConfigReadinessReport,
  type SetupConfigRequest,
  type SetupConfigResponse,
} from "@/lib/daemon-api";

type LoadState = "idle" | "loading" | "error";
type SaveState = "idle" | "pending" | "saving" | "saved" | "error";
const SETTINGS_AUTOSAVE_DELAY_MS = 800;

type SettingsPageProps = {
  mockSetupConfig?: SetupConfigResponse;
  onSaveSetupConfig?: (
    request: SetupConfigRequest,
    options?: { signal?: AbortSignal },
  ) => Promise<ConfigReadinessReport>;
};

export function SettingsPage({
  mockSetupConfig,
  onSaveSetupConfig = saveSetupConfig,
}: SettingsPageProps = {}) {
  const [baseConfig, setBaseConfig] = useState<SetupConfigRequest | null>(
    () => mockSetupConfig?.config ?? null,
  );
  const [readiness, setReadiness] = useState<ConfigReadinessReport | null>(
    () => mockSetupConfig?.readiness ?? null,
  );
  const [agentPersonalization, setAgentPersonalization] =
    useState<AgentPersonalizationEditorValue>(() =>
      mockSetupConfig
        ? setupConfigRequestToAgentPersonalizationEditorValue(
            mockSetupConfig.config,
          )
        : createDefaultAgentPersonalizationEditorValue(),
    );
  const [modelAccess, setModelAccess] = useState<ModelAccessEditorValue>(() =>
    mockSetupConfig
      ? setupConfigRequestToModelAccessEditorValue(mockSetupConfig.config)
      : createDefaultModelAccessEditorValue(),
  );
  const [loadState, setLoadState] = useState<LoadState>(
    () => (mockSetupConfig ? "idle" : "loading"),
  );
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saveState, setSaveState] = useState<SaveState>("idle");
  const [saveError, setSaveError] = useState<string | null>(null);
  const [isDirty, setIsDirty] = useState(false);

  const isLoading = loadState === "loading";
  const showSettingsAlerts = Boolean(
    loadError ||
      readiness?.recovery_note ||
      (saveState === "error" && saveError),
  );

  useEffect(() => {
    if (mockSetupConfig) {
      hydrateSettings(mockSetupConfig.config, mockSetupConfig.readiness);
      return;
    }

    const controller = new AbortController();
    void loadSettings(controller.signal);

    return () => controller.abort();
  }, [mockSetupConfig]);

  useEffect(() => {
    if (!isDirty || isLoading) {
      return;
    }

    const validationError = validateSettingsDraft(modelAccess);
    if (validationError) {
      setSaveState("error");
      setSaveError(validationError);
      return;
    }

    let cancelled = false;
    const controller = new AbortController();
    setSaveState("pending");
    setSaveError(null);

    const timeoutId = window.setTimeout(() => {
      const request = buildSettingsRequest();
      setSaveState("saving");
      setSaveError(null);

      void onSaveSetupConfig(request, { signal: controller.signal })
        .then((nextReadiness) => {
          if (cancelled) {
            return;
          }
          setBaseConfig(request);
          setReadiness(nextReadiness);
          setSaveState("saved");
          setSaveError(null);
          setIsDirty(false);
        })
        .catch((error) => {
          if (cancelled || controller.signal.aborted) {
            return;
          }
          setSaveState("error");
          setSaveError(error instanceof Error ? error.message : String(error));
        });
    }, SETTINGS_AUTOSAVE_DELAY_MS);

    return () => {
      cancelled = true;
      controller.abort();
      window.clearTimeout(timeoutId);
    };
  }, [
    agentPersonalization,
    baseConfig,
    isDirty,
    isLoading,
    modelAccess,
    onSaveSetupConfig,
  ]);

  function hydrateSettings(
    config: SetupConfigRequest,
    nextReadiness: ConfigReadinessReport,
  ) {
    setBaseConfig(config);
    setReadiness(nextReadiness);
    setAgentPersonalization(setupConfigRequestToAgentPersonalizationEditorValue(config));
    setModelAccess(setupConfigRequestToModelAccessEditorValue(config));
    setLoadState("idle");
    setLoadError(null);
    setSaveState("idle");
    setSaveError(null);
    setIsDirty(false);
  }

  async function loadSettings(signal?: AbortSignal) {
    if (mockSetupConfig) {
      hydrateSettings(mockSetupConfig.config, mockSetupConfig.readiness);
      return;
    }

    setLoadState("loading");
    setLoadError(null);

    try {
      const nextSetupConfig = await fetchSetupConfig({ signal });
      hydrateSettings(nextSetupConfig.config, nextSetupConfig.readiness);
    } catch (error) {
      if (signal?.aborted) {
        return;
      }
      setLoadState("error");
      setLoadError(error instanceof Error ? error.message : String(error));
    }
  }

  function markDirty() {
    setIsDirty(true);
    setSaveState("pending");
    setSaveError(null);
  }

  function buildSettingsRequest(): SetupConfigRequest {
    const request: SetupConfigRequest = {
      ...(baseConfig ?? {}),
      ...agentPersonalizationEditorValueToSetupRequest(agentPersonalization),
      ...modelAccessEditorValueToSetupRequest(modelAccess),
    };
    delete request.daemon_port;
    return request;
  }


  return (
    <section
      id="settings"
      aria-label="Settings"
      className="min-h-screen w-full px-6"
    >
      <div aria-hidden="true" className="h-20 md:h-8" />
      <div className="mx-auto flex w-full max-w-5xl flex-col gap-12">

        {showSettingsAlerts ? (
          <div className="flex flex-col gap-4">
            {loadError ? (
              <Alert variant="destructive">
                <TriangleAlertIcon aria-hidden="true" />
                <AlertTitle>Unable to load settings</AlertTitle>
                <AlertDescription>{loadError}</AlertDescription>
              </Alert>
            ) : null}

            {readiness?.recovery_note ? (
              <Alert>
                <TriangleAlertIcon aria-hidden="true" />
                <AlertTitle>Configuration file restored</AlertTitle>
                <AlertDescription>{readiness.recovery_note}</AlertDescription>
              </Alert>
            ) : null}

            {saveState === "error" && saveError ? (
              <Alert variant="destructive">
                <TriangleAlertIcon aria-hidden="true" />
                <AlertTitle>Unable to save settings</AlertTitle>
                <AlertDescription>{saveError}</AlertDescription>
              </Alert>
            ) : null}
          </div>
        ) : null}

        {isLoading && !baseConfig ? (
          <SettingsSkeleton />
        ) : (
          <>
            <AgentPersonalizationEditor
              value={agentPersonalization}
              onChange={(nextValue) => {
                setAgentPersonalization(nextValue);
                markDirty();
              }}
            />

            <ModelAccessEditor
              value={modelAccess}
              onChange={(nextValue) => {
                setModelAccess(nextValue);
                markDirty();
              }}
              providerDescription="Tune the secure access layer behind the agent's model capability."
              modelDescription="Shape available model capacity into a dependable runtime catalog."
              selectionDescription="Set the operating balance between depth, speed, and everyday work."
            />
          </>
        )}
      </div>
      <div aria-hidden="true" className="h-20 md:h-8" />
    </section>
  );
}
function SettingsSkeleton() {
  return (
    <div className="flex flex-col gap-10">
      {Array.from({ length: 4 }, (_, sectionIndex) => (
        <section key={sectionIndex} className="flex flex-col gap-5">
          <div className="flex flex-col gap-2">
            <Skeleton className="h-8 w-40" />
            <Skeleton className="h-4 w-full max-w-2xl" />
          </div>
          <div className="divide-y border-y">
            {Array.from({ length: 3 }, (_, rowIndex) => (
              <div key={rowIndex} className="flex flex-col gap-2 py-4">
                <Skeleton className="h-5 w-56" />
                <Skeleton className="h-4 w-full max-w-xl" />
              </div>
            ))}
          </div>
        </section>
      ))}
    </div>
  );
}

function validateSettingsDraft(modelAccess: ModelAccessEditorValue) {
  if (modelAccess.providers.length === 0) {
    return "Add at least one provider.";
  }
  if (modelAccess.models.length === 0) {
    return "Add at least one model.";
  }
  if (!modelAccess.models.some((model) => model.name === modelAccess.mainModel)) {
    return "Select a valid main model.";
  }
  if (
    !modelAccess.models.some((model) => model.name === modelAccess.efficientModel)
  ) {
    return "Select a valid efficient model.";
  }
  return null;
}
