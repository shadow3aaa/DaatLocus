//! Owner observation of existing device-local outputs; no App effects or resources.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Ready,
    Disabled,
    Connecting,
    Unavailable,
    Unsupported,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Display {
    pub id: String,
    pub actor_id: Option<String>,
    pub name: Option<String>,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Catalog {
    pub supported: bool,
    pub state: State,
    pub displays: Vec<Display>,
}

pub(crate) fn catalog(system: &crate::actors::ActorSystem) -> miette::Result<Catalog> {
    #[cfg(windows)]
    return system.computer_workspace()?.preview_catalog();
    #[cfg(target_os = "android")]
    {
        let mut catalog = super::android::preview_catalog()?;
        for display in &mut catalog.displays {
            display.name = display
                .actor_id
                .as_deref()
                .and_then(|id| system.actor(id).ok())
                .map(|actor| actor.config.name);
        }
        Ok(catalog)
    }
    #[cfg(not(any(windows, target_os = "android")))]
    {
        let _ = system;
        Ok(Catalog {
            supported: false,
            state: State::Unsupported,
            displays: vec![],
        })
    }
}

pub(crate) fn frame(
    system: &crate::actors::ActorSystem,
    id: &str,
) -> miette::Result<Option<Vec<u8>>> {
    #[cfg(windows)]
    return system.computer_workspace()?.preview_frame(id);
    #[cfg(target_os = "android")]
    {
        let _ = system;
        super::android::preview_frame(id)
    }
    #[cfg(not(any(windows, target_os = "android")))]
    {
        let _ = (system, id);
        Ok(None)
    }
}
