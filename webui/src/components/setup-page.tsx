import {
  type FormEvent,
  type ReactNode,
  useEffect,
  useMemo,
  useState,
} from "react";
import {
  ArrowRightIcon,
  CheckIcon,
  PencilIcon,
  PlusIcon,
  RotateCcwIcon,
  Trash2Icon,
  TriangleAlertIcon,
} from "lucide-react";

import { AgentExpression } from "@/components/agent-expression";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Field,
  FieldDescription,
  FieldError,
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
import { Spinner } from "@/components/ui/spinner";
import {
  completeSetupProviderAuthDevice,
  discoverSetupModels,
  runSetupProviderAuth,
  saveSetupConfig,
  startSetupProviderAuthDevice,
  type ConfigReadinessReport,
  type SetupConfigRequest,
  type SetupDiscoveredModel,
  type SetupModelRequest,
  type SetupProviderAuthStartResponse,
  type SetupProviderKind,
  type SetupProviderRequest,
} from "@/lib/daemon-api";
import { cn } from "@/lib/utils";

type SaveState = "idle" | "saving" | "error";
type SetupStep = "intro" | "personalization" | "configuration";
type ProviderDialogMode = "add" | "edit";
type ModelDialogMode = "add" | "edit";
type ProviderAuthState = "idle" | "running" | "device_pending" | "success" | "error";
type ThinkingSelection = "__unset__" | "__custom__" | string;

type CodexAuthMethod =
  | "browser_login"
  | "device_login"
  | "import_local_codex"
  | "import_auth_file"
  | "existing_auth_file";

type GithubAuthMethod = "device_login" | "manual_token" | "env_token";
type SupportsVisionValue = "auto" | "true" | "false";

export type SetupProviderDraft = {
  id: string;
  name: string;
  kind: SetupProviderKind;
  apiKey: string;
  baseUrl: string;
  keepAlive: string;
  codexAuthMethod: CodexAuthMethod;
  codexAuthFile: string;
  githubAuthMethod: GithubAuthMethod;
};

export type SetupModelDraft = {
  id: string;
  name: string;
  providerName: string;
  modelId: string;
  contextWindowTokens: string;
  maxCompletionTokens: string;
  supportsVision: SupportsVisionValue;
  thinkingBudget: string;
  source?: SetupModelRequest;
};

type ProviderDialogState = {
  mode: ProviderDialogMode;
  provider: SetupProviderDraft | null;
};

type ModelDialogState = {
  mode: ModelDialogMode;
  model: SetupModelDraft | null;
};

export type ModelAccessEditorValue = {
  providers: SetupProviderDraft[];
  models: SetupModelDraft[];
  mainModel: string;
  efficientModel: string;
};
export type AgentPersonalizationEditorValue = {
  personaName: string;
  personaLanguage: string;
};


type ModelAccessEditorProps = {
  value: ModelAccessEditorValue;
  onChange: (value: ModelAccessEditorValue) => void;
  submitSlot?: ReactNode;
  providerDescription?: string;
  modelDescription?: string;
  selectionDescription?: string;
};

type AgentPersonalizationEditorProps = {
  value: AgentPersonalizationEditorValue;
  onChange: (value: AgentPersonalizationEditorValue) => void;
  title?: string;
  description?: string | null;
  showHeader?: boolean;
  className?: string;
};

type SetupPageProps = {
  readiness: ConfigReadinessReport;
  onReadinessChanged: (readiness: ConfigReadinessReport) => void;
  onSaveSetupConfig?: (
    request: SetupConfigRequest,
  ) => Promise<ConfigReadinessReport>;
};

const PROVIDER_KIND_OPTIONS: Array<{
  value: SetupProviderKind;
  label: string;
  description: string;
}> = [
  {
    value: "openai",
    label: "OpenAI",
    description: "Use an API key with OpenAI Responses-compatible access.",
  },
  {
    value: "openai_codex_oauth",
    label: "OpenAI Codex",
    description: "Use a ChatGPT Codex OAuth account file.",
  },
  {
    value: "github_copilot",
    label: "GitHub Copilot",
    description: "Use a GitHub Copilot account token.",
  },
  {
    value: "openai_compatible",
    label: "OpenAI compatible",
    description: "Use an API key with a custom base URL.",
  },
  {
    value: "ollama",
    label: "Ollama local",
    description: "Use a local Ollama endpoint.",
  },
  {
    value: "ollama_cloud",
    label: "Ollama Cloud",
    description: "Use an Ollama Cloud API key.",
  },
];

const CODEX_AUTH_METHODS: Array<{
  value: CodexAuthMethod;
  label: string;
  description: string;
}> = [
  {
    value: "browser_login",
    label: "Browser login",
    description: "Open the OpenAI authorization page and write this provider's OAuth file.",
  },
  {
    value: "device_login",
    label: "Device code login",
    description: "Show a device code and complete authorization in the browser.",
  },
  {
    value: "import_local_codex",
    label: "Import local Codex",
    description: "Read auth.json from the local Codex CLI.",
  },
  {
    value: "import_auth_file",
    label: "Import auth.json",
    description: "Import from a selected Codex auth.json path.",
  },
  {
    value: "existing_auth_file",
    label: "Use existing Daat Locus OAuth file",
    description: "Keep or manually place the OAuth file for this provider.",
  },
];

const GITHUB_AUTH_METHODS: Array<{
  value: GithubAuthMethod;
  label: string;
  description: string;
}> = [
  {
    value: "device_login",
    label: "Device code login",
    description: "Get a Copilot access token through the GitHub device flow.",
  },
  {
    value: "manual_token",
    label: "Manual token",
    description: "Paste a GitHub token.",
  },
  {
    value: "env_token",
    label: "Environment variable",
    description: "Save a $GITHUB_TOKEN reference.",
  },
];

const PERSONA_LANGUAGES = [
  { value: "zh-CN", label: "Simplified Chinese", greeting: "你好" },
  { value: "zh-TW", label: "Traditional Chinese", greeting: "你好" },
  { value: "en-US", label: "English", greeting: "Hello" },
  { value: "ja-JP", label: "Japanese", greeting: "こんにちは" },
  { value: "ko-KR", label: "Korean", greeting: "안녕하세요" },
  { value: "fr-FR", label: "French", greeting: "Bonjour" },
  { value: "de-DE", label: "German", greeting: "Hallo" },
  { value: "es-ES", label: "Spanish", greeting: "Hola" },
  { value: "pt-BR", label: "Portuguese (Brazil)", greeting: "Olá" },
  { value: "ru-RU", label: "Russian", greeting: "Привет" },
  { value: "it-IT", label: "Italian", greeting: "Ciao" },
  { value: "nl-NL", label: "Dutch", greeting: "Hallo" },
  { value: "tr-TR", label: "Turkish", greeting: "Merhaba" },
  { value: "pl-PL", label: "Polish", greeting: "Cześć" },
  { value: "uk-UA", label: "Ukrainian", greeting: "Привіт" },
  { value: "ar", label: "Arabic", greeting: "مرحبا" },
  { value: "hi-IN", label: "Hindi", greeting: "नमस्ते" },
  { value: "id-ID", label: "Indonesian", greeting: "Halo" },
  { value: "vi-VN", label: "Vietnamese", greeting: "Xin chào" },
  { value: "th-TH", label: "Thai", greeting: "สวัสดี" },
];

const DEFAULT_CONTEXT_WINDOW_TOKENS = "200000";
const DEFAULT_MAX_COMPLETION_TOKENS = "32768";
const THINKING_UNSET_VALUE = "__unset__";
const THINKING_CUSTOM_VALUE = "__custom__";

export function SetupPage({
  readiness,
  onReadinessChanged,
  onSaveSetupConfig = saveSetupConfig,
}: SetupPageProps) {
  const [step, setStep] = useState<SetupStep>("intro");
  const [personaLanguage, setPersonaLanguage] = useState("zh-CN");
  const [agentName, setAgentName] = useState("DaatLocus");
  const [personalizationPreview, setPersonalizationPreview] = useState("Hello");
  const [personalizationPreviewVisible, setPersonalizationPreviewVisible] =
    useState(true);
  const [modelAccess, setModelAccess] = useState<ModelAccessEditorValue>(() =>
    createDefaultModelAccessEditorValue(),
  );
  const [saveState, setSaveState] = useState<SaveState>("idle");
  const [saveError, setSaveError] = useState<string | null>(null);

  const isSaving = saveState === "saving";
  const { providers, models, mainModel, efficientModel } = modelAccess;
  const mainModelMissing = !models.some((model) => model.name === mainModel);
  const efficientModelMissing = !models.some(
    (model) => model.name === efficientModel,
  );
  const canCompleteSetup =
    providers.length > 0 &&
    models.length > 0 &&
    !mainModelMissing &&
    !efficientModelMissing;
  const nextPersonalizationPreview =
    PERSONA_LANGUAGES.find((language) => language.value === personaLanguage)
      ?.greeting ?? "Hello";

  useEffect(() => {
    if (personalizationPreview === nextPersonalizationPreview) {
      return;
    }

    setPersonalizationPreviewVisible(false);
    const timeoutId = window.setTimeout(() => {
      setPersonalizationPreview(nextPersonalizationPreview);
      setPersonalizationPreviewVisible(true);
    }, 140);

    return () => window.clearTimeout(timeoutId);
  }, [nextPersonalizationPreview, personalizationPreview]);

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (providers.length === 0) {
      setSaveState("error");
      setSaveError("Add at least one provider.");
      return;
    }
    if (models.length === 0) {
      setSaveState("error");
      setSaveError("Add at least one model.");
      return;
    }
    if (mainModelMissing || efficientModelMissing) {
      setSaveState("error");
      setSaveError("Select valid main and efficient models.");
      return;
    }

    const request: SetupConfigRequest = {
      ...agentPersonalizationEditorValueToSetupRequest({
        personaName: agentName,
        personaLanguage,
      }),
      ...modelAccessEditorValueToSetupRequest(modelAccess),
      daemon_port: readiness.port,
    };

    setSaveState("saving");
    setSaveError(null);
    try {
      const nextReadiness = await onSaveSetupConfig(request);
      onReadinessChanged(nextReadiness);
      setSaveState(nextReadiness.kind === "complete" ? "idle" : "error");
      setSaveError(
        nextReadiness.kind === "complete" ? null : nextReadiness.message,
      );
    } catch (error) {
      setSaveState("error");
      setSaveError(error instanceof Error ? error.message : String(error));
    }
  }


  if (step === "intro") {
    return (
      <section
        id="setup"
        aria-label="Configuration setup"
        className="flex min-h-screen w-full bg-background px-6 py-10"
      >
        <div className="flex w-full flex-col justify-between px-[8vw] py-[8vh]">
          <div className="flex flex-col gap-24">
            <h1 className="text-7xl font-medium tracking-normal">Hello</h1>
            <div className="flex max-w-2xl flex-col gap-3">
              <p className="text-3xl leading-relaxed text-foreground">
                It looks like Daat Locus is not configured yet
              </p>
              <p className="text-3xl leading-relaxed text-foreground">
                This wizard will guide you through initial setup
              </p>
            </div>
          </div>
          <div>
            <Button
              type="button"
              size="icon"
              className="size-12 rounded-full"
              aria-label="Next"
              onClick={() => setStep("personalization")}
            >
              <ArrowRightIcon data-icon="inline-end" aria-hidden="true" />
            </Button>
          </div>
        </div>
      </section>
    );
  }

  if (step === "personalization") {
    return (
      <section
        id="setup"
        aria-label="Personalization setup"
        className="flex min-h-screen w-full bg-background px-6 py-10"
      >
        <div className="flex w-full flex-col justify-between px-[8vw] py-[8vh]">
          <div className="grid flex-1 grid-cols-1 gap-16 lg:grid-cols-12">
            <div className="flex flex-col gap-24 lg:col-span-6">
              <h1 className="text-7xl font-medium tracking-normal">Personalize</h1>
              <AgentPersonalizationEditor
                value={{ personaName: agentName, personaLanguage }}
                onChange={(nextValue) => {
                  setAgentName(nextValue.personaName);
                  setPersonaLanguage(nextValue.personaLanguage);
                }}
                showHeader={false}
                className="max-w-xl"
              />
            </div>

            <div className="flex items-start justify-center pt-6 lg:col-span-4 lg:col-start-8 lg:justify-center lg:pt-36">
              <div className="flex flex-col items-center gap-10">
                <AgentExpression
                  status="idle"
                  className="w-44 p-0 sm:w-52"
                />
                <p
                  className={cn(
                    "text-7xl font-medium leading-none tracking-normal text-foreground transition-opacity duration-200",
                    personalizationPreviewVisible ? "opacity-100" : "opacity-0",
                  )}
                >
                  {personalizationPreview}
                </p>
              </div>
            </div>
          </div>
          <div>
            <Button
              type="button"
              size="icon"
              className="size-14 rounded-full"
              aria-label="Next"
              onClick={() => setStep("configuration")}
            >
              <ArrowRightIcon data-icon="inline-end" aria-hidden="true" />
            </Button>
          </div>
        </div>
      </section>
    );
  }

  return (
    <section
      id="setup"
      aria-label="Provider and model setup"
      className="flex min-h-screen w-full bg-background px-6 py-10"
    >
      <form
        onSubmit={handleSubmit}
        className="mx-auto flex w-full max-w-5xl flex-col px-[6vw] py-[7vh]"
      >
        <div className="flex flex-col gap-14">
          <div className="flex flex-col gap-16">
            <h1 className="text-7xl font-medium tracking-normal">Model Access</h1>
            <div className="flex max-w-3xl flex-col gap-3">
              <p className="text-3xl leading-relaxed text-foreground">
                Configure providers and models
              </p>
            </div>
          </div>

          <div className="flex flex-col gap-4">
            {readiness.recovery_note ? (
              <Alert>
                <TriangleAlertIcon aria-hidden="true" />
                <AlertTitle>Configuration file restored</AlertTitle>
                <AlertDescription>{readiness.recovery_note}</AlertDescription>
              </Alert>
            ) : null}

            {saveState === "error" && saveError ? (
              <Alert variant="destructive">
                <TriangleAlertIcon aria-hidden="true" />
                <AlertTitle>Unable to save configuration</AlertTitle>
                <AlertDescription>{saveError}</AlertDescription>
              </Alert>
            ) : null}
          </div>

          <ModelAccessEditor
            value={modelAccess}
            onChange={(nextValue) => {
              setModelAccess(nextValue);
              setSaveState("idle");
              setSaveError(null);
            }}
            submitSlot={
              canCompleteSetup ? (
                <div className="flex justify-start pt-4">
                  <Button
                    type="submit"
                    size="icon"
                    className="size-14 rounded-full"
                    disabled={isSaving}
                    aria-label={isSaving ? "Completing setup" : "Complete setup"}
                  >
                    {isSaving ? (
                      <Spinner data-icon="inline-start" />
                    ) : (
                      <CheckIcon data-icon="inline-start" aria-hidden="true" />
                    )}
                  </Button>
                </div>
              ) : null
            }
          />
        </div>
      </form>

    </section>
  );
}

export function AgentPersonalizationEditor({
  value,
  onChange,
  title,
  description = "Shape the agent's identity and voice across every interaction.",
  showHeader = true,
  className,
}: AgentPersonalizationEditorProps) {
  const agentDisplayName = displayAgentName(value.personaName);
  const sectionTitle = title ?? `Customize ${agentDisplayName}`;

  return (
    <section className={cn("flex flex-col gap-6", className)}>
      {showHeader ? (
        <div className="flex flex-col gap-2">
          <h2 className="text-3xl font-medium tracking-normal">
            {sectionTitle}
          </h2>
          {description ? (
            <p className="max-w-3xl text-base text-muted-foreground">
              {description}
            </p>
          ) : null}
        </div>
      ) : null}
      <FieldGroup className="max-w-xl">
        <Field>
          <FieldLabel htmlFor="agent-personalization-language">
            Language for {agentDisplayName}
          </FieldLabel>
          <Select
            value={normalizePersonaLanguage(value.personaLanguage)}
            onValueChange={(personaLanguage) =>
              onChange({ ...value, personaLanguage })
            }
          >
            <SelectTrigger
              id="agent-personalization-language"
              className="w-full"
            >
              <SelectValue placeholder="Select language" />
            </SelectTrigger>
            <SelectContent>
              <SelectGroup>
                {PERSONA_LANGUAGES.map((language) => (
                  <SelectItem key={language.value} value={language.value}>
                    {language.label}
                  </SelectItem>
                ))}
              </SelectGroup>
            </SelectContent>
          </Select>
        </Field>
        <Field>
          <FieldLabel htmlFor="agent-personalization-name">
            {agentDisplayName} name
          </FieldLabel>
          <Input
            id="agent-personalization-name"
            value={value.personaName}
            onChange={(event) =>
              onChange({ ...value, personaName: event.target.value })
            }
            placeholder="DaatLocus"
            spellCheck={false}
          />
        </Field>
      </FieldGroup>
    </section>
  );
}

export function ModelAccessEditor({
  value,
  onChange,
  submitSlot = null,
  providerDescription = "Connect the capability sources the agent can draw from.",
  modelDescription = "Shape the model catalog into dependable reasoning capacity.",
  selectionDescription =
    "Set the operating balance between deep focus and lightweight work.",
}: ModelAccessEditorProps) {
  const [providerDialog, setProviderDialog] =
    useState<ProviderDialogState | null>(null);
  const [modelDialog, setModelDialog] = useState<ModelDialogState | null>(null);
  const { providers, models, mainModel, efficientModel } = value;
  const modelNames = models.map((model) => model.name);
  const mainModelMissing = !models.some((model) => model.name === mainModel);
  const efficientModelMissing = !models.some(
    (model) => model.name === efficientModel,
  );

  function updateValue(nextValue: ModelAccessEditorValue) {
    onChange(nextValue);
  }

  function openAddProviderDialog() {
    setProviderDialog({
      mode: "add",
      provider: createDefaultProvider(providers),
    });
  }

  function openEditProviderDialog(provider: SetupProviderDraft) {
    setProviderDialog({ mode: "edit", provider });
  }

  function saveProvider(provider: SetupProviderDraft) {
    const previous = providerDialog?.provider ?? null;
    const nextProviders =
      providerDialog?.mode === "edit"
        ? providers.map((item) => (item.id === provider.id ? provider : item))
        : [...providers, provider];
    const nextModels =
      previous && previous.name !== provider.name
        ? models.map((model) =>
            model.providerName === previous.name
              ? { ...model, providerName: provider.name }
              : model,
          )
        : models;

    updateValue({
      ...value,
      providers: nextProviders,
      models: nextModels,
    });
    setProviderDialog(null);
  }

  function deleteProvider(provider: SetupProviderDraft) {
    const nextProviders = providers.filter((item) => item.id !== provider.id);
    const nextModels = models.filter(
      (model) => model.providerName !== provider.name,
    );
    updateValue({
      ...value,
      providers: nextProviders,
      models: nextModels,
      mainModel: safeSelectedModel(mainModel, nextModels),
      efficientModel: safeSelectedModel(efficientModel, nextModels),
    });
  }

  function openAddModelDialog() {
    setModelDialog({
      mode: "add",
      model: createDefaultModel(providers),
    });
  }

  function openEditModelDialog(model: SetupModelDraft) {
    setModelDialog({ mode: "edit", model });
  }

  function saveModel(model: SetupModelDraft) {
    const nextModels =
      modelDialog?.mode === "edit"
        ? models.map((item) => (item.id === model.id ? model : item))
        : [...models, model];
    updateValue({
      ...value,
      models: nextModels,
      mainModel: safeSelectedModel(mainModel, nextModels),
      efficientModel: safeSelectedModel(efficientModel, nextModels),
    });
    setModelDialog(null);
  }

  function deleteModel(model: SetupModelDraft) {
    const nextModels = models.filter((item) => item.id !== model.id);
    updateValue({
      ...value,
      models: nextModels,
      mainModel: safeSelectedModel(mainModel, nextModels),
      efficientModel: safeSelectedModel(efficientModel, nextModels),
    });
  }

  return (
    <>
      <RegistrySection
        title="Providers"
        description={providerDescription}
        actionLabel="Add provider"
        onAdd={openAddProviderDialog}
      >
        <ProviderList
          providers={providers}
          onEdit={openEditProviderDialog}
          onDelete={deleteProvider}
        />
      </RegistrySection>

      <RegistrySection
        title="Models"
        description={modelDescription}
        actionLabel="Add model"
        onAdd={openAddModelDialog}
        addDisabled={providers.length === 0}
      >
        <ModelList
          models={models}
          providers={providers}
          onEdit={openEditModelDialog}
          onDelete={deleteModel}
        />
      </RegistrySection>

      <section className="flex flex-col gap-6">
        <div className="flex flex-col gap-2">
          <h2 className="text-3xl font-medium tracking-normal">Select Models</h2>
          <p className="max-w-2xl text-base text-muted-foreground">
            {selectionDescription}
          </p>
        </div>
        <FieldGroup className="max-w-2xl">
          <div className="flex flex-col gap-4">
            <Field data-invalid={mainModelMissing}>
              <FieldLabel htmlFor="model-access-main-model">Main model</FieldLabel>
              <Select
                value={mainModel}
                onValueChange={(selected) =>
                  updateValue({ ...value, mainModel: selected })
                }
              >
                <SelectTrigger id="model-access-main-model" className="w-full">
                  <SelectValue placeholder="Select main model" />
                </SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    {modelNames.map((name) => (
                      <SelectItem key={name} value={name}>
                        {name}
                      </SelectItem>
                    ))}
                  </SelectGroup>
                </SelectContent>
              </Select>
              <FieldError>{mainModelMissing ? "Select a model." : null}</FieldError>
            </Field>
            <Field data-invalid={efficientModelMissing}>
              <FieldLabel htmlFor="model-access-efficient-model">
                Efficient model
              </FieldLabel>
              <Select
                value={efficientModel}
                onValueChange={(selected) =>
                  updateValue({ ...value, efficientModel: selected })
                }
              >
                <SelectTrigger
                  id="model-access-efficient-model"
                  className="w-full"
                >
                  <SelectValue placeholder="Select efficient model" />
                </SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    {modelNames.map((name) => (
                      <SelectItem key={name} value={name}>
                        {name}
                      </SelectItem>
                    ))}
                  </SelectGroup>
                </SelectContent>
              </Select>
              <FieldError>
                {efficientModelMissing ? "Select a model." : null}
              </FieldError>
            </Field>
          </div>
        </FieldGroup>
        {submitSlot}
      </section>

      <ProviderDialog
        dialog={providerDialog}
        providers={providers}
        onOpenChange={(open) => {
          if (!open) {
            setProviderDialog(null);
          }
        }}
        onSubmit={saveProvider}
      />
      <ModelDialog
        dialog={modelDialog}
        models={models}
        providers={providers}
        onOpenChange={(open) => {
          if (!open) {
            setModelDialog(null);
          }
        }}
        onSubmit={saveModel}
      />
    </>
  );
}

function RegistrySection({
  actionLabel,
  addDisabled = false,
  children,
  description,
  onAdd,
  title,
}: {
  actionLabel: string;
  addDisabled?: boolean;
  children: ReactNode;
  description?: string;
  onAdd: () => void;
  title: string;
}) {
  return (
    <section className="flex flex-col gap-6">
      <div className="flex items-start justify-between gap-6">
        <div className="flex flex-col gap-2">
          <h2 className="text-3xl font-medium tracking-normal">{title}</h2>
          {description ? (
            <p className="max-w-3xl text-base text-muted-foreground">
              {description}
            </p>
          ) : null}
        </div>
        <Button
          type="button"
          variant="outline"
          size="icon"
          className="size-12 rounded-full"
          disabled={addDisabled}
          aria-label={actionLabel}
          onClick={onAdd}
        >
          <PlusIcon data-icon="inline-start" aria-hidden="true" />
        </Button>
      </div>
      {children}
    </section>
  );
}

function ProviderList({
  onDelete,
  onEdit,
  providers,
}: {
  onDelete: (provider: SetupProviderDraft) => void;
  onEdit: (provider: SetupProviderDraft) => void;
  providers: SetupProviderDraft[];
}) {
  if (providers.length === 0) {
    return (
      <div className="border-y py-8 text-sm text-muted-foreground">
        No providers yet. Use the plus button to add one.
      </div>
    );
  }

  return (
    <div className="divide-y border-y">
      {providers.map((provider) => (
        <div
          key={provider.id}
          className="flex items-center justify-between gap-6 py-4"
        >
          <div className="min-w-0">
            <div className="flex flex-wrap items-center gap-2">
              <span className="truncate text-lg font-medium">
                {provider.name}
              </span>
              <Badge variant="secondary">
                {providerKindLabel(provider.kind)}
              </Badge>
              {provider.kind === "openai_codex_oauth" ? (
                <Badge variant="outline">
                  {codexAuthMethodLabel(provider.codexAuthMethod)}
                </Badge>
              ) : null}
            </div>
            <p className="mt-1 truncate text-sm text-muted-foreground">
              {providerSummary(provider)}
            </p>
          </div>
          <div className="flex shrink-0 items-center gap-2">
            <Button
              type="button"
              variant="ghost"
              size="icon"
              aria-label={`Edit ${provider.name}`}
              onClick={() => onEdit(provider)}
            >
              <PencilIcon data-icon="inline-start" aria-hidden="true" />
            </Button>
            <Button
              type="button"
              variant="ghost"
              size="icon"
              aria-label={`Delete ${provider.name}`}
              onClick={() => onDelete(provider)}
            >
              <Trash2Icon data-icon="inline-start" aria-hidden="true" />
            </Button>
          </div>
        </div>
      ))}
    </div>
  );
}

function ModelList({
  models,
  onDelete,
  onEdit,
  providers,
}: {
  models: SetupModelDraft[];
  onDelete: (model: SetupModelDraft) => void;
  onEdit: (model: SetupModelDraft) => void;
  providers: SetupProviderDraft[];
}) {
  if (models.length === 0) {
    return (
      <div className="border-y py-8 text-sm text-muted-foreground">
        No models yet. Add a provider, then use the plus button to add a model.
      </div>
    );
  }

  return (
    <div className="divide-y border-y">
      {models.map((model) => {
        const provider = providers.find(
          (item) => item.name === model.providerName,
        );
        return (
          <div
            key={model.id}
            className="flex items-center justify-between gap-6 py-4"
          >
            <div className="min-w-0">
              <div className="flex flex-wrap items-center gap-2">
                <span className="truncate text-lg font-medium">
                  {model.name}
                </span>
                <Badge variant="secondary">{model.modelId}</Badge>
                <Badge variant="outline">
                  {provider?.name ?? model.providerName}
                </Badge>
              </div>
              <p className="mt-1 truncate text-sm text-muted-foreground">
                context {model.contextWindowTokens || "auto"} · output{" "}
                {model.maxCompletionTokens || "auto"} · vision{" "}
                {supportsVisionLabel(model.supportsVision)}
              </p>
            </div>
            <div className="flex shrink-0 items-center gap-2">
              <Button
                type="button"
                variant="ghost"
                size="icon"
                aria-label={`Edit ${model.name}`}
                onClick={() => onEdit(model)}
              >
                <PencilIcon data-icon="inline-start" aria-hidden="true" />
              </Button>
              <Button
                type="button"
                variant="ghost"
                size="icon"
                aria-label={`Delete ${model.name}`}
                onClick={() => onDelete(model)}
              >
                <Trash2Icon data-icon="inline-start" aria-hidden="true" />
              </Button>
            </div>
          </div>
        );
      })}
    </div>
  );
}

function ProviderDialog({
  dialog,
  onOpenChange,
  onSubmit,
  providers,
}: {
  dialog: ProviderDialogState | null;
  onOpenChange: (open: boolean) => void;
  onSubmit: (provider: SetupProviderDraft) => void;
  providers: SetupProviderDraft[];
}) {
  const [draft, setDraft] = useState<SetupProviderDraft>(() =>
    createDefaultProvider([]),
  );
  const [error, setError] = useState<string | null>(null);
  const [authState, setAuthState] = useState<ProviderAuthState>("idle");
  const [authMessage, setAuthMessage] = useState<string | null>(null);
  const [authError, setAuthError] = useState<string | null>(null);
  const [deviceFlow, setDeviceFlow] =
    useState<SetupProviderAuthStartResponse | null>(null);
  const selectedKind = PROVIDER_KIND_OPTIONS.find(
    (kind) => kind.value === draft.kind,
  );
  const selectedCodexMethod = CODEX_AUTH_METHODS.find(
    (method) => method.value === draft.codexAuthMethod,
  );
  const selectedGithubMethod = GITHUB_AUTH_METHODS.find(
    (method) => method.value === draft.githubAuthMethod,
  );
  const needsApiKey =
    draft.kind === "openai" ||
    draft.kind === "openai_compatible" ||
    draft.kind === "ollama_cloud";
  const needsBaseUrl = draft.kind === "openai_compatible";
  const showBaseUrl =
    draft.kind === "openai" ||
    draft.kind === "openai_compatible" ||
    draft.kind === "openai_codex_oauth" ||
    draft.kind === "ollama";
  const showKeepAlive = draft.kind === "ollama" || draft.kind === "ollama_cloud";
  const showCodexAuthFile = draft.codexAuthMethod === "import_auth_file";
  const usesDeviceAuth =
    (draft.kind === "openai_codex_oauth" &&
      draft.codexAuthMethod === "device_login") ||
    (draft.kind === "github_copilot" &&
      draft.githubAuthMethod === "device_login");
  const usesProviderAuthAction =
    draft.kind === "openai_codex_oauth" ||
    (draft.kind === "github_copilot" &&
      draft.githubAuthMethod === "device_login");
  const providerAuthButtonLabel = providerAuthActionLabel(draft);
  const requiresCompletedProviderAuth =
    providerRequiresCompletedAuthBeforeSave(draft);
  const canSaveProvider =
    authState !== "running" &&
    (!requiresCompletedProviderAuth || authState === "success");
  const duplicateName = providers.some(
    (provider) =>
      provider.id !== draft.id &&
      provider.name.trim() === draft.name.trim() &&
      draft.name.trim() !== "",
  );

  useEffect(() => {
    if (!dialog?.provider) {
      return;
    }
    setDraft(dialog.provider);
    setError(null);
    setAuthState("idle");
    setAuthMessage(null);
    setAuthError(null);
    setDeviceFlow(null);
  }, [dialog]);

  function handleKindChange(value: SetupProviderKind) {
    resetProviderAuthStatus();
    setDraft((current) => {
      const previousDefault = defaultProviderName(current.kind);
      const nextDefault = uniqueProviderName(
        defaultProviderName(value),
        providers,
        current.id,
      );
      const shouldReplaceName =
        !current.name.trim() || current.name === previousDefault;
      return {
        ...current,
        kind: value,
        name: shouldReplaceName ? nextDefault : current.name,
        apiKey: defaultProviderApiKey(value, current.githubAuthMethod),
        baseUrl: defaultProviderBaseUrl(value),
      };
    });
  }

  function handleProviderNameChange(value: string) {
    if (value !== draft.name && draft.kind === "openai_codex_oauth") {
      resetProviderAuthStatus();
    }
    setDraft((current) => ({
      ...current,
      name: value,
    }));
  }

  function handleGithubAuthMethodChange(value: GithubAuthMethod) {
    resetProviderAuthStatus();
    setDraft((current) => ({
      ...current,
      githubAuthMethod: value,
      apiKey: defaultGithubToken(value, current.apiKey),
    }));
  }

  function handleCodexAuthMethodChange(value: CodexAuthMethod) {
    resetProviderAuthStatus();
    setDraft((current) => ({
      ...current,
      codexAuthMethod: value,
    }));
  }

  function handleCodexAuthFileChange(value: string) {
    if (value !== draft.codexAuthFile) {
      resetProviderAuthStatus();
    }
    setDraft((current) => ({
      ...current,
      codexAuthFile: value,
    }));
  }

  function resetProviderAuthStatus() {
    setAuthState("idle");
    setAuthMessage(null);
    setAuthError(null);
    setDeviceFlow(null);
  }

  function applyProviderAuthResponse(response: {
    api_key?: string | null;
    auth_file?: string | null;
    message: string;
  }) {
    setDraft((current) => ({
      ...current,
      apiKey: response.api_key ?? current.apiKey,
      codexAuthFile: response.auth_file ?? current.codexAuthFile,
    }));
    setAuthState("success");
    setAuthMessage(response.message);
    setAuthError(null);
  }

  async function handleRunProviderAuth() {
    if (!usesProviderAuthAction) {
      return;
    }
    if (!draft.name.trim()) {
      setAuthState("error");
      setAuthError("Enter a provider name first.");
      return;
    }
    if (
      draft.kind === "openai_codex_oauth" &&
      draft.codexAuthMethod === "import_auth_file" &&
      !draft.codexAuthFile.trim()
    ) {
      setAuthState("error");
      setAuthError("Enter an auth.json path first.");
      return;
    }

    setAuthState("running");
    setAuthMessage(null);
    setAuthError(null);
    try {
      if (usesDeviceAuth) {
        const flow = await startSetupProviderAuthDevice(providerToRequest(draft));
        setDeviceFlow(flow);
        setAuthState("device_pending");
        setAuthMessage(
          "Authorization page opened. Enter the device code in the browser to finish authorization.",
        );
        return;
      }

      const response = await runSetupProviderAuth(providerToRequest(draft));
      applyProviderAuthResponse(response);
    } catch (error) {
      setAuthState("error");
      setAuthError(error instanceof Error ? error.message : String(error));
    }
  }

  async function handleCompleteProviderAuth() {
    if (!deviceFlow) {
      return;
    }
    setAuthState("running");
    setAuthError(null);
    try {
      const response = await completeSetupProviderAuthDevice(
        providerToRequest(draft),
        deviceFlow.flow_id,
      );
      applyProviderAuthResponse(response);
      setDeviceFlow(null);
    } catch (error) {
      setAuthState("device_pending");
      setAuthError(error instanceof Error ? error.message : String(error));
    }
  }

  function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!draft.name.trim()) {
      setError("Provider name is required.");
      return;
    }
    if (duplicateName) {
      setError("Provider name already exists.");
      return;
    }
    if (needsApiKey && !draft.apiKey.trim()) {
      setError("This provider requires an API key.");
      return;
    }
    if (draft.kind === "github_copilot" && !draft.apiKey.trim()) {
      setError(
        "GitHub Copilot requires a token or environment variable reference.",
      );
      return;
    }
    if (needsBaseUrl && !draft.baseUrl.trim()) {
      setError("OpenAI compatible providers require a base URL.");
      return;
    }
    if (draft.kind === "openai_codex_oauth" && showCodexAuthFile) {
      if (!draft.codexAuthFile.trim()) {
        setError("This Codex authentication method requires an auth.json path.");
        return;
      }
    }
    if (requiresCompletedProviderAuth && authState !== "success") {
      setError(providerAuthSaveBlockMessage(draft));
      return;
    }
    onSubmit({
      ...draft,
      name: draft.name.trim(),
      apiKey: draft.apiKey.trim(),
      baseUrl: draft.baseUrl.trim(),
      keepAlive: draft.keepAlive.trim(),
      codexAuthFile: draft.codexAuthFile.trim(),
    });
  }

  return (
    <Dialog open={dialog !== null} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>
            {dialog?.mode === "edit" ? "Edit provider" : "Add provider"}
          </DialogTitle>
          <DialogDescription>
            Providers define credentials and API endpoints. Models are bound to
            providers in the next section.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={handleSubmit} className="flex flex-col gap-5">
          <FieldGroup>
            <Field data-invalid={Boolean(error)}>
              <FieldLabel htmlFor="setup-provider-name">Name</FieldLabel>
              <Input
                id="setup-provider-name"
                value={draft.name}
                onChange={(event) => handleProviderNameChange(event.target.value)}
                spellCheck={false}
                required
              />
              <FieldError>{error}</FieldError>
            </Field>

            <Field>
              <FieldLabel htmlFor="setup-provider-kind">Type</FieldLabel>
              <Select value={draft.kind} onValueChange={handleKindChange}>
                <SelectTrigger id="setup-provider-kind" className="w-full">
                  <SelectValue placeholder="Select provider type" />
                </SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    {PROVIDER_KIND_OPTIONS.map((kind) => (
                      <SelectItem key={kind.value} value={kind.value}>
                        {kind.label}
                      </SelectItem>
                    ))}
                  </SelectGroup>
                </SelectContent>
              </Select>
              <FieldDescription>{selectedKind?.description}</FieldDescription>
            </Field>

            {draft.kind === "openai_codex_oauth" ? (
              <>
                <Field>
                  <FieldLabel htmlFor="setup-codex-auth-method">
                    Codex authentication method
                  </FieldLabel>
                  <Select
                    value={draft.codexAuthMethod}
                    onValueChange={(value) =>
                      handleCodexAuthMethodChange(value as CodexAuthMethod)
                    }
                  >
                    <SelectTrigger
                      id="setup-codex-auth-method"
                      className="w-full"
                    >
                      <SelectValue placeholder="Select authentication method" />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectGroup>
                        {CODEX_AUTH_METHODS.map((method) => (
                          <SelectItem key={method.value} value={method.value}>
                            {method.label}
                          </SelectItem>
                        ))}
                      </SelectGroup>
                    </SelectContent>
                  </Select>
                  <FieldDescription>
                    {selectedCodexMethod?.description}
                  </FieldDescription>
                </Field>

                {showCodexAuthFile ? (
                  <Field>
                    <FieldLabel htmlFor="setup-codex-auth-file">
                      auth.json path
                    </FieldLabel>
                    <Input
                      id="setup-codex-auth-file"
                      value={draft.codexAuthFile}
                      onChange={(event) =>
                        handleCodexAuthFileChange(event.target.value)
                      }
                      placeholder="C:\Users\you\.codex\auth.json"
                      spellCheck={false}
                    />
                  </Field>
                ) : null}
              </>
            ) : null}

            {draft.kind === "github_copilot" ? (
              <>
                <Field>
                  <FieldLabel htmlFor="setup-github-auth-method">
                    GitHub authentication method
                  </FieldLabel>
                  <Select
                    value={draft.githubAuthMethod}
                    onValueChange={(value) =>
                      handleGithubAuthMethodChange(value as GithubAuthMethod)
                    }
                  >
                    <SelectTrigger
                      id="setup-github-auth-method"
                      className="w-full"
                    >
                      <SelectValue placeholder="Select authentication method" />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectGroup>
                        {GITHUB_AUTH_METHODS.map((method) => (
                          <SelectItem key={method.value} value={method.value}>
                            {method.label}
                          </SelectItem>
                        ))}
                      </SelectGroup>
                    </SelectContent>
                  </Select>
                  <FieldDescription>
                    {selectedGithubMethod?.description}
                  </FieldDescription>
                </Field>
                <Field>
                  <FieldLabel htmlFor="setup-github-token">
                    GitHub token
                  </FieldLabel>
                  <Input
                    id="setup-github-token"
                    value={draft.apiKey}
                    onChange={(event) =>
                      setDraft((current) => ({
                        ...current,
                        apiKey: event.target.value,
                      }))
                    }
                    type={
                      draft.githubAuthMethod === "manual_token"
                        ? "password"
                        : "text"
                    }
                    autoComplete="off"
                    spellCheck={false}
                  />
                </Field>
              </>
            ) : null}

            {usesProviderAuthAction ? (
              <Field data-invalid={authState === "error"}>
                <FieldLabel>Authentication</FieldLabel>
                <div className="flex flex-wrap items-center gap-2">
                  <Button
                    type="button"
                    variant="outline"
                    disabled={authState === "running"}
                    onClick={handleRunProviderAuth}
                  >
                    {authState === "running" ? (
                      <Spinner data-icon="inline-start" />
                    ) : (
                      <ArrowRightIcon data-icon="inline-start" aria-hidden="true" />
                    )}
                    {deviceFlow ? "Restart" : providerAuthButtonLabel}
                  </Button>
                  {deviceFlow ? (
                    <Button
                      type="button"
                      disabled={authState === "running"}
                      onClick={handleCompleteProviderAuth}
                    >
                      {authState === "running" ? (
                        <Spinner data-icon="inline-start" />
                      ) : (
                        <CheckIcon data-icon="inline-start" aria-hidden="true" />
                      )}
                      Complete authorization
                    </Button>
                  ) : null}
                </div>
                {deviceFlow ? (
                  <div className="grid gap-1 text-sm">
                    <a
                      href={deviceFlow.verification_url}
                      target="_blank"
                      rel="noreferrer"
                      className="text-foreground underline underline-offset-4"
                    >
                      {deviceFlow.verification_url}
                    </a>
                    <div className="font-mono text-2xl tracking-wide">
                      {deviceFlow.user_code}
                    </div>
                  </div>
                ) : null}
                {authError ? (
                  <FieldError>{authError}</FieldError>
                ) : (
                  <FieldDescription>
                    {authMessage ?? providerAuthDescription(draft)}
                  </FieldDescription>
                )}
              </Field>
            ) : null}

            {needsApiKey ? (
              <Field>
                <FieldLabel htmlFor="setup-provider-api-key">
                  API Key
                </FieldLabel>
                <Input
                  id="setup-provider-api-key"
                  value={draft.apiKey}
                  onChange={(event) =>
                    setDraft((current) => ({
                      ...current,
                      apiKey: event.target.value,
                    }))
                  }
                  type="password"
                  autoComplete="off"
                  spellCheck={false}
                />
              </Field>
            ) : null}

            {showBaseUrl ? (
              <Field data-invalid={needsBaseUrl && !draft.baseUrl.trim()}>
                <FieldLabel htmlFor="setup-provider-base-url">
                  {draft.kind === "ollama" ? "Host" : "Base URL"}
                </FieldLabel>
                <Input
                  id="setup-provider-base-url"
                  value={draft.baseUrl}
                  onChange={(event) =>
                    setDraft((current) => ({
                      ...current,
                      baseUrl: event.target.value,
                    }))
                  }
                  placeholder={providerBaseUrlPlaceholder(draft.kind)}
                  aria-invalid={needsBaseUrl && !draft.baseUrl.trim()}
                  spellCheck={false}
                />
                <FieldDescription>
                  Leave empty to use the provider default when optional.
                </FieldDescription>
                <FieldError>
                  {needsBaseUrl ? "OpenAI compatible requires a base URL." : null}
                </FieldError>
              </Field>
            ) : null}

            {showKeepAlive ? (
              <Field>
                <FieldLabel htmlFor="setup-provider-keep-alive">
                  keep_alive
                </FieldLabel>
                <Input
                  id="setup-provider-keep-alive"
                  value={draft.keepAlive}
                  onChange={(event) =>
                    setDraft((current) => ({
                      ...current,
                      keepAlive: event.target.value,
                    }))
                  }
                  placeholder="5m"
                  spellCheck={false}
                />
              </Field>
            ) : null}
          </FieldGroup>

          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={!canSaveProvider}>
              Save provider
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function ModelDialog({
  dialog,
  models,
  onOpenChange,
  onSubmit,
  providers,
}: {
  dialog: ModelDialogState | null;
  models: SetupModelDraft[];
  onOpenChange: (open: boolean) => void;
  onSubmit: (model: SetupModelDraft) => void;
  providers: SetupProviderDraft[];
}) {
  const [draft, setDraft] = useState<SetupModelDraft>(() =>
    createDefaultModel([]),
  );
  const [error, setError] = useState<string | null>(null);
  const [discoveredModels, setDiscoveredModels] = useState<
    SetupDiscoveredModel[]
  >([]);
  const [discoveryState, setDiscoveryState] = useState<
    "idle" | "loading" | "loaded" | "error"
  >("idle");
  const [discoveryError, setDiscoveryError] = useState<string | null>(null);
  const [discoveryRefreshKey, setDiscoveryRefreshKey] = useState(0);
  const [thinkingSelection, setThinkingSelection] = useState<ThinkingSelection>(
    THINKING_UNSET_VALUE,
  );
  const selectedProvider = providers.find(
    (provider) => provider.name === draft.providerName,
  );
  const selectedProviderRequest = useMemo(
    () => (selectedProvider ? providerToRequest(selectedProvider) : null),
    [selectedProvider],
  );
  const selectedSuggestion = discoveredModels.some(
    (model) => model.id === draft.modelId,
  )
    ? draft.modelId
    : "__manual__";
  const thinkingOptions = useMemo(
    () => modelThinkingOptions(draft.modelId, discoveredModels),
    [discoveredModels, draft.modelId],
  );
  const duplicateName = models.some(
    (model) =>
      model.id !== draft.id &&
      model.name.trim() === draft.name.trim() &&
      draft.name.trim() !== "",
  );

  useEffect(() => {
    if (!dialog?.model) {
      return;
    }
    setDraft(dialog.model);
    setThinkingSelection(
      dialog.model.thinkingBudget.trim()
        ? THINKING_CUSTOM_VALUE
        : THINKING_UNSET_VALUE,
    );
    setError(null);
  }, [dialog]);

  useEffect(() => {
    const value = draft.thinkingBudget.trim();
    if (!value) {
      if (thinkingSelection !== THINKING_CUSTOM_VALUE) {
        setThinkingSelection(THINKING_UNSET_VALUE);
      }
      return;
    }
    setThinkingSelection(
      thinkingOptions.includes(value) ? value : THINKING_CUSTOM_VALUE,
    );
  }, [draft.thinkingBudget, thinkingOptions, thinkingSelection]);

  useEffect(() => {
    if (!dialog || !selectedProviderRequest) {
      setDiscoveredModels([]);
      setDiscoveryState("idle");
      setDiscoveryError(null);
      return;
    }

    const controller = new AbortController();
    setDiscoveryState("loading");
    setDiscoveryError(null);
    void discoverSetupModels(selectedProviderRequest, {
      signal: controller.signal,
    })
      .then((discovered) => {
        setDiscoveredModels(discovered);
        setDiscoveryState("loaded");
        if (dialog.mode === "add" && discovered.length > 0) {
          setDraft((current) => {
            if (current.modelId.trim() !== "") {
              return current;
            }
            if (current.providerName !== selectedProviderRequest.name) {
              return current;
            }
            return applyDiscoveredModelToDraft(current, discovered[0], models);
          });
        }
      })
      .catch((error: unknown) => {
        if (controller.signal.aborted) {
          return;
        }
        setDiscoveredModels([]);
        setDiscoveryState("error");
        setDiscoveryError(error instanceof Error ? error.message : String(error));
      });

    return () => controller.abort();
  }, [
    dialog?.mode,
    dialog?.model?.id,
    selectedProviderRequest,
    discoveryRefreshKey,
  ]);

  function applySuggestion(modelId: string) {
    const model = discoveredModels.find((item) => item.id === modelId);
    if (!model) {
      return;
    }
    setDraft((current) =>
      applyDiscoveredModelToDraft(current, model, models),
    );
  }

  function handleProviderChange(providerName: string) {
    setThinkingSelection(THINKING_UNSET_VALUE);
    setDraft((current) => ({
      ...current,
      providerName,
      ...(dialog?.mode === "add"
        ? {
            modelId: "",
            name: "",
            contextWindowTokens: DEFAULT_CONTEXT_WINDOW_TOKENS,
            maxCompletionTokens: DEFAULT_MAX_COMPLETION_TOKENS,
            supportsVision: "auto" as const,
            thinkingBudget: "",
          }
        : {}),
    }));
  }

  function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!draft.providerName.trim()) {
      setError("Select a provider.");
      return;
    }
    if (!draft.name.trim()) {
      setError("Model name is required.");
      return;
    }
    if (duplicateName) {
      setError("Model name already exists.");
      return;
    }
    if (!draft.modelId.trim()) {
      setError("Model ID is required.");
      return;
    }
    if (!parseOptionalPositiveInt(draft.contextWindowTokens)) {
      setError("Context window tokens must be a positive integer.");
      return;
    }
    if (!parseOptionalPositiveInt(draft.maxCompletionTokens)) {
      setError("Max completion tokens must be a positive integer.");
      return;
    }
    onSubmit({
      ...draft,
      name: draft.name.trim(),
      modelId: draft.modelId.trim(),
      contextWindowTokens: draft.contextWindowTokens.trim(),
      maxCompletionTokens: draft.maxCompletionTokens.trim(),
      thinkingBudget: draft.thinkingBudget.trim(),
    });
  }

  return (
    <Dialog open={dialog !== null} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>
            {dialog?.mode === "edit" ? "Edit model" : "Add model"}
          </DialogTitle>
          <DialogDescription>
            Model definitions are bound to providers and can be selected as the
            main or efficient model.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={handleSubmit} className="flex flex-col gap-5">
          <FieldGroup>
            <Field data-invalid={Boolean(error)}>
              <FieldLabel htmlFor="setup-model-provider">Provider</FieldLabel>
              <Select
                value={draft.providerName}
                onValueChange={handleProviderChange}
              >
                <SelectTrigger id="setup-model-provider" className="w-full">
                  <SelectValue placeholder="Select provider" />
                </SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    {providers.map((provider) => (
                      <SelectItem key={provider.id} value={provider.name}>
                        {provider.name}
                      </SelectItem>
                    ))}
                  </SelectGroup>
                </SelectContent>
              </Select>
              <FieldError>{error}</FieldError>
            </Field>

            <Field>
              <div className="flex items-center justify-between gap-3">
                <FieldLabel htmlFor="setup-model-discovered">
                  Discovered models
                </FieldLabel>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  disabled={!selectedProvider || discoveryState === "loading"}
                  onClick={() => setDiscoveryRefreshKey((current) => current + 1)}
                >
                  {discoveryState === "loading" ? (
                    <Spinner data-icon="inline-start" />
                  ) : (
                    <RotateCcwIcon data-icon="inline-start" aria-hidden="true" />
                  )}
                  Rediscover
                </Button>
              </div>
              <Select
                value={selectedSuggestion}
                onValueChange={(value) => {
                  if (value !== "__manual__") {
                    applySuggestion(value);
                  }
                }}
                disabled={!selectedProvider}
              >
                <SelectTrigger id="setup-model-discovered" className="w-full">
                  <SelectValue placeholder="Select a model or enter manually" />
                </SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    {discoveredModels.map((model) => (
                      <SelectItem
                        key={model.id}
                        value={model.id}
                      >
                        {model.id}
                      </SelectItem>
                    ))}
                    <SelectItem value="__manual__">Manual input</SelectItem>
                  </SelectGroup>
                </SelectContent>
              </Select>
              {discoveryError ? (
                <FieldError>{discoveryError}</FieldError>
              ) : (
                <FieldDescription>
                  {modelDiscoveryDescription(
                    selectedProvider,
                    discoveryState,
                    discoveredModels.length,
                  )}
                </FieldDescription>
              )}
            </Field>

            <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
              <Field>
                <FieldLabel htmlFor="setup-model-name">Model name</FieldLabel>
                <Input
                  id="setup-model-name"
                  value={draft.name}
                  onChange={(event) =>
                    setDraft((current) => ({
                      ...current,
                      name: event.target.value,
                    }))
                  }
                  spellCheck={false}
                  required
                />
              </Field>
              <Field>
                <FieldLabel htmlFor="setup-model-id">Model ID</FieldLabel>
                <Input
                  id="setup-model-id"
                  value={draft.modelId}
                  onChange={(event) =>
                    setDraft((current) => ({
                      ...current,
                      modelId: event.target.value,
                    }))
                  }
                  spellCheck={false}
                  required
                />
              </Field>
              <Field>
                <FieldLabel htmlFor="setup-model-context">
                  Context window tokens
                </FieldLabel>
                <Input
                  id="setup-model-context"
                  value={draft.contextWindowTokens}
                  onChange={(event) =>
                    setDraft((current) => ({
                      ...current,
                      contextWindowTokens: event.target.value,
                    }))
                  }
                  inputMode="numeric"
                />
              </Field>
              <Field>
                <FieldLabel htmlFor="setup-model-output">
                  Max completion tokens
                </FieldLabel>
                <Input
                  id="setup-model-output"
                  value={draft.maxCompletionTokens}
                  onChange={(event) =>
                    setDraft((current) => ({
                      ...current,
                      maxCompletionTokens: event.target.value,
                    }))
                  }
                  inputMode="numeric"
                />
              </Field>
              <Field>
                <FieldLabel htmlFor="setup-model-vision">Vision</FieldLabel>
                <Select
                  value={draft.supportsVision}
                  onValueChange={(value) =>
                    setDraft((current) => ({
                      ...current,
                      supportsVision: value as SupportsVisionValue,
                    }))
                  }
                >
                  <SelectTrigger id="setup-model-vision" className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectGroup>
                      <SelectItem value="auto">Auto</SelectItem>
                      <SelectItem value="true">Supported</SelectItem>
                      <SelectItem value="false">Unsupported</SelectItem>
                    </SelectGroup>
                  </SelectContent>
                </Select>
              </Field>
              <Field>
                <FieldLabel htmlFor="setup-model-thinking">
                  Reasoning / thinking
                </FieldLabel>
                <Select
                  value={thinkingSelection}
                  onValueChange={(value) => {
                    if (value === THINKING_UNSET_VALUE) {
                      setThinkingSelection(value);
                      setDraft((current) => ({
                        ...current,
                        thinkingBudget: "",
                      }));
                      return;
                    }
                    if (value === THINKING_CUSTOM_VALUE) {
                      setThinkingSelection(value);
                      setDraft((current) => ({
                        ...current,
                        thinkingBudget: thinkingOptions.includes(
                          current.thinkingBudget.trim(),
                        )
                          ? ""
                          : current.thinkingBudget,
                      }));
                      return;
                    }
                    setThinkingSelection(value);
                    setDraft((current) => ({
                      ...current,
                      thinkingBudget: value,
                    }));
                  }}
                >
                  <SelectTrigger id="setup-model-thinking" className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectGroup>
                      <SelectItem value={THINKING_UNSET_VALUE}>
                        Not configured
                      </SelectItem>
                      {thinkingOptions.map((option) => (
                        <SelectItem key={option} value={option}>
                          {option}
                        </SelectItem>
                      ))}
                      <SelectItem value={THINKING_CUSTOM_VALUE}>
                        Custom
                      </SelectItem>
                    </SelectGroup>
                  </SelectContent>
                </Select>
                {thinkingSelection === THINKING_CUSTOM_VALUE ? (
                  <Input
                    value={draft.thinkingBudget}
                    onChange={(event) =>
                      setDraft((current) => ({
                        ...current,
                        thinkingBudget: event.target.value,
                      }))
                    }
                    placeholder="Enter a custom reasoning / thinking value"
                    spellCheck={false}
                  />
                ) : null}
              </Field>
            </div>
          </FieldGroup>

          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
            >
              Cancel
            </Button>
            <Button type="submit">Save model</Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function displayAgentName(personaName: string | null | undefined) {
  return personaName?.trim() || "DaatLocus";
}

function normalizePersonaLanguage(personaLanguage: string | null | undefined) {
  return personaLanguage?.trim() || "zh-CN";
}

export function createDefaultAgentPersonalizationEditorValue(): AgentPersonalizationEditorValue {
  return {
    personaName: "DaatLocus",
    personaLanguage: "zh-CN",
  };
}

export function setupConfigRequestToAgentPersonalizationEditorValue(
  request: SetupConfigRequest,
): AgentPersonalizationEditorValue {
  return {
    personaName: displayAgentName(request.persona_name),
    personaLanguage: normalizePersonaLanguage(request.persona_language),
  };
}

export function agentPersonalizationEditorValueToSetupRequest(
  value: AgentPersonalizationEditorValue,
): Pick<SetupConfigRequest, "persona_name" | "persona_language"> {
  return {
    persona_name: displayAgentName(value.personaName),
    persona_language: normalizePersonaLanguage(value.personaLanguage),
  };
}

export function createDefaultModelAccessEditorValue(): ModelAccessEditorValue {
  return {
    providers: [],
    models: [],
    mainModel: "",
    efficientModel: "",
  };
}

export function modelAccessEditorValueToSetupRequest(
  value: ModelAccessEditorValue,
): Pick<
  SetupConfigRequest,
  "providers" | "models" | "main_model" | "efficient_model"
> {
  return {
    providers: value.providers.map(providerToRequest),
    models: value.models.map(modelToRequest),
    main_model: value.mainModel || null,
    efficient_model: value.efficientModel || value.mainModel || null,
  };
}

export function setupConfigRequestToModelAccessEditorValue(
  request: SetupConfigRequest,
): ModelAccessEditorValue {
  return {
    providers: (request.providers ?? []).map(providerRequestToDraft),
    models: (request.models ?? []).map(modelRequestToDraft),
    mainModel: request.main_model ?? "",
    efficientModel: request.efficient_model ?? request.main_model ?? "",
  };
}

function providerRequestToDraft(provider: SetupProviderRequest): SetupProviderDraft {
  const githubAuthMethod = normalizeGithubAuthMethod(provider);
  return {
    id: createLocalId("provider"),
    name: provider.name,
    kind: provider.kind,
    apiKey:
      provider.api_key ?? defaultProviderApiKey(provider.kind, githubAuthMethod),
    baseUrl: provider.base_url ?? "",
    keepAlive: provider.keep_alive ?? "",
    codexAuthMethod: normalizeCodexAuthMethod(provider.codex_auth_method),
    codexAuthFile: provider.codex_auth_file ?? "",
    githubAuthMethod,
  };
}

function modelRequestToDraft(model: SetupModelRequest): SetupModelDraft {
  return {
    id: createLocalId("model"),
    name: model.name,
    providerName: model.provider_name,
    modelId: model.model_id,
    contextWindowTokens: String(
      model.context_window_tokens ?? DEFAULT_CONTEXT_WINDOW_TOKENS,
    ),
    maxCompletionTokens: String(
      model.max_completion_tokens ?? DEFAULT_MAX_COMPLETION_TOKENS,
    ),
    supportsVision:
      model.supports_vision == null
        ? "auto"
        : model.supports_vision
          ? "true"
          : "false",
    thinkingBudget: model.thinking_budget ?? "",
    source: model,
  };
}

function normalizeCodexAuthMethod(
  value: string | null | undefined,
): CodexAuthMethod {
  switch (value) {
    case "browser_login":
    case "device_login":
    case "import_local_codex":
    case "import_auth_file":
    case "existing_auth_file":
      return value;
    default:
      return "existing_auth_file";
  }
}

function normalizeGithubAuthMethod(
  provider: SetupProviderRequest,
): GithubAuthMethod {
  switch (provider.github_auth_method) {
    case "device_login":
    case "manual_token":
    case "env_token":
      return provider.github_auth_method;
    default:
      return provider.api_key?.trim().startsWith("$")
        ? "env_token"
        : "manual_token";
  }
}

function createDefaultProvider(
  providers: SetupProviderDraft[],
): SetupProviderDraft {
  const kind: SetupProviderKind = "openai_codex_oauth";
  return {
    id: createLocalId("provider"),
    name: uniqueProviderName(defaultProviderName(kind), providers),
    kind,
    apiKey: "",
    baseUrl: "",
    keepAlive: "",
    codexAuthMethod: "import_local_codex",
    codexAuthFile: "",
    githubAuthMethod: "env_token",
  };
}

function createDefaultModel(providers: SetupProviderDraft[]): SetupModelDraft {
  const provider = providers[0];
  return {
    id: createLocalId("model"),
    name: "",
    providerName: provider?.name ?? "",
    modelId: "",
    contextWindowTokens: DEFAULT_CONTEXT_WINDOW_TOKENS,
    maxCompletionTokens: DEFAULT_MAX_COMPLETION_TOKENS,
    supportsVision: "auto",
    thinkingBudget: "",
  };
}

function applyDiscoveredModelToDraft(
  current: SetupModelDraft,
  model: SetupDiscoveredModel,
  models: SetupModelDraft[],
): SetupModelDraft {
  return {
    ...current,
    modelId: model.id,
    name:
      !current.name.trim() || current.name === defaultModelName(current.modelId)
        ? uniqueModelName(defaultModelName(model.id), models, current.id)
        : current.name,
    contextWindowTokens: String(
      model.context_window_tokens ?? DEFAULT_CONTEXT_WINDOW_TOKENS,
    ),
    maxCompletionTokens: String(
      model.max_completion_tokens ?? DEFAULT_MAX_COMPLETION_TOKENS,
    ),
    supportsVision:
      model.supports_vision == null
        ? "auto"
        : model.supports_vision
          ? "true"
          : "false",
    thinkingBudget:
      defaultThinkingBudget(model.thinking_budgets ?? []) ??
      current.thinkingBudget,
  };
}

function providerToRequest(provider: SetupProviderDraft): SetupProviderRequest {
  return {
    kind: provider.kind,
    name: provider.name,
    api_key:
      provider.kind === "openai_codex_oauth"
        ? null
        : provider.apiKey.trim() || null,
    base_url: provider.baseUrl.trim() || null,
    keep_alive: provider.keepAlive.trim() || null,
    codex_auth_method:
      provider.kind === "openai_codex_oauth" ? provider.codexAuthMethod : null,
    codex_auth_file:
      provider.kind === "openai_codex_oauth"
        ? provider.codexAuthFile.trim() || null
        : null,
    github_auth_method:
      provider.kind === "github_copilot" ? provider.githubAuthMethod : null,
  };
}

function modelToRequest(model: SetupModelDraft): SetupModelRequest {
  return {
    ...model.source,
    name: model.name,
    provider_name: model.providerName,
    model_id: model.modelId,
    context_window_tokens: parseOptionalPositiveInt(model.contextWindowTokens),
    max_completion_tokens: parseOptionalPositiveInt(model.maxCompletionTokens),
    supports_vision:
      model.supportsVision === "auto" ? null : model.supportsVision === "true",
    thinking_budget: model.thinkingBudget.trim() || null,
  };
}

function providerKindLabel(kind: SetupProviderKind) {
  return (
    PROVIDER_KIND_OPTIONS.find((option) => option.value === kind)?.label ?? kind
  );
}

function modelDiscoveryDescription(
  provider: SetupProviderDraft | undefined,
  state: "idle" | "loading" | "loaded" | "error",
  count: number,
) {
  if (!provider) {
    return "Select a provider first.";
  }
  if (state === "loading") {
    return "Discovering models from this provider.";
  }
  if (state === "loaded" && count > 0) {
    return `Discovered ${count} models. You can also enter a model ID manually.`;
  }
  if (state === "loaded") {
    return "No models discovered. You can enter a model ID manually.";
  }
  return "Models are discovered automatically after a provider is selected.";
}

function defaultThinkingBudget(values: string[]) {
  if (values.includes("medium")) {
    return "medium";
  }
  return values[0] ?? null;
}

function modelThinkingOptions(
  modelId: string,
  discoveredModels: SetupDiscoveredModel[],
) {
  const model = discoveredModels.find((item) => item.id === modelId);
  return uniqueStrings(
    (model?.thinking_budgets ?? [])
      .map((value) => value.trim())
      .filter(Boolean),
  );
}

function uniqueStrings(values: string[]) {
  return Array.from(new Set(values));
}

function providerSummary(provider: SetupProviderDraft) {
  if (provider.kind === "openai_codex_oauth") {
    return provider.baseUrl.trim()
      ? `Codex OAuth · ${provider.baseUrl}`
      : "Codex OAuth · default endpoint";
  }
  if (provider.kind === "github_copilot") {
    return `GitHub Copilot · ${githubAuthMethodLabel(provider.githubAuthMethod)}`;
  }
  if (provider.kind === "ollama") {
    return provider.baseUrl.trim() || "http://127.0.0.1:11434";
  }
  if (provider.kind === "ollama_cloud") {
    return "https://ollama.com";
  }
  return provider.baseUrl.trim() || "provider default";
}

function defaultProviderName(kind: SetupProviderKind) {
  switch (kind) {
    case "openai":
      return "openai";
    case "openai_codex_oauth":
      return "codex-oauth";
    case "github_copilot":
      return "copilot";
    case "openai_compatible":
      return "openai-compatible";
    case "ollama":
      return "ollama";
    case "ollama_cloud":
      return "ollama-cloud";
  }
}

function defaultProviderApiKey(
  kind: SetupProviderKind,
  githubAuthMethod: GithubAuthMethod,
) {
  if (kind === "openai") {
    return "$OPENAI_API_KEY";
  }
  if (kind === "github_copilot") {
    return defaultGithubToken(githubAuthMethod, "");
  }
  if (kind === "ollama_cloud") {
    return "$OLLAMA_API_KEY";
  }
  return "";
}

function defaultGithubToken(method: GithubAuthMethod, current: string) {
  if (method === "env_token") {
    return "$GITHUB_TOKEN";
  }
  return current === "$GITHUB_TOKEN" ? "" : current;
}

function defaultProviderBaseUrl(kind: SetupProviderKind) {
  if (kind === "openai_compatible") {
    return "https://api.openai.com/v1";
  }
  if (kind === "ollama") {
    return "http://127.0.0.1:11434";
  }
  return "";
}

function providerBaseUrlPlaceholder(kind: SetupProviderKind) {
  if (kind === "ollama") {
    return "http://127.0.0.1:11434";
  }
  if (kind === "openai_compatible") {
    return "https://api.example.com/v1";
  }
  return "Use provider default";
}

function codexAuthMethodLabel(method: CodexAuthMethod) {
  return CODEX_AUTH_METHODS.find((item) => item.value === method)?.label ?? method;
}

function githubAuthMethodLabel(method: GithubAuthMethod) {
  return GITHUB_AUTH_METHODS.find((item) => item.value === method)?.label ?? method;
}

function providerAuthActionLabel(provider: SetupProviderDraft) {
  if (provider.kind === "github_copilot") {
    return "Start device code login";
  }
  switch (provider.codexAuthMethod) {
    case "browser_login":
      return "Open browser login";
    case "device_login":
      return "Start device code login";
    case "import_local_codex":
      return "Import local Codex";
    case "import_auth_file":
      return "Import auth.json";
    case "existing_auth_file":
      return "Check OAuth file";
  }
}

function providerAuthDescription(provider: SetupProviderDraft) {
  if (provider.kind === "github_copilot") {
    return "Authorization writes the GitHub token into the current provider draft.";
  }
  switch (provider.codexAuthMethod) {
    case "browser_login":
      return "After login, Daat Locus writes the fixed Codex OAuth file for this provider.";
    case "device_login":
      return "Start the flow, enter the device code, then return here to complete authorization.";
    case "import_local_codex":
      return "Import from the local Codex CLI auth.json into this provider.";
    case "import_auth_file":
      return "Import from the specified auth.json into this provider.";
    case "existing_auth_file":
      return "Check whether this provider's fixed Codex OAuth file exists.";
  }
}

function providerRequiresCompletedAuthBeforeSave(provider: SetupProviderDraft) {
  return (
    provider.kind === "openai_codex_oauth" ||
    (provider.kind === "github_copilot" &&
      provider.githubAuthMethod === "device_login")
  );
}

function providerAuthSaveBlockMessage(provider: SetupProviderDraft) {
  if (provider.kind === "github_copilot") {
    return "Complete GitHub device code login first.";
  }
  switch (provider.codexAuthMethod) {
    case "browser_login":
      return "Complete browser login first.";
    case "device_login":
      return "Complete device code login first.";
    case "import_local_codex":
      return "Import local Codex and wait for it to finish first.";
    case "import_auth_file":
      return "Import auth.json and wait for it to finish first.";
    case "existing_auth_file":
      return "Check the existing OAuth file first.";
  }
}

function supportsVisionLabel(value: SupportsVisionValue) {
  if (value === "auto") {
    return "auto";
  }
  return value === "true" ? "yes" : "no";
}

function defaultModelName(modelId: string) {
  const name = modelId.split(/[/:]/).filter(Boolean).at(-1) ?? modelId;
  return sanitizeName(name || "model");
}

function uniqueProviderName(
  base: string,
  providers: SetupProviderDraft[],
  currentId?: string,
) {
  const names = new Set(
    providers
      .filter((provider) => provider.id !== currentId)
      .map((provider) => provider.name),
  );
  return uniqueName(base, names);
}

function uniqueModelName(
  base: string,
  models: SetupModelDraft[],
  currentId?: string,
) {
  const names = new Set(
    models
      .filter((model) => model.id !== currentId)
      .map((model) => model.name),
  );
  return uniqueName(base, names);
}

function uniqueName(base: string, names: Set<string>) {
  const sanitized = sanitizeName(base);
  if (!names.has(sanitized)) {
    return sanitized;
  }
  let index = 2;
  while (names.has(`${sanitized}-${index}`)) {
    index += 1;
  }
  return `${sanitized}-${index}`;
}

function sanitizeName(value: string) {
  const sanitized = value
    .trim()
    .replace(/[^A-Za-z0-9_.-]+/g, "-")
    .replace(/^-+|-+$/g, "");
  return sanitized || "item";
}

function safeSelectedModel(current: string, models: SetupModelDraft[]) {
  if (models.some((model) => model.name === current)) {
    return current;
  }
  return models[0]?.name ?? "";
}

function parseOptionalPositiveInt(value: string) {
  const trimmed = value.trim();
  if (!trimmed) {
    return null;
  }
  const parsed = Number.parseInt(trimmed, 10);
  if (!Number.isInteger(parsed) || parsed <= 0) {
    return null;
  }
  return parsed;
}

function createLocalId(prefix: string) {
  return `${prefix}-${Math.random().toString(36).slice(2, 10)}`;
}
