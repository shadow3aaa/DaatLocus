//! Independent App instances and execution-time capability boundaries.
use std::collections::HashSet;

use miette::{Result, miette};

use crate::{
    actor_app::ActorApp,
    actors::{ActorRecord, ActorSystem},
    app::{App, AppManager, AppToolExecutionContext},
    artifacts_app::ArtifactsApp,
    config::Config,
    files_app::FilesApp,
    model_catalog::catalog_model_capacity,
    openskills::load_openskills_with_policy,
    terminal_app::TerminalApp,
    web_app::WebApp,
};

pub const DEVICE_APPS: &[&str] = &[
    "apps_manager",
    "actors",
    "files",
    #[cfg(not(target_os = "android"))]
    "terminal",
    "web",
    #[cfg(any(windows, target_os = "macos", target_os = "android"))]
    "browser_use",
    "artifacts",
    #[cfg(any(all(windows, target_arch = "x86_64"), target_os = "macos"))]
    "computer_use",
    #[cfg(target_os = "android")]
    "phone_use",
];
pub(crate) const ALL_APPS: &[&str] = &[
    "actors",
    "files",
    "terminal",
    "web",
    "browser_use",
    "artifacts",
    "apps_manager",
    "computer_use",
    "phone_use",
];

pub fn validate_app_names(names: &[String]) -> Result<()> {
    let mut seen = HashSet::new();
    for name in names {
        if !ALL_APPS.contains(&name.as_str()) {
            return Err(miette!("unknown App `{name}`"));
        }
        if !seen.insert(name) {
            return Err(miette!("duplicated App `{name}`"));
        }
    }
    Ok(())
}

pub fn create_apps(
    actor: &ActorRecord,
    system: ActorSystem,
    config: &Config,
) -> Result<AppManager> {
    validate_app_names(&actor.config.apps)?;
    let model_key = actor.config.model.as_deref().unwrap_or(&config.main_model);
    let model = config
        .models
        .get(model_key)
        .ok_or_else(|| miette!("Actor model is not configured"))?;
    let supports_vision = model.supports_vision.unwrap_or_else(|| {
        catalog_model_capacity(&model.model_id).is_some_and(|capacity| capacity.supports_vision)
    });
    let state_dir = system.actor_state_dir(&actor.id);
    let mut apps: Vec<Box<dyn App>> = Vec::new();
    let installed = if actor.id == crate::stage::shared::ACTOR_ID {
        &["actors"][..]
    } else {
        DEVICE_APPS
    };
    for name in installed {
        let inner: Box<dyn App> = match *name {
            "actors" => Box::new(ActorApp::new(actor.id.clone(), system.clone())),
            "apps_manager" => Box::new(crate::mcp::AppsManager::new(
                system.app_registry(),
                actor.id.clone(),
            )),
            "files" => Box::new(FilesApp::new(state_dir.join("images"), supports_vision)),
            "terminal" if !cfg!(target_os = "android") => Box::new(TerminalApp::new()),
            "web" => Box::new(WebApp::new(
                actor.id.clone(),
                system.clone(),
                actor.remote_read_only,
            )),
            "browser_use" => Box::new(crate::browser_use_app::BrowserUseApp::new(
                actor.id.clone(),
                system.clone(),
                actor.remote_read_only,
                supports_vision,
            )),
            "artifacts" => Box::new(ArtifactsApp::new(actor.id.clone(), system.clone())),
            #[cfg(any(
                all(windows, target_arch = "x86_64"),
                target_os = "android",
                target_os = "macos"
            ))]
            "computer_use" | "phone_use" => Box::new(crate::computer_use_app::ComputerUseApp::new(
                actor.id.clone(),
                system.clone(),
                supports_vision,
            )),
            _ => return Err(miette!("unknown App `{name}`")),
        };
        apps.push(inner);
    }
    for entry in system
        .app_registry()
        .entries()
        .into_iter()
        .filter(|entry| entry.enabled && actor.id != crate::stage::shared::ACTOR_ID)
    {
        apps.push(Box::new(crate::mcp::McpApp::new(
            entry,
            system.app_registry(),
            system.clone(),
        )));
    }
    AppManager::new(apps)
}

pub fn prompt_resources(
    _actor: &ActorRecord,
    context: &AppToolExecutionContext,
) -> Result<Vec<String>> {
    Ok(
        load_openskills_with_policy(&context.execution_cwd, &context.sandbox_policy)
            .render_prompt_block()
            .into_iter()
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_and_duplicate_app_configs_fail() {
        assert!(validate_app_names(&["unknown".into()]).is_err());
        assert!(validate_app_names(&["files".into(), "files".into()]).is_err());
    }
}
