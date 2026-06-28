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
        deleteDialogDescription:
          "This permanently deletes {{title}} ({{id}}).",
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
          "Context composition heatmap. The base layout is {{columns}} by {{rows}}.",
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
          personaContentDescription:
            "支持 {{token}}；此内容会写入人格提示词。",
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
          modelSummary: "上下文 {{context}} · 输出 {{output}} · 视觉 {{vision}}",
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
          "上下文组成热力图。基础布局为 {{columns}} × {{rows}}。",
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

  if (normalizeWebUiLocale(i18n.resolvedLanguage ?? i18n.language) !== nextLocale) {
    await i18n.changeLanguage(nextLocale);
  }

  return nextLocale;
}

export default i18n;
