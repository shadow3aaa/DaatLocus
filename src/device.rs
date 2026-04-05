use std::{any::Any, collections::HashMap, fmt::Display, time::Duration};

use async_trait::async_trait;
use miette::{Result, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::terminal_device::TerminalDevice;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize, JsonSchema)]
pub enum DeviceId {
    Terminal,
}

impl Display for DeviceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Terminal => write!(f, "Terminal"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionLevel {
    Quiet,
    Notice,
}

impl Display for AttentionLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Quiet => write!(f, "Quiet"),
            Self::Notice => write!(f, "Notice"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceToolScope {
    Terminal,
}

#[derive(Debug, Clone)]
pub struct DeviceStateRender {
    pub title: String,
    pub lines: Vec<String>,
    pub attention: AttentionLevel,
    pub is_focused: bool,
}

#[async_trait]
pub trait Device: Send + Sync {
    fn id(&self) -> DeviceId;

    fn as_any_mut(&mut self) -> &mut dyn Any;

    fn render_state(&self, is_focused: bool) -> DeviceStateRender;

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
                self.devices.get(id).map(|device| {
                    let is_focused = self.focused == Some(*id);
                    (*id, device.render_state(is_focused))
                })
            })
            .collect()
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
        create_new_session: bool,
        workdir: Option<String>,
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
                create_new_session,
                workdir,
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

    pub async fn shutdown(mut self) -> Result<()> {
        for id in self.order {
            if let Some(device) = self.devices.get_mut(&id) {
                device.shutdown().await?;
            }
        }
        Ok(())
    }
}
