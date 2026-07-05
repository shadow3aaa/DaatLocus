import i18n from "i18next";
import { initReactI18next } from "react-i18next";

export const WEBUI_LOCALES = ["en-US", "zh-CN"] as const;
export type WebUiLocale = (typeof WEBUI_LOCALES)[number];

export const DEFAULT_WEBUI_LOCALE: WebUiLocale = "en-US";

export const webUiLocaleOptions: Array<{
  value: WebUiLocale;
  label: string;
}> = [
  { value: "en-US", label: "English" },
  { value: "zh-CN", label: "简体中文" },
];

const LANGUAGE_STORAGE_KEY = "daat-locus.webui.language";

export function normalizeWebUiLocale(
  locale: string | null | undefined,
): WebUiLocale {
  const normalizedLocale = locale?.trim().toLowerCase();
  return normalizedLocale === "zh-cn" || normalizedLocale?.startsWith("zh")
    ? "zh-CN"
    : DEFAULT_WEBUI_LOCALE;
}

function readStoredWebUiLanguage(): WebUiLocale | null {
  if (typeof window === "undefined") {
    return null;
  }

  try {
    const storedLanguage = window.localStorage.getItem(LANGUAGE_STORAGE_KEY);
    if (storedLanguage === "en-US" || storedLanguage === "zh-CN") {
      return storedLanguage;
    }
  } catch {
    // Ignore localStorage failures, e.g. private mode or disabled storage.
  }

  return null;
}

function storeWebUiLanguage(locale: WebUiLocale) {
  if (typeof window === "undefined") {
    return;
  }

  try {
    window.localStorage.setItem(LANGUAGE_STORAGE_KEY, locale);
  } catch {
    // Ignore localStorage failures, e.g. private mode or disabled storage.
  }
}

function applyDocumentLanguage(locale: WebUiLocale) {
  if (typeof document !== "undefined") {
    document.documentElement.lang = locale;
  }
}

function initialWebUiLanguage(): WebUiLocale {
  return readStoredWebUiLanguage() ?? DEFAULT_WEBUI_LOCALE;
}

const resources = {
  "en-US": {
    translation: {
      common: {
        appName: "Daat Locus",
        cancel: "Cancel",
        delete: "Delete",
        deleting: "Deleting",
        retry: "Retry",
        unknown: "unknown",
        thisSession: "this session",
        untitledSession: "Untitled session",
      },
      document: {
        signIn: "Sign in",
      },
      navigation: {
        agent: "Agent",
        status: "Status",
        settings: "Settings",
        logs: "Logs",
      },
      app: {
        sessionRequiredAria: "Session required",
        noSessionTitle: "No session selected",
        loadingSessionsTitle: "Loading sessions",
        sessionListLoadFailed: "Session list could not be loaded.",
        createOrSelectSession: "Create or select a session from the sidebar.",
        fetchingSessions: "Fetching available sessions.",
        setupLoadingAria: "Loading configuration readiness",
        setupLoadingTitle: "Checking configuration",
        setupLoadingDescription:
          "Loading Manager readiness before opening the agent workspace.",
        authLoadingAria: "Verifying daemon token",
        authLoadingTitle: "Checking authentication",
        authLoadingDescription:
          "Verifying the saved daemon token before opening the WebUI.",
        setupErrorAria: "Configuration readiness error",
        setupErrorTitle: "Unable to read configuration state",
        setupErrorDescription:
          "The WebUI could not determine whether the agent can run.",
      },

      setup: {
        intro: {
          pageAria: "Configuration setup",
          greeting: "Hello",
          languageLabel: "WebUI language",
          languageDescription:
            "Choose the interface language before continuing setup.",
          languagePlaceholder: "Select language",
          notConfigured: "It looks like Daat Locus is not configured yet",
          wizardGuide: "This wizard will guide you through initial setup",
          next: "Next",
        },
        personalization: {
          pageAria: "Personalization setup",
          title: "Personalize",
          next: "Next",
          customize: "Customize {{agent}}",
          defaultDescription:
            "Shape the agent's identity and voice across every interaction.",
          languageForAgent: "Language for {{agent}}",
          languagePlaceholder: "Select language",
          agentName: "{{agent}} name",
          personaContent: "Persona content",
          personaContentDescription:
            "Supports {{token}}; this content is written into the persona prompt.",
        },
        configuration: {
          pageAria: "Provider and model setup",
          title: "Model Access",
          description: "Configure providers and models",
          configRestored: "Configuration file restored",
          unableToSave: "Unable to save configuration",
          completingSetup: "Completing setup",
          completeSetup: "Complete setup",
        },
        modelAccess: {
          providerDescription:
            "Connect the capability sources the agent can draw from.",
          modelDescription:
            "Shape the model catalog into dependable reasoning capacity.",
          selectionDescription:
            "Set the operating balance between deep focus and lightweight work.",
          providers: "Providers",
          addProvider: "Add provider",
          models: "Models",
          addModel: "Add model",
          selectModels: "Select Models",
          mainModel: "Main model",
          selectMainModel: "Select main model",
          efficientModel: "Efficient model",
          selectEfficientModel: "Select efficient model",
          selectModelError: "Select a model.",
          noProviders: "No providers yet. Use the plus button to add one.",
          noModels:
            "No models yet. Add a provider, then use the plus button to add a model.",
          editProviderAria: "Edit {{name}}",
          deleteProviderAria: "Delete {{name}}",
          editModelAria: "Edit {{name}}",
          deleteModelAria: "Delete {{name}}",
          auto: "auto",
          visionAuto: "auto",
          visionYes: "yes",
          visionNo: "no",
          modelSummary:
            "context {{context}} · output {{output}} · vision {{vision}}",
          providerKinds: {
            openai: {
              label: "OpenAI",
              description:
                "Use an API key with OpenAI Responses-compatible access.",
            },
            openai_codex_oauth: {
              label: "OpenAI Codex",
              description: "Use a ChatGPT Codex OAuth account file.",
            },
            github_copilot: {
              label: "GitHub Copilot",
              description: "Use a GitHub Copilot account token.",
            },
            openai_compatible: {
              label: "OpenAI compatible",
              description: "Use an API key with a custom base URL.",
            },
            ollama: {
              label: "Ollama local",
              description: "Use a local Ollama endpoint.",
            },
            ollama_cloud: {
              label: "Ollama Cloud",
              description: "Use an Ollama Cloud API key.",
            },
          },
          codexAuthMethods: {
            browser_login: {
              label: "Browser login",
              description:
                "Open the OpenAI authorization page and write this provider's OAuth file.",
            },
            device_login: {
              label: "Device code login",
              description:
                "Show a device code and complete authorization in the browser.",
            },
            import_local_codex: {
              label: "Import local Codex",
              description: "Read auth.json from the local Codex CLI.",
            },
            import_auth_file: {
              label: "Import auth.json",
              description: "Import from a selected Codex auth.json path.",
            },
            existing_auth_file: {
              label: "Use existing Daat Locus OAuth file",
              description:
                "Keep or manually place the OAuth file for this provider.",
            },
          },
          githubAuthMethods: {
            device_login: {
              label: "Device code login",
              description:
                "Get a Copilot access token through the GitHub device flow.",
            },
            manual_token: {
              label: "Manual token",
              description: "Paste a GitHub token.",
            },
            env_token: {
              label: "Environment variable",
              description: "Save a $GITHUB_TOKEN reference.",
            },
          },
          providerDialog: {
            editTitle: "Edit provider",
            addTitle: "Add provider",
            description:
              "Providers define credentials and API endpoints. Models are bound to providers in the next section.",
            name: "Name",
            type: "Type",
            selectProviderType: "Select provider type",
            codexAuthMethodLabel: "Codex authentication method",
            githubAuthMethodLabel: "GitHub authentication method",
            selectAuthMethod: "Select authentication method",
            authFilePath: "auth.json path",
            githubToken: "GitHub token",
            authentication: "Authentication",
            restart: "Restart",
            completeAuthorization: "Complete authorization",
            apiKey: "API Key",
            host: "Host",
            baseUrl: "Base URL",
            baseUrlOptionalHint:
              "Leave empty to use the provider default when optional.",
            baseUrlRequiredError: "OpenAI compatible requires a base URL.",
            baseUrlPlaceholderDefault: "Use provider default",
            keepAlive: "keep_alive",
            cancel: "Cancel",
            saveProvider: "Save provider",
            deviceAuthOpened:
              "Authorization page opened. Enter the device code in the browser to finish authorization.",
            authAction: {
              github: "Start device code login",
              browser_login: "Open browser login",
              device_login: "Start device code login",
              import_local_codex: "Import local Codex",
              import_auth_file: "Import auth.json",
              existing_auth_file: "Check OAuth file",
            },
            authDescription: {
              github:
                "Authorization writes the GitHub token into the current provider draft.",
              browser_login:
                "After login, Daat Locus writes the fixed Codex OAuth file for this provider.",
              device_login:
                "Start the flow, enter the device code, then return here to complete authorization.",
              import_local_codex:
                "Import from the local Codex CLI auth.json into this provider.",
              import_auth_file:
                "Import from the specified auth.json into this provider.",
              existing_auth_file:
                "Check whether this provider's fixed Codex OAuth file exists.",
            },
            authSaveBlock: {
              github: "Complete GitHub device code login first.",
              browser_login: "Complete browser login first.",
              device_login: "Complete device code login first.",
              import_local_codex:
                "Import local Codex and wait for it to finish first.",
              import_auth_file:
                "Import auth.json and wait for it to finish first.",
              existing_auth_file: "Check the existing OAuth file first.",
            },
            summary: {
              codexDefaultEndpoint: "Codex OAuth · default endpoint",
              codexEndpoint: "Codex OAuth · {{url}}",
              githubCopilot: "GitHub Copilot · {{method}}",
              providerDefault: "provider default",
            },
            errors: {
              nameRequired: "Provider name is required.",
              nameExists: "Provider name already exists.",
              apiKeyRequired: "This provider requires an API key.",
              githubTokenRequired:
                "GitHub Copilot requires a token or environment variable reference.",
              baseUrlRequired:
                "OpenAI compatible providers require a base URL.",
              authFileRequired:
                "This Codex authentication method requires an auth.json path.",
              enterNameFirst: "Enter a provider name first.",
              enterAuthFileFirst: "Enter an auth.json path first.",
            },
          },
          modelDialog: {
            editTitle: "Edit model",
            addTitle: "Add model",
            description:
              "Model definitions are bound to providers and can be selected as the main or efficient model.",
            provider: "Provider",
            selectProvider: "Select provider",
            discoveredModels: "Discovered models",
            rediscover: "Rediscover",
            selectModelOrManual: "Select a model or enter manually",
            manualInput: "Manual input",
            modelName: "Model name",
            modelId: "Model ID",
            contextWindowTokens: "Context window tokens",
            maxCompletionTokens: "Max completion tokens",
            vision: "Vision",
            visionAuto: "Auto",
            visionSupported: "Supported",
            visionUnsupported: "Unsupported",
            apiStyle: "API style",
            apiStyleChatCompletions: "Chat completions (default)",
            apiStyleResponses: "Responses",
            apiStyleDescription:
              "Selects the endpoint protocol for this openai-compatible model: chat completions or the responses API.",
            reasoning: "Reasoning / thinking",
            notConfigured: "Not configured",
            custom: "Custom",
            customReasoningPlaceholder:
              "Enter a custom reasoning / thinking value",
            cancel: "Cancel",
            saveModel: "Save model",
            discovery: {
              selectProviderFirst: "Select a provider first.",
              loading: "Discovering models from this provider.",
              loadedSome:
                "Discovered {{count}} models. You can also enter a model ID manually.",
              loadedNone:
                "No models discovered. You can enter a model ID manually.",
              idle: "Models are discovered automatically after a provider is selected.",
            },
            errors: {
              providerRequired: "Select a provider.",
              nameRequired: "Model name is required.",
              nameExists: "Model name already exists.",
              modelIdRequired: "Model ID is required.",
              contextWindowInvalid:
                "Context window tokens must be a positive integer.",
              maxCompletionInvalid:
                "Max completion tokens must be a positive integer.",
            },
          },
        },
        validation: {
          providerRequired: "Add at least one provider.",
          modelRequired: "Add at least one model.",
          mainAndEfficientModelsRequired:
            "Select valid main and efficient models.",
        },
      },
      login: {
        daemonToken: "Daemon token",
        tokenPlaceholder: "Token",
        enterToken: "Enter the daemon token.",
        verifyingToken: "Verifying token…",
        verifiedToken: "Token verified. Future pages will reuse this token.",
        verifying: "Verifying",
        submit: "Login",
      },
      sidebar: {
        open: "Open sidebar",
        projects: "Projects",
        noProjects: "No projects",
        conversations: "Conversations",
        noChats: "No chats",
        newCodingSession: "New coding session",
        newProjectSession: "New project session",
        newSessionInProject: "New session in {{project}}",
        newConversation: "New conversation",
        showMore: "Show more",
        showLess: "Show less",
        deleteSessionAria: "Delete {{title}}",
        deleteSessionTitle: "Delete session",
        deleteDialogTitle: "Delete session?",
        deleteDialogDescription: "This permanently deletes {{title}} ({{id}}).",
        relativeTime: {
          now: "now",
          minute: "{{count}} min",
          hour: "{{count}} hr",
          day: "{{count}} d",
          month: "{{count}} mo",
          year: "{{count}} yr",
        },
      },
      theme: {
        switchToLight: "Switch to light mode",
        switchToDark: "Switch to dark mode",
        lightMode: "Light mode",
        darkMode: "Dark mode",
      },
      settings: {
        pageAria: "Settings",
        unableToLoad: "Unable to load settings",
        configRestored: "Configuration file restored",
        unableToSave: "Unable to save settings",
        interface: {
          title: "Interface",
          description:
            "Choose how WebUI labels, navigation, and controls are displayed.",
          languageLabel: "WebUI language",
          languageDescription:
            "This setting is saved to the shared Daat Locus locale configuration.",
          languagePlaceholder: "Select language",
        },
        telegram: {
          title: "Telegram",
          description:
            "Enable Telegram transport and provide the bot token used for incoming messages and event replies.",
          enableLabel: "Enable Telegram",
          enableDescription:
            "The transport starts only when this switch is on and the token is a real Bot API token.",
          enableAria: "Enable Telegram transport",
          botToken: "Bot token",
          botTokenDescription:
            "Paste the token from <botFather>BotFather</botFather>.",
        },
        modelAccess: {
          providerDescription:
            "Tune the secure access layer behind the agent's model capability.",
          modelDescription:
            "Shape available model capacity into a dependable runtime catalog.",
          selectionDescription:
            "Set the operating balance between depth, speed, and everyday work.",
        },
        validation: {
          providerRequired: "Add at least one provider.",
          modelRequired: "Add at least one model.",
          mainModelRequired: "Select a valid main model.",
          efficientModelRequired: "Select a valid efficient model.",
        },
      },
      status: {
        pageAria: "Status",
        unableToLoad: "Unable to load status",
        reorderCard: "Reorder {{label}} card",
        dragToReorder: "Drag to reorder {{label}}",
        cards: {
          contextComposition: "Context Composition",
          tokenUsage: "Token Usage",
        },
        session: "Session",
        noSession: "No session",
        context: "context",
        noContextSnapshot: "No context snapshot",
        noSessionsFound: "No sessions found",
        contextNoSnapshotDescription:
          "This session has not assembled a model request context yet.",
        contextNoSessionsDescription:
          "Context composition appears after a session publishes status data.",
        contextHeatmapLabel:
          "Context composition heatmap. The current adaptive layout is {{columns}} by {{rows}}.",
        contextCellLabel:
          "Each cell represents up to {{tokens}} estimated tokens.",
        contextDisplayAria:
          "{{gridLabel}} Showing {{occupied}} occupied units on a {{displayScale}} rectangular display for {{session}}.",
        tokenCount: "{{count}} tokens",
        total: "Total",
        cached: "Cached",
        uncached: "Uncached",
        noTokenUsage: "No token usage recorded",
        tokenUsageDescription:
          "Usage bars appear after sessions make model requests.",
      },
      logs: {
        pageAria: "Logs",
        loadingLogs: "Loading logs",
        sourceLoadFailed: "Unable to load log sources.",
        live: "live",
        missing: "missing",
        search: "Search logs",
        title: "Logs",
        loadingSources: "Loading log sources…",
        noSourceSelected: "No log source selected.",
        readFailed: "Unable to read this log.",
        loadingEntries: "Loading log entries…",
        noEntries: "No log entries.",
        noLevelEntries: "No {{level}} or higher log entries.",
        noMatchingEntries: "No matching log entries.",
        blank: "(blank)",
      },
    },
  },
  "zh-CN": {
    translation: {
      common: {
        appName: "Daat Locus",
        cancel: "取消",
        delete: "删除",
        deleting: "正在删除",
        retry: "重试",
        unknown: "未知",
        thisSession: "此会话",
        untitledSession: "未命名会话",
      },
      document: {
        signIn: "登录",
      },
      navigation: {
        agent: "代理",
        status: "状态",
        settings: "设置",
        logs: "日志",
      },
      app: {
        sessionRequiredAria: "需要会话",
        noSessionTitle: "未选择会话",
        loadingSessionsTitle: "正在加载会话",
        sessionListLoadFailed: "无法加载会话列表。",
        createOrSelectSession: "请从侧边栏创建或选择一个会话。",
        fetchingSessions: "正在获取可用会话。",
        setupLoadingAria: "正在加载配置就绪状态",
        setupLoadingTitle: "正在检查配置",
        setupLoadingDescription: "打开代理工作区前正在加载 Manager 就绪状态。",
        authLoadingAria: "正在验证 daemon token",
        authLoadingTitle: "正在检查认证",
        authLoadingDescription: "打开 WebUI 前正在验证已保存的 daemon token。",
        setupErrorAria: "配置就绪状态错误",
        setupErrorTitle: "无法读取配置状态",
        setupErrorDescription: "WebUI 无法确定代理是否可以运行。",
      },

      setup: {
        intro: {
          pageAria: "配置设置",
          greeting: "你好",
          languageLabel: "WebUI 语言",
          languageDescription: "继续设置前选择界面语言。",
          languagePlaceholder: "选择语言",
          notConfigured: "Daat Locus 似乎尚未配置",
          wizardGuide: "此向导将引导你完成初始设置",
          next: "下一步",
        },
        personalization: {
          pageAria: "个性化设置",
          title: "个性化",
          next: "下一步",
          customize: "自定义 {{agent}}",
          defaultDescription: "塑造代理在每次交互中的身份与表达风格。",
          languageForAgent: "{{agent}} 使用的语言",
          languagePlaceholder: "选择语言",
          agentName: "{{agent}} 名称",
          personaContent: "人格内容",
          personaContentDescription: "支持 {{token}}；此内容会写入人格提示词。",
        },
        configuration: {
          pageAria: "供应商和模型设置",
          title: "模型访问",
          description: "配置供应商和模型",
          configRestored: "配置文件已恢复",
          unableToSave: "无法保存配置",
          completingSetup: "正在完成设置",
          completeSetup: "完成设置",
        },
        modelAccess: {
          providerDescription: "连接代理可使用的能力来源。",
          modelDescription: "将模型目录整理成可靠的推理能力。",
          selectionDescription: "设置深度专注与轻量工作的运行平衡。",
          providers: "供应商",
          addProvider: "添加供应商",
          models: "模型",
          addModel: "添加模型",
          selectModels: "选择模型",
          mainModel: "主模型",
          selectMainModel: "选择主模型",
          efficientModel: "高效模型",
          selectEfficientModel: "选择高效模型",
          selectModelError: "请选择一个模型。",
          noProviders: "暂无供应商。使用加号按钮添加一个。",
          noModels: "暂无模型。先添加供应商，然后使用加号按钮添加模型。",
          editProviderAria: "编辑 {{name}}",
          deleteProviderAria: "删除 {{name}}",
          editModelAria: "编辑 {{name}}",
          deleteModelAria: "删除 {{name}}",
          auto: "自动",
          visionAuto: "自动",
          visionYes: "是",
          visionNo: "否",
          modelSummary:
            "上下文 {{context}} · 输出 {{output}} · 视觉 {{vision}}",
          providerKinds: {
            openai: {
              label: "OpenAI",
              description: "使用 API 密钥访问 OpenAI Responses 兼容接口。",
            },
            openai_codex_oauth: {
              label: "OpenAI Codex",
              description: "使用 ChatGPT Codex 的 OAuth 账户文件。",
            },
            github_copilot: {
              label: "GitHub Copilot",
              description: "使用 GitHub Copilot 账户令牌。",
            },
            openai_compatible: {
              label: "OpenAI 兼容",
              description: "使用 API 密钥和自定义 base URL。",
            },
            ollama: {
              label: "Ollama 本地",
              description: "使用本地 Ollama 端点。",
            },
            ollama_cloud: {
              label: "Ollama Cloud",
              description: "使用 Ollama Cloud API 密钥。",
            },
          },
          codexAuthMethods: {
            browser_login: {
              label: "浏览器登录",
              description:
                "打开 OpenAI 授权页面，并写入该供应商的 OAuth 文件。",
            },
            device_login: {
              label: "设备码登录",
              description: "显示设备码，并在浏览器中完成授权。",
            },
            import_local_codex: {
              label: "导入本地 Codex",
              description: "从本地 Codex CLI 读取 auth.json。",
            },
            import_auth_file: {
              label: "导入 auth.json",
              description: "从选定的 Codex auth.json 路径导入。",
            },
            existing_auth_file: {
              label: "使用现有的 Daat Locus OAuth 文件",
              description: "保留或手动放置该供应商的 OAuth 文件。",
            },
          },
          githubAuthMethods: {
            device_login: {
              label: "设备码登录",
              description: "通过 GitHub 设备流程获取 Copilot 访问令牌。",
            },
            manual_token: {
              label: "手动令牌",
              description: "粘贴一个 GitHub 令牌。",
            },
            env_token: {
              label: "环境变量",
              description: "保存一个 $GITHUB_TOKEN 引用。",
            },
          },
          providerDialog: {
            editTitle: "编辑供应商",
            addTitle: "添加供应商",
            description:
              "供应商定义凭据和 API 端点。模型将在下一部分绑定到供应商。",
            name: "名称",
            type: "类型",
            selectProviderType: "选择供应商类型",
            codexAuthMethodLabel: "Codex 认证方式",
            githubAuthMethodLabel: "GitHub 认证方式",
            selectAuthMethod: "选择认证方式",
            authFilePath: "auth.json 路径",
            githubToken: "GitHub 令牌",
            authentication: "认证",
            restart: "重新开始",
            completeAuthorization: "完成授权",
            apiKey: "API 密钥",
            host: "主机",
            baseUrl: "Base URL",
            baseUrlOptionalHint: "可选时留空则使用供应商默认值。",
            baseUrlRequiredError: "OpenAI 兼容需要 base URL。",
            baseUrlPlaceholderDefault: "使用供应商默认值",
            keepAlive: "keep_alive",
            cancel: "取消",
            saveProvider: "保存供应商",
            deviceAuthOpened:
              "授权页面已打开。在浏览器中输入设备码以完成授权。",
            authAction: {
              github: "开始设备码登录",
              browser_login: "打开浏览器登录",
              device_login: "开始设备码登录",
              import_local_codex: "导入本地 Codex",
              import_auth_file: "导入 auth.json",
              existing_auth_file: "检查 OAuth 文件",
            },
            authDescription: {
              github: "授权会将 GitHub 令牌写入当前供应商草稿。",
              browser_login:
                "登录后，Daat Locus 会为该供应商写入固定的 Codex OAuth 文件。",
              device_login: "开始流程，输入设备码，然后返回此处完成授权。",
              import_local_codex:
                "从本地 Codex CLI 的 auth.json 导入到该供应商。",
              import_auth_file: "从指定的 auth.json 导入到该供应商。",
              existing_auth_file:
                "检查该供应商的固定 Codex OAuth 文件是否存在。",
            },
            authSaveBlock: {
              github: "请先完成 GitHub 设备码登录。",
              browser_login: "请先完成浏览器登录。",
              device_login: "请先完成设备码登录。",
              import_local_codex: "请先导入本地 Codex 并等待完成。",
              import_auth_file: "请先导入 auth.json 并等待完成。",
              existing_auth_file: "请先检查现有的 OAuth 文件。",
            },
            summary: {
              codexDefaultEndpoint: "Codex OAuth · 默认端点",
              codexEndpoint: "Codex OAuth · {{url}}",
              githubCopilot: "GitHub Copilot · {{method}}",
              providerDefault: "供应商默认值",
            },
            errors: {
              nameRequired: "供应商名称不能为空。",
              nameExists: "供应商名称已存在。",
              apiKeyRequired: "该供应商需要 API 密钥。",
              githubTokenRequired: "GitHub Copilot 需要令牌或环境变量引用。",
              baseUrlRequired: "OpenAI 兼容供应商需要 base URL。",
              authFileRequired: "该 Codex 认证方式需要 auth.json 路径。",
              enterNameFirst: "请先输入供应商名称。",
              enterAuthFileFirst: "请先输入 auth.json 路径。",
            },
          },
          modelDialog: {
            editTitle: "编辑模型",
            addTitle: "添加模型",
            description: "模型定义绑定到供应商，可被选为主模型或高效模型。",
            provider: "供应商",
            selectProvider: "选择供应商",
            discoveredModels: "发现的模型",
            rediscover: "重新发现",
            selectModelOrManual: "选择一个模型或手动输入",
            manualInput: "手动输入",
            modelName: "模型名称",
            modelId: "模型 ID",
            contextWindowTokens: "上下文窗口 tokens",
            maxCompletionTokens: "最大输出 tokens",
            vision: "视觉",
            visionAuto: "自动",
            visionSupported: "支持",
            visionUnsupported: "不支持",
            apiStyle: "API 风格",
            apiStyleChatCompletions: "Chat completions（默认）",
            apiStyleResponses: "Responses",
            apiStyleDescription:
              "为该 openai-compatible 模型选择端点协议：chat completions 或 responses API。",
            reasoning: "推理 / 思考",
            notConfigured: "未配置",
            custom: "自定义",
            customReasoningPlaceholder: "输入自定义的推理 / 思考值",
            cancel: "取消",
            saveModel: "保存模型",
            discovery: {
              selectProviderFirst: "请先选择一个供应商。",
              loading: "正在从该供应商发现模型。",
              loadedSome: "发现了 {{count}} 个模型。也可以手动输入模型 ID。",
              loadedNone: "未发现模型。可以手动输入模型 ID。",
              idle: "选择供应商后会自动发现模型。",
            },
            errors: {
              providerRequired: "请选择一个供应商。",
              nameRequired: "模型名称不能为空。",
              nameExists: "模型名称已存在。",
              modelIdRequired: "模型 ID 不能为空。",
              contextWindowInvalid: "上下文窗口 tokens 必须是正整数。",
              maxCompletionInvalid: "最大输出 tokens 必须是正整数。",
            },
          },
        },
        validation: {
          providerRequired: "请至少添加一个供应商。",
          modelRequired: "请至少添加一个模型。",
          mainAndEfficientModelsRequired: "请选择有效的主模型和高效模型。",
        },
      },
      login: {
        daemonToken: "守护进程令牌",
        tokenPlaceholder: "令牌",
        enterToken: "请输入守护进程令牌。",
        verifyingToken: "正在验证令牌…",
        verifiedToken: "令牌已验证。后续页面将复用此令牌。",
        verifying: "正在验证",
        submit: "登录",
      },
      sidebar: {
        open: "打开侧边栏",
        projects: "项目",
        noProjects: "暂无项目",
        conversations: "会话",
        noChats: "暂无聊天",
        newCodingSession: "新建代码会话",
        newProjectSession: "新建项目会话",
        newSessionInProject: "在 {{project}} 中新建会话",
        newConversation: "新建会话",
        showMore: "显示更多",
        showLess: "收起",
        deleteSessionAria: "删除 {{title}}",
        deleteSessionTitle: "删除会话",
        deleteDialogTitle: "删除会话？",
        deleteDialogDescription: "这将永久删除 {{title}}（{{id}}）。",
        relativeTime: {
          now: "刚刚",
          minute: "{{count}} 分钟",
          hour: "{{count}} 小时",
          day: "{{count}} 天",
          month: "{{count}} 月",
          year: "{{count}} 年",
        },
      },
      theme: {
        switchToLight: "切换到浅色模式",
        switchToDark: "切换到深色模式",
        lightMode: "浅色模式",
        darkMode: "深色模式",
      },
      settings: {
        pageAria: "设置",
        unableToLoad: "无法加载设置",
        configRestored: "配置文件已恢复",
        unableToSave: "无法保存设置",
        interface: {
          title: "界面",
          description: "选择 WebUI 标签、导航和控件的显示语言。",
          languageLabel: "WebUI 语言",
          languageDescription: "此设置会保存到共享的 Daat Locus 语言配置。",
          languagePlaceholder: "选择语言",
        },
        telegram: {
          title: "Telegram",
          description:
            "启用 Telegram 传输，并提供用于接收消息和发送事件回复的机器人令牌。",
          enableLabel: "启用 Telegram",
          enableDescription:
            "只有打开此开关且令牌是真实的 Bot API 令牌时，传输才会启动。",
          enableAria: "启用 Telegram 传输",
          botToken: "机器人令牌",
          botTokenDescription:
            "粘贴来自 <botFather>BotFather</botFather> 的令牌。",
        },
        modelAccess: {
          providerDescription: "调校代理模型能力背后的安全访问层。",
          modelDescription: "将可用模型容量整理成可靠的运行时目录。",
          selectionDescription: "设置深度、速度与日常工作之间的运行平衡。",
        },
        validation: {
          providerRequired: "请至少添加一个供应商。",
          modelRequired: "请至少添加一个模型。",
          mainModelRequired: "请选择有效的主模型。",
          efficientModelRequired: "请选择有效的高效模型。",
        },
      },
      status: {
        pageAria: "状态",
        unableToLoad: "无法加载状态",
        reorderCard: "重新排序 {{label}} 卡片",
        dragToReorder: "拖动以重新排序 {{label}}",
        cards: {
          contextComposition: "上下文组成",
          tokenUsage: "Token 用量",
        },
        session: "会话",
        noSession: "暂无会话",
        context: "上下文",
        noContextSnapshot: "暂无上下文快照",
        noSessionsFound: "未找到会话",
        contextNoSnapshotDescription: "此会话尚未组装模型请求上下文。",
        contextNoSessionsDescription: "会话发布状态数据后会显示上下文组成。",
        contextHeatmapLabel:
          "上下文组成热力图。当前自适应布局为 {{columns}} × {{rows}}。",
        contextCellLabel: "每个单元最多代表 {{tokens}} 个预估 token。",
        contextDisplayAria:
          "{{gridLabel}} 正在为 {{session}} 显示 {{occupied}} 个占用单元，矩形显示范围为 {{displayScale}}。",
        tokenCount: "{{count}} tokens",
        total: "总计",
        cached: "缓存",
        uncached: "未缓存",
        noTokenUsage: "暂无 token 用量记录",
        tokenUsageDescription: "会话发起模型请求后会显示用量柱状图。",
      },
      logs: {
        pageAria: "日志",
        loadingLogs: "正在加载日志",
        sourceLoadFailed: "无法加载日志来源。",
        live: "实时",
        missing: "缺失",
        search: "搜索日志",
        title: "日志",
        loadingSources: "正在加载日志来源…",
        noSourceSelected: "未选择日志来源。",
        readFailed: "无法读取此日志。",
        loadingEntries: "正在加载日志条目…",
        noEntries: "暂无日志条目。",
        noLevelEntries: "没有 {{level}} 或更高级别的日志条目。",
        noMatchingEntries: "没有匹配的日志条目。",
        blank: "（空白）",
      },
    },
  },
};

const initialLanguage = initialWebUiLanguage();
applyDocumentLanguage(initialLanguage);

void i18n.use(initReactI18next).init({
  resources,
  lng: initialLanguage,
  fallbackLng: DEFAULT_WEBUI_LOCALE,
  supportedLngs: [...WEBUI_LOCALES],
  returnNull: false,
  interpolation: {
    escapeValue: false,
  },
  react: {
    useSuspense: false,
  },
});

i18n.on("languageChanged", (language) => {
  applyDocumentLanguage(normalizeWebUiLocale(language));
});

export function getCurrentWebUiLanguage(): WebUiLocale {
  return normalizeWebUiLocale(i18n.resolvedLanguage ?? i18n.language);
}

export async function setWebUiLanguage(locale: string | null | undefined) {
  const nextLocale = normalizeWebUiLocale(locale);
  applyDocumentLanguage(nextLocale);
  storeWebUiLanguage(nextLocale);

  if (
    normalizeWebUiLocale(i18n.resolvedLanguage ?? i18n.language) !== nextLocale
  ) {
    await i18n.changeLanguage(nextLocale);
  }

  return nextLocale;
}

export default i18n;
