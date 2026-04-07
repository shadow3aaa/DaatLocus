use std::{any::Any, collections::HashMap, fmt::Display, time::Duration};

use async_trait::async_trait;
use miette::{Result, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{browser_app::BrowserApp, sandbox::RuntimeSandboxPolicy, terminal_app::TerminalApp};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize, JsonSchema)]
pub enum AppId {
    Browser,
    Terminal,
}

impl Display for AppId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Browser => write!(f, "Browser"),
            Self::Terminal => write!(f, "Terminal"),
        }
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
    pub purpose: String,
    pub when_to_focus: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AppHowToUse {
    pub lines: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AppSkillSummary {
    pub id: String,
    pub name: String,
    pub when_to_use: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AppSkillContent {
    pub id: String,
    pub title: String,
    pub body: String,
}

#[async_trait]
pub trait App: Send + Sync {
    fn id(&self) -> AppId;

    fn as_any(&self) -> &dyn Any;

    fn as_any_mut(&mut self) -> &mut dyn Any;

    fn render_state(&self) -> AppStateRender;

    fn usage(&self) -> AppUsage;

    fn how_to_use(&self) -> AppHowToUse;

    fn skills(&self) -> Vec<AppSkillSummary> {
        Vec::new()
    }

    fn read_skill(&self, _id: &str) -> Result<AppSkillContent> {
        Err(miette!("unknown app skill"))
    }

    fn focused_tool_scopes(&self) -> &'static [AppToolScope] {
        &[]
    }

    async fn on_focus(&mut self) -> Result<()> {
        Ok(())
    }

    async fn on_blur(&mut self) -> Result<()> {
        Ok(())
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

impl AppManager {
    pub async fn new(focused: Option<AppId>, apps: Vec<Box<dyn App>>) -> Result<Self> {
        let mut order = Vec::with_capacity(apps.len());
        let mut table = HashMap::with_capacity(apps.len());

        for app in apps {
            let id = app.id();
            if table.insert(id, app).is_some() {
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
        self.focused
    }

    pub fn state_renders(&self) -> Vec<(AppId, AppStateRender)> {
        self.order
            .iter()
            .filter_map(|id| self.apps.get(id).map(|app| (*id, app.render_state())))
            .collect()
    }

    pub fn usage(&self, id: AppId) -> Option<AppUsage> {
        self.apps.get(&id).map(|app| app.usage())
    }

    pub fn how_to_use(&self, id: AppId) -> Option<AppHowToUse> {
        self.apps.get(&id).map(|app| app.how_to_use())
    }

    pub fn all_skills(&self) -> Vec<(AppId, Vec<AppSkillSummary>)> {
        self.order
            .iter()
            .filter_map(|id| self.apps.get(id).map(|app| (*id, app.skills())))
            .collect()
    }

    pub fn read_skill(&self, id: &str) -> Result<AppSkillContent> {
        let Some(focused) = self.focused else {
            return Err(miette!(
                "no focused app; focus the matching app before reading its skill"
            ));
        };
        let app = self
            .apps
            .get(&focused)
            .ok_or_else(|| miette!("focused app missing: {focused}"))?;
        app.read_skill(id).map_err(|_| {
            miette!(
                "skill `{id}` is not available on focused app {focused}; focus the matching app first"
            )
        })
    }

    pub fn focused_tool_scopes(&self) -> &'static [AppToolScope] {
        let Some(focused) = self.focused else {
            return &[];
        };
        self.apps
            .get(&focused)
            .map(|app| app.focused_tool_scopes())
            .unwrap_or(&[])
    }

    pub async fn focus(&mut self, id: AppId) -> Result<()> {
        if self.focused == Some(id) {
            return Ok(());
        }

        if !self.apps.contains_key(&id) {
            return Err(miette!("unknown app: {id}"));
        }

        if let Some(current) = self.focused
            && let Some(app) = self.apps.get_mut(&current)
        {
            app.on_blur().await?;
        }

        self.focused = Some(id);
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

    pub async fn wait_until_settled(&self, silence_duration: Duration, timeout: Duration) -> bool {
        let Some(focused) = self.focused else {
            return true;
        };
        let Some(app) = self.apps.get(&focused) else {
            return true;
        };
        app.wait_until_settled(silence_duration, timeout).await
    }

    fn focused_app_mut(&mut self) -> Result<&mut Box<dyn App>> {
        let Some(focused) = self.focused else {
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
        let focused = self.focused;
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
        let focused = self.focused;
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
        let focused = self.focused;
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
        let focused = self.focused;
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
        let focused = self.focused;
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
        let focused = self.focused;
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
        let focused = self.focused;
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
        let focused = self.focused;
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
        let focused = self.focused;
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
        let focused = self.focused;
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
        let focused = self.focused;
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
        let focused = self.focused;
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
        let focused = self.focused.ok_or_else(|| miette!("no focused app"))?;
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

    pub async fn shutdown(mut self) -> Result<()> {
        for id in self.order {
            if let Some(app) = self.apps.get_mut(&id) {
                app.shutdown().await?;
            }
        }
        Ok(())
    }
}
