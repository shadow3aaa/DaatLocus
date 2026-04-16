use std::{any::Any, collections::HashMap, fmt::Display, time::Duration};

use async_trait::async_trait;
use miette::{Result, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{browser_app::BrowserApp, sandbox::RuntimeSandboxPolicy, terminal_app::TerminalApp};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct AppId(String);

impl AppId {
    pub const DEFAULT_WORKSPACE_ENTRY: &str = "runtime/app.lua";

    pub fn browser() -> Self {
        Self("Browser".to_string())
    }

    pub fn terminal() -> Self {
        Self("Terminal".to_string())
    }

    pub fn from_workspace_folder(name: impl Into<String>) -> Result<Self> {
        let name = name.into();
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(miette!("workspace app folder name cannot be empty"));
        }
        if trimmed.contains(std::path::MAIN_SEPARATOR) || trimmed.contains('/') || trimmed == "." {
            return Err(miette!("invalid workspace app folder name `{trimmed}`"));
        }
        if !trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
        {
            return Err(miette!(
                "workspace app folder name `{trimmed}` must use only ASCII letters, numbers, `_`, or `-`"
            ));
        }
        if trimmed == Self::browser().as_str() || trimmed == Self::terminal().as_str() {
            return Err(miette!("workspace app id `{trimmed}` is reserved"));
        }
        Ok(Self(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_terminal(&self) -> bool {
        self.as_str() == Self::terminal().as_str()
    }
}

impl Display for AppId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppToolScope {
    Browser,
    Terminal,
}

#[derive(Debug, Clone)]
pub struct AppStateRender {
    pub title: String,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AppUsage {
    pub description: String,
    pub when_to_focus: Vec<String>,
    pub body_markdown: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AppHowToUse {
    pub lines: Vec<String>,
    pub body_markdown: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AppDynamicToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone)]
pub struct AppDynamicToolResult {
    pub summary: String,
    pub payload: Value,
    pub model_content: Option<String>,
    pub ui_lines: Vec<String>,
    pub turn_boundary_reason: Option<String>,
}

#[async_trait]
pub trait App: Send + Sync {
    fn id(&self) -> AppId;

    fn as_any(&self) -> &dyn Any;

    fn as_any_mut(&mut self) -> &mut dyn Any;

    fn render_state(&self) -> AppStateRender;

    fn usage(&self) -> AppUsage;

    fn how_to_use(&self) -> AppHowToUse;

    fn focused_tool_scopes(&self) -> &'static [AppToolScope] {
        &[]
    }

    fn dynamic_tools(&self) -> Result<Vec<AppDynamicToolSpec>> {
        Ok(Vec::new())
    }

    async fn execute_dynamic_tool(
        &mut self,
        _name: &str,
        _arguments: Value,
    ) -> Result<AppDynamicToolResult> {
        Err(miette!("unknown app tool"))
    }

    async fn on_focus(&mut self) -> Result<()> {
        Ok(())
    }

    async fn on_blur(&mut self) -> Result<()> {
        Ok(())
    }

    async fn refresh_notice(&mut self) -> Result<()> {
        Ok(())
    }

    fn notice_reason(&self) -> Option<String> {
        None
    }

    async fn shutdown(&mut self) -> Result<()> {
        Ok(())
    }

    async fn wait_until_settled(&self, _: Duration, _: Duration) -> bool {
        true
    }
}

pub struct AppManager {
    focused: Option<AppId>,
    order: Vec<AppId>,
    apps: HashMap<AppId, Box<dyn App>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppInstallDisposition {
    Added,
    Replaced,
}

impl AppManager {
    pub async fn new(focused: Option<AppId>, apps: Vec<Box<dyn App>>) -> Result<Self> {
        let mut order = Vec::with_capacity(apps.len());
        let mut table = HashMap::with_capacity(apps.len());

        for app in apps {
            let id = app.id();
            if table.insert(id.clone(), app).is_some() {
                return Err(miette!("duplicated app id: {id}"));
            }
            order.push(id);
        }

        let mut manager = Self {
            focused: None,
            order,
            apps: table,
        };
        if let Some(id) = focused {
            manager.focus(id).await?;
        }
        Ok(manager)
    }

    pub fn focused(&self) -> Option<AppId> {
        self.focused.clone()
    }

    pub fn state_renders(&self) -> Vec<(AppId, AppStateRender)> {
        self.order
            .iter()
            .filter_map(|id| {
                self.apps
                    .get(id)
                    .map(|app| (id.clone(), app.render_state()))
            })
            .collect()
    }

    pub fn usage(&self, id: &AppId) -> Option<AppUsage> {
        self.apps.get(id).map(|app| app.usage())
    }

    pub fn how_to_use(&self, id: &AppId) -> Option<AppHowToUse> {
        self.apps.get(id).map(|app| app.how_to_use())
    }

    pub fn focused_tool_scopes(&self) -> &'static [AppToolScope] {
        let Some(focused) = self.focused.clone() else {
            return &[];
        };
        self.apps
            .get(&focused)
            .map(|app| app.focused_tool_scopes())
            .unwrap_or(&[])
    }

    pub fn app_ids(&self) -> Vec<AppId> {
        self.order.clone()
    }

    pub fn notice_reason(&self, id: &AppId) -> Option<String> {
        self.apps.get(id).and_then(|app| app.notice_reason())
    }

    pub async fn refresh_all_notices(&mut self) -> Result<()> {
        for id in self.order.clone() {
            if let Some(app) = self.apps.get_mut(&id) {
                app.refresh_notice().await?;
            }
        }
        Ok(())
    }

    pub async fn refresh_notice_for(&mut self, id: &AppId) -> Result<()> {
        if let Some(app) = self.apps.get_mut(id) {
            app.refresh_notice().await?;
        }
        Ok(())
    }

    pub fn dynamic_tools(&self) -> Result<Vec<AppDynamicToolSpec>> {
        let Some(focused) = self.focused.clone() else {
            return Ok(Vec::new());
        };
        let app = self
            .apps
            .get(&focused)
            .ok_or_else(|| miette!("focused app missing: {focused}"))?;
        app.dynamic_tools()
    }

    pub async fn focus(&mut self, id: AppId) -> Result<()> {
        if self.focused.as_ref() == Some(&id) {
            return Ok(());
        }

        if !self.apps.contains_key(&id) {
            return Err(miette!("unknown app: {id}"));
        }

        if let Some(current) = self.focused.clone()
            && let Some(app) = self.apps.get_mut(&current)
        {
            app.on_blur().await?;
        }

        self.focused = Some(id.clone());
        if let Some(app) = self.apps.get_mut(&id) {
            app.on_focus().await?;
        }
        Ok(())
    }

    pub async fn put_away(&mut self) -> Result<()> {
        let Some(current) = self.focused.take() else {
            return Ok(());
        };
        if let Some(app) = self.apps.get_mut(&current) {
            app.on_blur().await?;
        }
        Ok(())
    }

    pub async fn install_or_replace(
        &mut self,
        mut app: Box<dyn App>,
    ) -> Result<AppInstallDisposition> {
        let id = app.id();
        let was_focused = self.focused.as_ref() == Some(&id);
        let disposition = if let Some(mut previous) = self.apps.remove(&id) {
            previous.shutdown().await?;
            AppInstallDisposition::Replaced
        } else {
            self.order.push(id.clone());
            AppInstallDisposition::Added
        };
        if was_focused {
            app.on_focus().await?;
        }
        self.apps.insert(id, app);
        Ok(disposition)
    }

    pub async fn remove(&mut self, id: &AppId) -> Result<bool> {
        let was_focused = self.focused.as_ref() == Some(id);
        if was_focused {
            self.focused = None;
        }
        let Some(mut app) = self.apps.remove(id) else {
            return Ok(false);
        };
        self.order.retain(|existing| existing != id);
        if was_focused {
            app.on_blur().await?;
        }
        app.shutdown().await?;
        Ok(true)
    }

    pub async fn wait_until_settled(&self, silence_duration: Duration, timeout: Duration) -> bool {
        let Some(focused) = self.focused.clone() else {
            return true;
        };
        let Some(app) = self.apps.get(&focused) else {
            return true;
        };
        app.wait_until_settled(silence_duration, timeout).await
    }

    fn focused_app_mut(&mut self) -> Result<&mut Box<dyn App>> {
        let Some(focused) = self.focused.clone() else {
            return Err(miette!("no focused app"));
        };
        self.apps
            .get_mut(&focused)
            .ok_or_else(|| miette!("focused app missing: {focused}"))
    }

    pub async fn terminal_exec_with_progress<F>(
        &mut self,
        command: String,
        session_id: Option<String>,
        workdir: Option<String>,
        sandbox_policy: &RuntimeSandboxPolicy,
        yield_time_ms: Option<u64>,
        max_chars: Option<usize>,
        on_progress: F,
    ) -> Result<crate::terminal_app::TerminalToolResult>
    where
        F: FnMut(&crate::terminal_app::TerminalSessionState, &str) + Send,
    {
        let focused = self.focused.clone();
        let app = self.focused_app_mut()?;
        let terminal = app
            .as_any_mut()
            .downcast_mut::<TerminalApp>()
            .ok_or_else(|| miette!("focused app is not Terminal: {:?}", focused))?;
        terminal
            .exec_command_with_progress(
                command,
                session_id,
                workdir,
                sandbox_policy,
                yield_time_ms,
                max_chars,
                on_progress,
            )
            .await
    }

    pub async fn terminal_write_stdin_with_progress<F>(
        &mut self,
        session_id: &str,
        text: String,
        yield_time_ms: Option<u64>,
        max_chars: Option<usize>,
        on_progress: F,
    ) -> Result<crate::terminal_app::TerminalToolResult>
    where
        F: FnMut(&crate::terminal_app::TerminalSessionState, &str) + Send,
    {
        let focused = self.focused.clone();
        let app = self.focused_app_mut()?;
        let terminal = app
            .as_any_mut()
            .downcast_mut::<TerminalApp>()
            .ok_or_else(|| miette!("focused app is not Terminal: {:?}", focused))?;
        terminal
            .write_stdin_with_progress(session_id, text, yield_time_ms, max_chars, on_progress)
            .await
    }

    pub async fn terminal_terminate(
        &mut self,
        session_id: &str,
    ) -> Result<crate::terminal_app::TerminalSessionState> {
        let focused = self.focused.clone();
        let app = self.focused_app_mut()?;
        let terminal = app
            .as_any_mut()
            .downcast_mut::<TerminalApp>()
            .ok_or_else(|| miette!("focused app is not Terminal: {:?}", focused))?;
        terminal.terminate_session(session_id).await
    }

    pub async fn browser_open_page(
        &mut self,
        url: &str,
    ) -> Result<crate::browser_app::BrowserOpenResult> {
        let focused = self.focused.clone();
        let app = self.focused_app_mut()?;
        let browser = app
            .as_any_mut()
            .downcast_mut::<BrowserApp>()
            .ok_or_else(|| miette!("focused app is not Browser: {:?}", focused))?;
        browser.open_page(url).await
    }

    pub async fn browser_snapshot(
        &mut self,
        page_id: &str,
    ) -> Result<crate::browser_app::BrowserSnapshotResult> {
        let focused = self.focused.clone();
        let app = self.focused_app_mut()?;
        let browser = app
            .as_any_mut()
            .downcast_mut::<BrowserApp>()
            .ok_or_else(|| miette!("focused app is not Browser: {:?}", focused))?;
        browser.snapshot_page(page_id).await
    }

    pub async fn browser_wait(
        &mut self,
        page_id: &str,
        state: Option<&str>,
        timeout_ms: Option<u64>,
    ) -> Result<crate::browser_app::BrowserWaitResult> {
        let focused = self.focused.clone();
        let app = self.focused_app_mut()?;
        let browser = app
            .as_any_mut()
            .downcast_mut::<BrowserApp>()
            .ok_or_else(|| miette!("focused app is not Browser: {:?}", focused))?;
        browser.wait_for_page(page_id, state, timeout_ms).await
    }

    pub async fn browser_click(
        &mut self,
        page_id: &str,
        element_ref: &str,
    ) -> Result<crate::browser_app::BrowserActionResult> {
        let focused = self.focused.clone();
        let app = self.focused_app_mut()?;
        let browser = app
            .as_any_mut()
            .downcast_mut::<BrowserApp>()
            .ok_or_else(|| miette!("focused app is not Browser: {:?}", focused))?;
        browser.click(page_id, element_ref).await
    }

    pub async fn browser_fill(
        &mut self,
        page_id: &str,
        element_ref: &str,
        value: &str,
    ) -> Result<crate::browser_app::BrowserActionResult> {
        let focused = self.focused.clone();
        let app = self.focused_app_mut()?;
        let browser = app
            .as_any_mut()
            .downcast_mut::<BrowserApp>()
            .ok_or_else(|| miette!("focused app is not Browser: {:?}", focused))?;
        browser.fill(page_id, element_ref, value).await
    }

    pub async fn browser_back(
        &mut self,
        page_id: &str,
    ) -> Result<crate::browser_app::BrowserActionResult> {
        let focused = self.focused.clone();
        let app = self.focused_app_mut()?;
        let browser = app
            .as_any_mut()
            .downcast_mut::<BrowserApp>()
            .ok_or_else(|| miette!("focused app is not Browser: {:?}", focused))?;
        browser.go_back(page_id).await
    }

    pub async fn browser_forward(
        &mut self,
        page_id: &str,
    ) -> Result<crate::browser_app::BrowserActionResult> {
        let focused = self.focused.clone();
        let app = self.focused_app_mut()?;
        let browser = app
            .as_any_mut()
            .downcast_mut::<BrowserApp>()
            .ok_or_else(|| miette!("focused app is not Browser: {:?}", focused))?;
        browser.go_forward(page_id).await
    }

    pub async fn browser_reload(
        &mut self,
        page_id: &str,
    ) -> Result<crate::browser_app::BrowserActionResult> {
        let focused = self.focused.clone();
        let app = self.focused_app_mut()?;
        let browser = app
            .as_any_mut()
            .downcast_mut::<BrowserApp>()
            .ok_or_else(|| miette!("focused app is not Browser: {:?}", focused))?;
        browser.reload(page_id).await
    }

    pub async fn browser_close_page(
        &mut self,
        page_id: &str,
    ) -> Result<crate::browser_app::BrowserActionResult> {
        let focused = self.focused.clone();
        let app = self.focused_app_mut()?;
        let browser = app
            .as_any_mut()
            .downcast_mut::<BrowserApp>()
            .ok_or_else(|| miette!("focused app is not Browser: {:?}", focused))?;
        browser.close_page(page_id).await
    }

    pub fn terminal_session_state(
        &self,
        session_id: &str,
    ) -> Result<crate::terminal_app::TerminalSessionState> {
        let focused = self
            .focused
            .clone()
            .ok_or_else(|| miette!("no focused app"))?;
        let app = self
            .apps
            .get(&focused)
            .ok_or_else(|| miette!("focused app missing: {focused}"))?;
        let terminal = app
            .as_any()
            .downcast_ref::<TerminalApp>()
            .ok_or_else(|| miette!("focused app is not Terminal: {:?}", focused))?;
        terminal.session_state(session_id)
    }

    pub async fn execute_dynamic_tool(
        &mut self,
        name: &str,
        arguments: Value,
    ) -> Result<AppDynamicToolResult> {
        let app = self.focused_app_mut()?;
        app.execute_dynamic_tool(name, arguments).await
    }

    pub async fn shutdown(mut self) -> Result<()> {
        for id in self.order {
            if let Some(app) = self.apps.get_mut(&id) {
                app.shutdown().await?;
            }
        }
        Ok(())
    }
}
