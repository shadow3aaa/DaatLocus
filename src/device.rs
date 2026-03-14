use std::{collections::HashMap, fmt::Display, time::Duration};

use async_trait::async_trait;
use miette::{Result, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize, JsonSchema)]
pub enum DeviceId {
    Terminal,
    Telegram,
}

impl Display for DeviceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Terminal => write!(f, "Terminal"),
            Self::Telegram => write!(f, "Telegram"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionLevel {
    Quiet,
    Notice,
    Urgent,
}

impl Display for AttentionLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Quiet => write!(f, "Quiet"),
            Self::Notice => write!(f, "Notice"),
            Self::Urgent => write!(f, "Urgent"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PeripheralRender {
    pub title: String,
    pub summary: String,
    pub attention: AttentionLevel,
    pub is_focused: bool,
    pub interactive: bool,
}

#[derive(Debug, Clone)]
pub struct FocusedRender {
    pub title: String,
    pub content: String,
    pub interactive: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "type")]
pub enum DeviceAction {
    /// 将文本输入到终端并由 PTY 原样接收
    TerminalInput {
        text: String,
    },
    /// 打开 Telegram 的某个会话
    TelegramSelectChat {
        chat_id: String,
    },
    /// 向当前打开的 Telegram 会话发送一条消息
    TelegramSendMessage {
        text: String,
    },
}

#[async_trait]
pub trait Device: Send + Sync {
    fn id(&self) -> DeviceId;

    fn render_peripheral(&self, is_focused: bool) -> PeripheralRender;

    fn render_focused(&self) -> FocusedRender;

    fn requires_attention(&self) -> bool {
        false
    }

    async fn on_focus(&mut self) -> Result<()> {
        Ok(())
    }

    async fn on_blur(&mut self) -> Result<()> {
        Ok(())
    }

    async fn wait_until_settled(&self, _silence_duration: Duration, _timeout: Duration) -> bool {
        true
    }

    async fn execute(&mut self, action: DeviceAction) -> Result<()>;
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

    pub fn peripheral_renders(&self) -> Vec<(DeviceId, PeripheralRender)> {
        self.order
            .iter()
            .filter_map(|id| {
                self.devices.get(id).map(|device| {
                    let is_focused = self.focused == Some(*id);
                    (*id, device.render_peripheral(is_focused))
                })
            })
            .collect()
    }

    pub fn focused_render(&self) -> Option<FocusedRender> {
        self.focused
            .and_then(|id| self.devices.get(&id).map(|device| device.render_focused()))
    }

    pub fn requires_attention(&self) -> bool {
        self.devices.values().any(|device| device.requires_attention())
    }

    pub async fn focus(&mut self, id: DeviceId) -> Result<()> {
        if self.focused == Some(id) {
            return Ok(());
        }

        if !self.devices.contains_key(&id) {
            return Err(miette!("unknown device: {id}"));
        }

        if let Some(current) = self.focused {
            if let Some(device) = self.devices.get_mut(&current) {
                device.on_blur().await?;
            }
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

    pub async fn execute_focused(&mut self, action: DeviceAction) -> Result<()> {
        let Some(focused) = self.focused else {
            return Err(miette!("no focused device"));
        };
        let Some(device) = self.devices.get_mut(&focused) else {
            return Err(miette!("focused device missing: {focused}"));
        };
        device.execute(action).await
    }
}
