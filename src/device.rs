use std::{any::Any, collections::HashMap, fmt::Display, time::Duration};

use async_trait::async_trait;
use miette::{Result, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    browser_device::BrowserDevice, sandbox::RuntimeSandboxPolicy, terminal_device::TerminalDevice,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize, JsonSchema)]
pub enum DeviceId {
    Browser,
    Terminal,
}

impl Display for DeviceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Browser => write!(f, "Browser"),
            Self::Terminal => write!(f, "Terminal"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceToolScope {
    Browser,
    Terminal,
}

#[derive(Debug, Clone)]
pub struct DeviceStateRender {
    pub title: String,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DeviceUsage {
    pub purpose: String,
    pub when_to_focus: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DeviceHowToUse {
    pub lines: Vec<String>,
}

#[async_trait]
pub trait Device: Send + Sync {
    fn id(&self) -> DeviceId;

    fn as_any(&self) -> &dyn Any;

    fn as_any_mut(&mut self) -> &mut dyn Any;

    fn render_state(&self) -> DeviceStateRender;

    fn usage(&self) -> DeviceUsage;

    fn how_to_use(&self) -> DeviceHowToUse;

    fn focused_tool_scopes(&self) -> &'static [DeviceToolScope] {
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

pub struct DeviceManager {
    focused: Option<DeviceId>,
    order: Vec<DeviceId>,
    devices: HashMap<DeviceId, Box<dyn Device>>,
}

impl DeviceManager {
    pub async fn new(focused: Option<DeviceId>, devices: Vec<Box<dyn Device>>) -> Result<Self> {
        let mut order = Vec::with_capacity(devices.len());
        let mut table = HashMap::with_capacity(devices.len());

        for device in devices {
            let id = device.id();
            if table.insert(id, device).is_some() {
                return Err(miette!("duplicated device id: {id}"));
            }
            order.push(id);
        }

        let mut manager = Self {
            focused: None,
            order,
            devices: table,
        };
        if let Some(id) = focused {
            manager.focus(id).await?;
        }
        Ok(manager)
    }

    pub fn focused(&self) -> Option<DeviceId> {
        self.focused
    }

    pub fn state_renders(&self) -> Vec<(DeviceId, DeviceStateRender)> {
        self.order
            .iter()
            .filter_map(|id| {
                self.devices
                    .get(id)
                    .map(|device| (*id, device.render_state()))
            })
            .collect()
    }

    pub fn usage(&self, id: DeviceId) -> Option<DeviceUsage> {
        self.devices.get(&id).map(|device| device.usage())
    }

    pub fn how_to_use(&self, id: DeviceId) -> Option<DeviceHowToUse> {
        self.devices.get(&id).map(|device| device.how_to_use())
    }

    pub fn focused_tool_scopes(&self) -> &'static [DeviceToolScope] {
        let Some(focused) = self.focused else {
            return &[];
        };
        self.devices
            .get(&focused)
            .map(|device| device.focused_tool_scopes())
            .unwrap_or(&[])
    }

    pub async fn focus(&mut self, id: DeviceId) -> Result<()> {
        if self.focused == Some(id) {
            return Ok(());
        }

        if !self.devices.contains_key(&id) {
            return Err(miette!("unknown device: {id}"));
        }

        if let Some(current) = self.focused
            && let Some(device) = self.devices.get_mut(&current)
        {
            device.on_blur().await?;
        }

        self.focused = Some(id);
        if let Some(device) = self.devices.get_mut(&id) {
            device.on_focus().await?;
        }
        Ok(())
    }

    pub async fn put_away(&mut self) -> Result<()> {
        let Some(current) = self.focused.take() else {
            return Ok(());
        };
        if let Some(device) = self.devices.get_mut(&current) {
            device.on_blur().await?;
        }
        Ok(())
    }

    pub async fn wait_until_settled(&self, silence_duration: Duration, timeout: Duration) -> bool {
        let Some(focused) = self.focused else {
            return true;
        };
        let Some(device) = self.devices.get(&focused) else {
            return true;
        };
        device.wait_until_settled(silence_duration, timeout).await
    }

    fn focused_device_mut(&mut self) -> Result<&mut Box<dyn Device>> {
        let Some(focused) = self.focused else {
            return Err(miette!("no focused device"));
        };
        self.devices
            .get_mut(&focused)
            .ok_or_else(|| miette!("focused device missing: {focused}"))
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
    ) -> Result<crate::terminal_device::TerminalToolResult>
    where
        F: FnMut(&crate::terminal_device::TerminalSessionState, &str) + Send,
    {
        let focused = self.focused;
        let device = self.focused_device_mut()?;
        let terminal = device
            .as_any_mut()
            .downcast_mut::<TerminalDevice>()
            .ok_or_else(|| miette!("focused device is not Terminal: {:?}", focused))?;
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
    ) -> Result<crate::terminal_device::TerminalToolResult>
    where
        F: FnMut(&crate::terminal_device::TerminalSessionState, &str) + Send,
    {
        let focused = self.focused;
        let device = self.focused_device_mut()?;
        let terminal = device
            .as_any_mut()
            .downcast_mut::<TerminalDevice>()
            .ok_or_else(|| miette!("focused device is not Terminal: {:?}", focused))?;
        terminal
            .write_stdin_with_progress(session_id, text, yield_time_ms, max_chars, on_progress)
            .await
    }

    pub async fn terminal_terminate(
        &mut self,
        session_id: &str,
    ) -> Result<crate::terminal_device::TerminalSessionState> {
        let focused = self.focused;
        let device = self.focused_device_mut()?;
        let terminal = device
            .as_any_mut()
            .downcast_mut::<TerminalDevice>()
            .ok_or_else(|| miette!("focused device is not Terminal: {:?}", focused))?;
        terminal.terminate_session(session_id).await
    }

    pub async fn browser_open(
        &mut self,
        url: &str,
    ) -> Result<crate::browser_device::BrowserOpenResult> {
        let focused = self.focused;
        let device = self.focused_device_mut()?;
        let browser = device
            .as_any_mut()
            .downcast_mut::<BrowserDevice>()
            .ok_or_else(|| miette!("focused device is not Browser: {:?}", focused))?;
        browser.open_page(url).await
    }

    pub async fn browser_snapshot(
        &mut self,
        page_id: &str,
    ) -> Result<crate::browser_device::BrowserSnapshotResult> {
        let focused = self.focused;
        let device = self.focused_device_mut()?;
        let browser = device
            .as_any_mut()
            .downcast_mut::<BrowserDevice>()
            .ok_or_else(|| miette!("focused device is not Browser: {:?}", focused))?;
        browser.snapshot_page(page_id).await
    }

    pub async fn browser_find_in_page(
        &mut self,
        page_id: &str,
        query: &str,
        max_results: usize,
    ) -> Result<crate::browser_device::BrowserFindResult> {
        let focused = self.focused;
        let device = self.focused_device_mut()?;
        let browser = device
            .as_any_mut()
            .downcast_mut::<BrowserDevice>()
            .ok_or_else(|| miette!("focused device is not Browser: {:?}", focused))?;
        browser.find_in_page(page_id, query, max_results).await
    }

    pub async fn browser_wait(
        &mut self,
        page_id: &str,
        state: Option<&str>,
        timeout_ms: Option<u64>,
    ) -> Result<crate::browser_device::BrowserWaitResult> {
        let focused = self.focused;
        let device = self.focused_device_mut()?;
        let browser = device
            .as_any_mut()
            .downcast_mut::<BrowserDevice>()
            .ok_or_else(|| miette!("focused device is not Browser: {:?}", focused))?;
        browser.wait_for_page(page_id, state, timeout_ms).await
    }

    pub async fn browser_click(
        &mut self,
        page_id: &str,
        snapshot_id: &str,
        element_ref: &str,
    ) -> Result<crate::browser_device::BrowserActionResult> {
        let focused = self.focused;
        let device = self.focused_device_mut()?;
        let browser = device
            .as_any_mut()
            .downcast_mut::<BrowserDevice>()
            .ok_or_else(|| miette!("focused device is not Browser: {:?}", focused))?;
        browser.click(page_id, snapshot_id, element_ref).await
    }

    pub async fn browser_fill(
        &mut self,
        page_id: &str,
        snapshot_id: &str,
        element_ref: &str,
        value: &str,
    ) -> Result<crate::browser_device::BrowserActionResult> {
        let focused = self.focused;
        let device = self.focused_device_mut()?;
        let browser = device
            .as_any_mut()
            .downcast_mut::<BrowserDevice>()
            .ok_or_else(|| miette!("focused device is not Browser: {:?}", focused))?;
        browser.fill(page_id, snapshot_id, element_ref, value).await
    }

    pub async fn browser_back(
        &mut self,
        page_id: &str,
    ) -> Result<crate::browser_device::BrowserActionResult> {
        let focused = self.focused;
        let device = self.focused_device_mut()?;
        let browser = device
            .as_any_mut()
            .downcast_mut::<BrowserDevice>()
            .ok_or_else(|| miette!("focused device is not Browser: {:?}", focused))?;
        browser.go_back(page_id).await
    }

    pub async fn browser_forward(
        &mut self,
        page_id: &str,
    ) -> Result<crate::browser_device::BrowserActionResult> {
        let focused = self.focused;
        let device = self.focused_device_mut()?;
        let browser = device
            .as_any_mut()
            .downcast_mut::<BrowserDevice>()
            .ok_or_else(|| miette!("focused device is not Browser: {:?}", focused))?;
        browser.go_forward(page_id).await
    }

    pub async fn browser_reload(
        &mut self,
        page_id: &str,
    ) -> Result<crate::browser_device::BrowserActionResult> {
        let focused = self.focused;
        let device = self.focused_device_mut()?;
        let browser = device
            .as_any_mut()
            .downcast_mut::<BrowserDevice>()
            .ok_or_else(|| miette!("focused device is not Browser: {:?}", focused))?;
        browser.reload(page_id).await
    }

    pub async fn browser_close_page(
        &mut self,
        page_id: &str,
    ) -> Result<crate::browser_device::BrowserActionResult> {
        let focused = self.focused;
        let device = self.focused_device_mut()?;
        let browser = device
            .as_any_mut()
            .downcast_mut::<BrowserDevice>()
            .ok_or_else(|| miette!("focused device is not Browser: {:?}", focused))?;
        browser.close_page(page_id).await
    }

    pub fn terminal_session_state(
        &self,
        session_id: &str,
    ) -> Result<crate::terminal_device::TerminalSessionState> {
        let focused = self.focused.ok_or_else(|| miette!("no focused device"))?;
        let device = self
            .devices
            .get(&focused)
            .ok_or_else(|| miette!("focused device missing: {focused}"))?;
        let terminal = device
            .as_any()
            .downcast_ref::<TerminalDevice>()
            .ok_or_else(|| miette!("focused device is not Terminal: {:?}", focused))?;
        terminal.session_state(session_id)
    }

    pub async fn shutdown(mut self) -> Result<()> {
        for id in self.order {
            if let Some(device) = self.devices.get_mut(&id) {
                device.shutdown().await?;
            }
        }
        Ok(())
    }
}
