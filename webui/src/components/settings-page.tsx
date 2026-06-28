import type { TFunction } from "i18next";
import { useEffect, useState } from "react";
import { Trans, useTranslation } from "react-i18next";
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
import {
  Field,
  FieldContent,
  FieldDescription,
  FieldGroup,
  FieldLabel,
} from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import {
  fetchSetupConfig,
  saveSetupConfig,
  type ConfigReadinessReport,
  type SetupConfigRequest,
  type SetupConfigResponse,
} from "@/lib/daemon-api";
import {
  getCurrentWebUiLanguage,
  normalizeWebUiLocale,
  setWebUiLanguage,
  webUiLocaleOptions,
  type WebUiLocale,
} from "@/lib/i18n";

type LoadState = "idle" | "loading" | "error";
type SaveState = "idle" | "pending" | "saving" | "saved" | "error";
const SETTINGS_AUTOSAVE_DELAY_MS = 800;

type InterfaceSettingsValue = {
  locale: WebUiLocale;
};

type TelegramSettingsValue = {
  enabled: boolean;
  botToken: string;
};

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
  const { t } = useTranslation();
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
  const [interfaceSettings, setInterfaceSettings] =
    useState<InterfaceSettingsValue>(() =>
      mockSetupConfig
        ? setupConfigRequestToInterfaceSettingsValue(mockSetupConfig.config)
        : createDefaultInterfaceSettingsValue(),
    );
  const [telegramSettings, setTelegramSettings] =
    useState<TelegramSettingsValue>(() =>
      mockSetupConfig
        ? setupConfigRequestToTelegramSettingsValue(mockSetupConfig.config)
        : createDefaultTelegramSettingsValue(),
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

    const validationError = validateSettingsDraft(modelAccess, t);
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
    interfaceSettings,
    isDirty,
    isLoading,
    modelAccess,
    onSaveSetupConfig,
    telegramSettings,
    t,
  ]);

  function hydrateSettings(
    config: SetupConfigRequest,
    nextReadiness: ConfigReadinessReport,
  ) {
    const nextInterfaceSettings = setupConfigRequestToInterfaceSettingsValue(config);

    setBaseConfig(config);
    setReadiness(nextReadiness);
    setAgentPersonalization(
      setupConfigRequestToAgentPersonalizationEditorValue(config),
    );
    setModelAccess(setupConfigRequestToModelAccessEditorValue(config));
    setInterfaceSettings(nextInterfaceSettings);
    void setWebUiLanguage(nextInterfaceSettings.locale);
    setTelegramSettings(setupConfigRequestToTelegramSettingsValue(config));
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
      ...interfaceSettingsValueToSetupRequest(interfaceSettings),
      ...telegramSettingsValueToSetupRequest(telegramSettings),
    };
    delete request.daemon_port;
    return request;
  }

  return (
    <section
      id="settings"
      aria-label={t("settings.pageAria")}
      className="min-h-screen w-full px-6"
    >
      <div aria-hidden="true" className="h-20 md:h-8" />
      <div className="mx-auto flex w-full max-w-5xl flex-col gap-12">

        {showSettingsAlerts ? (
          <div className="flex flex-col gap-4">
            {loadError ? (
              <Alert variant="destructive">
                <TriangleAlertIcon aria-hidden="true" />
                <AlertTitle>{t("settings.unableToLoad")}</AlertTitle>
                <AlertDescription>{loadError}</AlertDescription>
              </Alert>
            ) : null}

            {readiness?.recovery_note ? (
              <Alert>
                <TriangleAlertIcon aria-hidden="true" />
                <AlertTitle>{t("settings.configRestored")}</AlertTitle>
                <AlertDescription>{readiness.recovery_note}</AlertDescription>
              </Alert>
            ) : null}

            {saveState === "error" && saveError ? (
              <Alert variant="destructive">
                <TriangleAlertIcon aria-hidden="true" />
                <AlertTitle>{t("settings.unableToSave")}</AlertTitle>
                <AlertDescription>{saveError}</AlertDescription>
              </Alert>
            ) : null}
          </div>
        ) : null}

        {isLoading && !baseConfig ? (
          <SettingsSkeleton />
        ) : (
          <>
            <InterfaceSettingsEditor
              value={interfaceSettings}
              onChange={(nextValue) => {
                setInterfaceSettings(nextValue);
                void setWebUiLanguage(nextValue.locale);
                markDirty();
              }}
            />

            <AgentPersonalizationEditor
              value={agentPersonalization}
              onChange={(nextValue) => {
                setAgentPersonalization(nextValue);
                markDirty();
              }}
              fieldGroupClassName="max-w-none"
            />

            <ModelAccessEditor
              value={modelAccess}
              onChange={(nextValue) => {
                setModelAccess(nextValue);
                markDirty();
              }}
              fieldGroupClassName="max-w-none"
              providerDescription={t("settings.modelAccess.providerDescription")}
              modelDescription={t("settings.modelAccess.modelDescription")}
              selectionDescription={t("settings.modelAccess.selectionDescription")}
            />

            <TelegramSettingsEditor
              value={telegramSettings}
              onChange={(nextValue) => {
                setTelegramSettings(nextValue);
                markDirty();
              }}
            />
          </>
        )}
      </div>
      <div aria-hidden="true" className="h-20 md:h-8" />
    </section>
  );
}

function createDefaultInterfaceSettingsValue(): InterfaceSettingsValue {
  return {
    locale: getCurrentWebUiLanguage(),
  };
}

function setupConfigRequestToInterfaceSettingsValue(
  request: SetupConfigRequest,
): InterfaceSettingsValue {
  return {
    locale: request.locale
      ? normalizeWebUiLocale(request.locale)
      : getCurrentWebUiLanguage(),
  };
}

function interfaceSettingsValueToSetupRequest(
  value: InterfaceSettingsValue,
): Pick<SetupConfigRequest, "locale"> {
  return {
    locale: value.locale,
  };
}

function InterfaceSettingsEditor({
  value,
  onChange,
}: {
  value: InterfaceSettingsValue;
  onChange: (value: InterfaceSettingsValue) => void;
}) {
  const { t } = useTranslation();

  return (
    <section className="flex flex-col gap-6">
      <div className="flex flex-col gap-2">
        <h2 className="text-3xl font-medium tracking-normal">
          {t("settings.interface.title")}
        </h2>
        <p className="max-w-2xl text-base text-muted-foreground">
          {t("settings.interface.description")}
        </p>
      </div>

      <FieldGroup className="max-w-xl">
        <Field>
          <FieldLabel htmlFor="webui-settings-language">
            {t("settings.interface.languageLabel")}
          </FieldLabel>
          <Select
            value={value.locale}
            onValueChange={(locale) =>
              onChange({ locale: locale as WebUiLocale })
            }
          >
            <SelectTrigger id="webui-settings-language" className="w-full">
              <SelectValue
                placeholder={t("settings.interface.languagePlaceholder")}
              />
            </SelectTrigger>
            <SelectContent>
              <SelectGroup>
                {webUiLocaleOptions.map((language) => (
                  <SelectItem key={language.value} value={language.value}>
                    {language.label}
                  </SelectItem>
                ))}
              </SelectGroup>
            </SelectContent>
          </Select>
          <FieldDescription>
            {t("settings.interface.languageDescription")}
          </FieldDescription>
        </Field>
      </FieldGroup>
    </section>
  );
}

function createDefaultTelegramSettingsValue(): TelegramSettingsValue {
  return {
    enabled: false,
    botToken: "",
  };
}

function setupConfigRequestToTelegramSettingsValue(
  request: SetupConfigRequest,
): TelegramSettingsValue {
  return {
    enabled: request.telegram_enabled ?? false,
    botToken: request.telegram_bot_token ?? "",
  };
}

function telegramSettingsValueToSetupRequest(
  value: TelegramSettingsValue,
): Pick<SetupConfigRequest, "telegram_enabled" | "telegram_bot_token"> {
  return {
    telegram_enabled: value.enabled,
    telegram_bot_token: value.botToken,
  };
}

function TelegramSettingsEditor({
  value,
  onChange,
}: {
  value: TelegramSettingsValue;
  onChange: (value: TelegramSettingsValue) => void;
}) {
  const { t } = useTranslation();

  return (
    <section className="flex flex-col gap-6">
      <div className="flex flex-col gap-2">
        <h2 className="text-3xl font-medium tracking-normal">
          {t("settings.telegram.title")}
        </h2>
        <p className="max-w-2xl text-base text-muted-foreground">
          {t("settings.telegram.description")}
        </p>
      </div>

      <FieldGroup>
        <Field orientation="horizontal">
          <FieldContent>
            <FieldLabel htmlFor="telegram-settings-enabled">
              {t("settings.telegram.enableLabel")}
            </FieldLabel>
            <FieldDescription>
              {t("settings.telegram.enableDescription")}
            </FieldDescription>
          </FieldContent>
          <Switch
            id="telegram-settings-enabled"
            checked={value.enabled}
            onCheckedChange={(enabled) => onChange({ ...value, enabled })}
            aria-label={t("settings.telegram.enableAria")}
          />
        </Field>

        <Field>
          <FieldLabel htmlFor="telegram-settings-bot-token">
            {t("settings.telegram.botToken")}
          </FieldLabel>
          <Input
            id="telegram-settings-bot-token"
            type="password"
            value={value.botToken}
            onChange={(event) =>
              onChange({ ...value, botToken: event.target.value })
            }
            placeholder="123456789:AA..."
            autoComplete="off"
            spellCheck={false}
          />
          <FieldDescription>
            <Trans
              i18nKey="settings.telegram.botTokenDescription"
              components={{
                botFather: (
                  <a
                    href="https://t.me/BotFather"
                    target="_blank"
                    rel="noreferrer"
                  />
                ),
              }}
            />
          </FieldDescription>
        </Field>
      </FieldGroup>
    </section>
  );
}

function SettingsSkeleton() {
  return (
    <div className="flex flex-col gap-10">
      {Array.from({ length: 5 }, (_, sectionIndex) => (
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

function validateSettingsDraft(modelAccess: ModelAccessEditorValue, t: TFunction) {
  if (modelAccess.providers.length === 0) {
    return t("settings.validation.providerRequired");
  }
  if (modelAccess.models.length === 0) {
    return t("settings.validation.modelRequired");
  }
  if (!modelAccess.models.some((model) => model.name === modelAccess.mainModel)) {
    return t("settings.validation.mainModelRequired");
  }
  if (
    !modelAccess.models.some((model) => model.name === modelAccess.efficientModel)
  ) {
    return t("settings.validation.efficientModelRequired");
  }
  return null;
}
