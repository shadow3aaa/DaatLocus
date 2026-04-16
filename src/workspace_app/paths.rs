use std::{
    env,
    path::{Path, PathBuf},
};

use miette::{Result, miette};

const WORKSPACE_APPS_DIR_NAME: &str = "apps";

pub fn resolve_runtime_workspace_dir() -> Result<PathBuf> {
    let home = env::home_dir()
        .ok_or_else(|| miette!("failed to determine home directory for workspace"))?;
    Ok(home.join("daat-locus-workspace"))
}

pub fn workspace_apps_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join(WORKSPACE_APPS_DIR_NAME)
}
