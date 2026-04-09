use std::{
    any::Any,
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
    time::Duration,
};

use async_trait::async_trait;
use miette::{Context as _, Result, miette};
use mlua::{Function, Lua, LuaOptions, LuaSerdeExt, StdLib, Table, Value as LuaValue};
use notify::{Event, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    app::{
        App, AppDynamicToolResult, AppDynamicToolSpec, AppHowToUse, AppId, AppInstallDisposition,
        AppManager, AppStateRender, AppUsage,
    },
    daat_locus_paths::daat_locus_paths_sync,
    skill::{SkillContent, SkillDoc, SkillSummary, load_skills_from_dir},
    workspace_paths::workspace_apps_dir,
};

pub struct WorkspaceAppBootstrap {
    pub apps: Vec<Box<dyn App>>,
    pub registry: WorkspaceAppRegistry,
    pub errors: Vec<String>,
}

#[derive(Debug, Default)]
pub struct WorkspaceAppRegistry {
    apps_root: PathBuf,
    state_root: PathBuf,
    records: BTreeMap<String, WorkspaceAppRecord>,
    dirty_apps: BTreeSet<String>,
    full_rescan_needed: bool,
}

#[derive(Debug, Default)]
struct WorkspaceAppRecord {
    app_id: Option<AppId>,
    loaded_digest: Option<String>,
    attempted_digest: Option<String>,
    last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceAppInvalidation {
    Dirty { folder_name: String },
    FullRescan,
}

#[derive(Debug, Default)]
pub struct WorkspaceAppSyncReport {
    pub added: Vec<AppId>,
    pub reloaded: Vec<AppId>,
    pub removed: Vec<AppId>,
    pub errors: Vec<String>,
}

impl WorkspaceAppSyncReport {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.reloaded.is_empty()
            && self.removed.is_empty()
            && self.errors.is_empty()
    }
}

pub enum WorkspaceAppWatcherHandle {
    Recommended(RecommendedWatcher),
    Poll(PollWatcher),
}

impl WorkspaceAppWatcherHandle {
    pub fn backend_name(&self) -> &'static str {
        match self {
            Self::Recommended(watcher) => {
                let _ = watcher;
                "recommended"
            }
            Self::Poll(watcher) => {
                let _ = watcher;
                "poll"
            }
        }
    }
}

#[derive(Debug)]
pub struct WorkspaceApp {
    id: AppId,
    app_dir: PathBuf,
    state_dir: PathBuf,
    state_file: PathBuf,
    entry_relative_path: String,
    entry_source: String,
    usage_markdown: String,
    how_to_use_markdown: String,
    skills: Vec<SkillDoc>,
    runtime_state: Mutex<WorkspaceAppRuntimeState>,
}

#[derive(Debug, Default, Deserialize)]
struct WorkspaceAppManifest {
    entry: Option<String>,
}

#[derive(Debug)]
struct WorkspaceAppRuntimeState {
    initialized: bool,
    state: JsonValue,
    notice_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct WorkspaceRenderOutput {
    title: Option<String>,
    #[serde(default)]
    lines: Vec<String>,
    state: Option<JsonValue>,
}

#[derive(Debug, Deserialize)]
struct WorkspaceToolDescriptor {
    name: String,
    description: String,
    input_schema: JsonValue,
    output_schema: Option<JsonValue>,
}

#[derive(Debug, Deserialize)]
struct WorkspaceToolCallOutput {
    summary: String,
    #[serde(default)]
    payload: JsonValue,
    model_content: Option<String>,
    #[serde(default)]
    ui_lines: Vec<String>,
    state: Option<JsonValue>,
    turn_boundary: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct WorkspaceNoticeOutput {
    #[serde(default)]
    notices: Vec<String>,
    state: Option<JsonValue>,
}

pub fn bootstrap_workspace_apps(workspace_root: &Path) -> WorkspaceAppBootstrap {
    let state_root = daat_locus_paths_sync().state_dir().join("apps");
    bootstrap_workspace_apps_with_state_root(workspace_root, &state_root)
}

fn bootstrap_workspace_apps_with_state_root(
    workspace_root: &Path,
    state_root: &Path,
) -> WorkspaceAppBootstrap {
    let apps_root = workspace_apps_dir(workspace_root);
    let registry = WorkspaceAppRegistry {
        apps_root: apps_root.clone(),
        state_root: state_root.to_path_buf(),
        ..WorkspaceAppRegistry::default()
    };
    let mut report = WorkspaceAppBootstrap {
        apps: Vec::new(),
        registry,
        errors: Vec::new(),
    };
    if !apps_root.exists() {
        return report;
    }

    let app_dirs = match discover_workspace_app_dirs(&apps_root) {
        Ok(app_dirs) => app_dirs,
        Err(err) => {
            report.errors.push(format!(
                "failed to discover workspace apps under {}: {err:?}",
                apps_root.display()
            ));
            return report;
        }
    };

    for app_dir in app_dirs {
        let folder_name = app_dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "<unknown>".to_string());
        let digest = match workspace_app_source_digest(&app_dir) {
            Ok(digest) => digest,
            Err(err) => {
                let record = report
                    .registry
                    .records
                    .entry(folder_name.clone())
                    .or_default();
                record.attempted_digest = None;
                record.last_error = Some(err.to_string());
                report.errors.push(format!(
                    "failed to hash workspace app `{folder_name}` from {}: {err:?}",
                    app_dir.display()
                ));
                continue;
            }
        };
        match WorkspaceApp::load_from_dir(&app_dir, state_root, &folder_name) {
            Ok(app) => {
                let app_id = app.id();
                report.registry.records.insert(
                    folder_name,
                    WorkspaceAppRecord {
                        app_id: Some(app_id),
                        loaded_digest: Some(digest.clone()),
                        attempted_digest: Some(digest),
                        last_error: None,
                    },
                );
                report.apps.push(Box::new(app));
            }
            Err(err) => {
                let record = report
                    .registry
                    .records
                    .entry(folder_name.clone())
                    .or_default();
                record.attempted_digest = Some(digest);
                record.last_error = Some(err.to_string());
                report.errors.push(format!(
                    "failed to load workspace app `{folder_name}` from {}: {err:?}",
                    app_dir.display()
                ));
            }
        }
    }

    report
}

impl WorkspaceAppRegistry {
    pub fn record_invalidation(&mut self, invalidation: WorkspaceAppInvalidation) {
        match invalidation {
            WorkspaceAppInvalidation::Dirty { folder_name } => {
                self.dirty_apps.insert(folder_name);
            }
            WorkspaceAppInvalidation::FullRescan => {
                self.full_rescan_needed = true;
            }
        }
    }

    pub async fn sync_dirty_apps(
        &mut self,
        apps: &mut AppManager,
    ) -> Result<WorkspaceAppSyncReport> {
        let mut report = WorkspaceAppSyncReport::default();
        let folders = if self.full_rescan_needed {
            let discovered = match discover_workspace_app_folder_names(&self.apps_root) {
                Ok(discovered) => discovered,
                Err(err) => {
                    report.errors.push(format!(
                        "failed to rescan workspace apps under {}: {err:?}",
                        self.apps_root.display()
                    ));
                    return Ok(report);
                }
            };
            self.full_rescan_needed = false;
            self.dirty_apps.clear();
            let mut all = discovered;
            all.extend(self.records.keys().cloned());
            all
        } else if self.dirty_apps.is_empty() {
            return Ok(report);
        } else {
            std::mem::take(&mut self.dirty_apps)
        };

        for folder_name in folders {
            self.sync_single_app(&folder_name, apps, &mut report)
                .await?;
        }

        Ok(report)
    }

    async fn sync_single_app(
        &mut self,
        folder_name: &str,
        apps: &mut AppManager,
        report: &mut WorkspaceAppSyncReport,
    ) -> Result<()> {
        let app_dir = self.apps_root.join(folder_name);
        if !app_dir.is_dir() {
            if let Some(record) = self.records.remove(folder_name)
                && let Some(app_id) = record.app_id
                && apps.remove(&app_id).await?
            {
                report.removed.push(app_id);
            }
            return Ok(());
        }

        let digest = match workspace_app_source_digest(&app_dir) {
            Ok(digest) => digest,
            Err(err) => {
                self.records
                    .entry(folder_name.to_string())
                    .or_default()
                    .last_error = Some(err.to_string());
                report.errors.push(format!(
                    "failed to hash workspace app `{folder_name}` from {}: {err:?}",
                    app_dir.display()
                ));
                return Ok(());
            }
        };

        if self
            .records
            .get(folder_name)
            .and_then(|record| record.attempted_digest.as_deref())
            == Some(digest.as_str())
        {
            return Ok(());
        }

        match WorkspaceApp::load_from_dir(&app_dir, &self.state_root, folder_name) {
            Ok(app) => {
                let app_id = app.id();
                let disposition = apps.install_or_replace(Box::new(app)).await?;
                let record = self.records.entry(folder_name.to_string()).or_default();
                record.app_id = Some(app_id.clone());
                record.loaded_digest = Some(digest.clone());
                record.attempted_digest = Some(digest);
                record.last_error = None;
                match disposition {
                    AppInstallDisposition::Added => report.added.push(app_id),
                    AppInstallDisposition::Replaced => report.reloaded.push(app_id),
                }
            }
            Err(err) => {
                let record = self.records.entry(folder_name.to_string()).or_default();
                record.attempted_digest = Some(digest);
                record.last_error = Some(err.to_string());
                report.errors.push(format!(
                    "failed to reload workspace app `{folder_name}` from {}: {err:?}",
                    app_dir.display()
                ));
            }
        }

        Ok(())
    }
}

pub fn start_workspace_app_watcher(
    apps_root: PathBuf,
    tx: UnboundedSender<WorkspaceAppInvalidation>,
) -> Result<WorkspaceAppWatcherHandle> {
    let recommended_callback = build_watcher_callback(apps_root.clone(), tx.clone());
    match notify::recommended_watcher(recommended_callback) {
        Ok(mut watcher) => {
            watcher
                .watch(&apps_root, RecursiveMode::Recursive)
                .map_err(|err| {
                    miette!(
                        "failed to watch workspace app directory {}: {err}",
                        apps_root.display()
                    )
                })?;
            Ok(WorkspaceAppWatcherHandle::Recommended(watcher))
        }
        Err(recommended_err) => {
            tracing::warn!(
                "failed to start native workspace app watcher for {}: {recommended_err}; falling back to poll watcher",
                apps_root.display()
            );
            let poll_callback = build_watcher_callback(apps_root.clone(), tx);
            let mut watcher = PollWatcher::new(
                poll_callback,
                notify::Config::default().with_poll_interval(Duration::from_millis(500)),
            )
            .map_err(|err| {
                miette!(
                    "failed to start poll workspace app watcher for {}: {err}",
                    apps_root.display()
                )
            })?;
            watcher
                .watch(&apps_root, RecursiveMode::Recursive)
                .map_err(|err| {
                    miette!(
                        "failed to watch workspace app directory {} with poll watcher: {err}",
                        apps_root.display()
                    )
                })?;
            Ok(WorkspaceAppWatcherHandle::Poll(watcher))
        }
    }
}

fn build_watcher_callback(
    apps_root: PathBuf,
    tx: UnboundedSender<WorkspaceAppInvalidation>,
) -> impl FnMut(notify::Result<Event>) + Send + 'static {
    move |event_result| match event_result {
        Ok(event) => {
            if event.kind.is_access() {
                return;
            }
            match invalidations_for_event(&apps_root, &event) {
                Ok(invalidations) => {
                    for invalidation in invalidations {
                        let _ = tx.send(invalidation);
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        "failed to map workspace app watcher event for {}: {err:?}",
                        apps_root.display()
                    );
                    let _ = tx.send(WorkspaceAppInvalidation::FullRescan);
                }
            }
        }
        Err(err) => {
            tracing::warn!(
                "workspace app watcher error for {}: {err:?}",
                apps_root.display()
            );
            let _ = tx.send(WorkspaceAppInvalidation::FullRescan);
        }
    }
}

fn invalidations_for_event(
    apps_root: &Path,
    event: &Event,
) -> Result<Vec<WorkspaceAppInvalidation>> {
    if event.paths.is_empty() {
        return Ok(vec![WorkspaceAppInvalidation::FullRescan]);
    }
    let mut dirty = BTreeSet::new();
    for path in &event.paths {
        let Some(folder_name) = app_folder_name_from_path(apps_root, path) else {
            return Ok(vec![WorkspaceAppInvalidation::FullRescan]);
        };
        dirty.insert(folder_name);
    }
    Ok(dirty
        .into_iter()
        .map(|folder_name| WorkspaceAppInvalidation::Dirty { folder_name })
        .collect())
}

fn app_folder_name_from_path(apps_root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(apps_root).ok()?;
    let component = relative.components().next()?;
    match component {
        Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
        _ => None,
    }
}

fn discover_workspace_app_dirs(apps_root: &Path) -> Result<Vec<PathBuf>> {
    let folder_names = discover_workspace_app_folder_names(apps_root)?;
    Ok(folder_names
        .into_iter()
        .map(|folder_name| apps_root.join(folder_name))
        .collect())
}

fn discover_workspace_app_folder_names(apps_root: &Path) -> Result<BTreeSet<String>> {
    if !apps_root.exists() {
        return Ok(BTreeSet::new());
    }
    let entries = fs::read_dir(apps_root).map_err(|err| {
        miette!(
            "failed to read workspace app directory {}: {err}",
            apps_root.display()
        )
    })?;
    let mut folder_names = BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(|err| {
            miette!(
                "failed to read workspace app entry under {}: {err}",
                apps_root.display()
            )
        })?;
        let file_type = entry.file_type().map_err(|err| {
            miette!(
                "failed to inspect workspace app entry {}: {err}",
                entry.path().display()
            )
        })?;
        if file_type.is_dir() {
            folder_names.insert(entry.file_name().to_string_lossy().into_owned());
        }
    }
    Ok(folder_names)
}

fn workspace_app_source_digest(app_dir: &Path) -> Result<String> {
    let mut files = Vec::new();
    collect_digest_file(&mut files, app_dir, app_dir.join("app.toml"))?;
    collect_digest_files_under(&mut files, app_dir, &app_dir.join("runtime"), "lua")?;
    collect_digest_files_under(&mut files, app_dir, &app_dir.join("prompt"), "md")?;
    collect_digest_files_under(&mut files, app_dir, &app_dir.join("skills"), "md")?;
    files.sort_by(|left, right| left.0.cmp(&right.0));

    let mut hasher = Sha256::new();
    for (relative, content) in files {
        hasher.update(relative.as_bytes());
        hasher.update([0]);
        hasher.update(content.len().to_le_bytes());
        hasher.update([0]);
        hasher.update(content);
        hasher.update([0xff]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn collect_digest_files_under(
    files: &mut Vec<(String, Vec<u8>)>,
    app_dir: &Path,
    dir: &Path,
    extension: &str,
) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in
        fs::read_dir(dir).map_err(|err| miette!("failed to read {}: {err}", dir.display()))?
    {
        let entry =
            entry.map_err(|err| miette!("failed to read entry under {}: {err}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|err| miette!("failed to inspect {}: {err}", path.display()))?;
        if file_type.is_dir() {
            collect_digest_files_under(files, app_dir, &path, extension)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some(extension) {
            continue;
        }
        collect_digest_file(files, app_dir, path)?;
    }
    Ok(())
}

fn collect_digest_file(
    files: &mut Vec<(String, Vec<u8>)>,
    app_dir: &Path,
    path: PathBuf,
) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let relative = path
        .strip_prefix(app_dir)
        .map_err(|err| {
            miette!(
                "failed to compute relative path for {} inside {}: {err}",
                path.display(),
                app_dir.display()
            )
        })?
        .to_string_lossy()
        .into_owned();
    let content =
        fs::read(&path).map_err(|err| miette!("failed to read {}: {err}", path.display()))?;
    files.push((relative, content));
    Ok(())
}

fn new_workspace_app_lua_runtime(app_dir: &Path, app_id: Option<&AppId>) -> Result<Lua> {
    let lua = Lua::new_with(WorkspaceApp::app_stdlib(), LuaOptions::default()).map_err(|err| {
        if let Some(app_id) = app_id {
            miette!("failed to create lua runtime for app `{app_id}`: {err}")
        } else {
            miette!("failed to create workspace app lua runtime: {err}")
        }
    })?;
    configure_workspace_app_lua_runtime(&lua, app_dir)
        .wrap_err("failed to configure workspace app lua runtime")?;
    Ok(lua)
}

fn configure_workspace_app_lua_runtime(lua: &Lua, app_dir: &Path) -> Result<()> {
    let globals = lua.globals();
    let package: Table = globals
        .get("package")
        .map_err(|err| miette!("failed to access Lua package table: {err}"))?;
    let package_path = workspace_app_package_path(app_dir);
    package
        .set("path", package_path)
        .map_err(|err| miette!("failed to set package.path: {err}"))?;
    package
        .set("cpath", "")
        .map_err(|err| miette!("failed to disable package.cpath: {err}"))?;
    Ok(())
}

fn workspace_app_package_path(app_dir: &Path) -> String {
    let app_dir = normalize_lua_path(app_dir);
    [
        format!("{app_dir}/?.lua"),
        format!("{app_dir}/?/init.lua"),
        format!("{app_dir}/runtime/?.lua"),
        format!("{app_dir}/runtime/?/init.lua"),
    ]
    .join(";")
}

fn normalize_lua_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn validate_workspace_tool_schema(schema: &JsonValue, label: &str) -> Result<()> {
    validate_schema_definition(schema, label)
}

fn validate_workspace_tool_value(value: &JsonValue, schema: &JsonValue, label: &str) -> Result<()> {
    validate_value_against_schema(value, schema, label)
}

fn validate_schema_definition(schema: &JsonValue, label: &str) -> Result<()> {
    let object = schema
        .as_object()
        .ok_or_else(|| miette!("{label} must be a JSON object schema"))?;

    if let Some(type_value) = object.get("type") {
        let type_name = type_value
            .as_str()
            .ok_or_else(|| miette!("{label}.type must be a string"))?;
        match type_name {
            "object" | "array" | "string" | "integer" | "number" | "boolean" | "null" => {}
            other => return Err(miette!("{label}.type uses unsupported type `{other}`")),
        }
    }

    if let Some(required) = object.get("required") {
        let required = required
            .as_array()
            .ok_or_else(|| miette!("{label}.required must be an array"))?;
        for (index, item) in required.iter().enumerate() {
            if item.as_str().is_none() {
                return Err(miette!("{label}.required[{index}] must be a string"));
            }
        }
    }

    if let Some(properties) = object.get("properties") {
        let properties = properties
            .as_object()
            .ok_or_else(|| miette!("{label}.properties must be an object"))?;
        for (key, property_schema) in properties {
            validate_schema_definition(property_schema, &format!("{label}.properties.{key}"))?;
        }
    }

    if let Some(items) = object.get("items") {
        validate_schema_definition(items, &format!("{label}.items"))?;
    }

    if let Some(additional) = object.get("additionalProperties")
        && !additional.is_boolean()
    {
        validate_schema_definition(additional, &format!("{label}.additionalProperties"))?;
    }

    if let Some(enum_values) = object.get("enum")
        && !enum_values.is_array()
    {
        return Err(miette!("{label}.enum must be an array"));
    }

    for key in ["minLength", "maxLength", "minItems", "maxItems"] {
        if let Some(value) = object.get(key)
            && value.as_u64().is_none()
        {
            return Err(miette!("{label}.{key} must be a non-negative integer"));
        }
    }

    for key in ["minimum", "maximum"] {
        if let Some(value) = object.get(key)
            && value.as_f64().is_none()
        {
            return Err(miette!("{label}.{key} must be a number"));
        }
    }

    Ok(())
}

fn normalize_workspace_input_schema(mut schema: JsonValue) -> JsonValue {
    if let Some(object) = schema.as_object_mut()
        && object.get("type").and_then(|value| value.as_str()) == Some("object")
    {
        object
            .entry("properties".to_string())
            .or_insert_with(|| serde_json::json!({}));
        object
            .entry("additionalProperties".to_string())
            .or_insert_with(|| serde_json::json!(false));
    }
    schema
}

fn validate_value_against_schema(value: &JsonValue, schema: &JsonValue, label: &str) -> Result<()> {
    let object = schema
        .as_object()
        .ok_or_else(|| miette!("{label} schema must be a JSON object"))?;

    if let Some(enum_values) = object.get("enum") {
        let enum_values = enum_values
            .as_array()
            .ok_or_else(|| miette!("{label}.enum must be an array"))?;
        if !enum_values.iter().any(|candidate| candidate == value) {
            return Err(miette!("{label} must match one of the allowed enum values"));
        }
    }

    let type_name = object.get("type").and_then(|value| value.as_str());
    match type_name {
        Some("object") => validate_object_value(value, object, label)?,
        Some("array") => validate_array_value(value, object, label)?,
        Some("string") => validate_string_value(value, object, label)?,
        Some("integer") => validate_integer_value(value, object, label)?,
        Some("number") => validate_number_value(value, object, label)?,
        Some("boolean") => {
            if !value.is_boolean() {
                return Err(miette!("{label} must be a boolean"));
            }
        }
        Some("null") => {
            if !value.is_null() {
                return Err(miette!("{label} must be null"));
            }
        }
        Some(other) => return Err(miette!("{label} schema uses unsupported type `{other}`")),
        None => {}
    }

    Ok(())
}

fn validate_object_value(
    value: &JsonValue,
    schema: &serde_json::Map<String, JsonValue>,
    label: &str,
) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| miette!("{label} must be an object"))?;
    let properties = schema
        .get("properties")
        .and_then(|value| value.as_object())
        .cloned()
        .unwrap_or_default();

    if let Some(required) = schema.get("required").and_then(|value| value.as_array()) {
        for field in required {
            let field = field
                .as_str()
                .ok_or_else(|| miette!("{label}.required entries must be strings"))?;
            if !object.contains_key(field) {
                return Err(miette!("{label}.{field} is required"));
            }
        }
    }

    for (key, field_value) in object {
        if let Some(field_schema) = properties.get(key) {
            validate_value_against_schema(field_value, field_schema, &format!("{label}.{key}"))?;
            continue;
        }

        match schema.get("additionalProperties") {
            Some(JsonValue::Bool(true)) | None => {}
            Some(JsonValue::Bool(false)) => {
                return Err(miette!("{label}.{key} is not allowed"));
            }
            Some(additional_schema) => {
                validate_value_against_schema(
                    field_value,
                    additional_schema,
                    &format!("{label}.{key}"),
                )?;
            }
        }
    }

    Ok(())
}

fn validate_array_value(
    value: &JsonValue,
    schema: &serde_json::Map<String, JsonValue>,
    label: &str,
) -> Result<()> {
    let items = value
        .as_array()
        .ok_or_else(|| miette!("{label} must be an array"))?;
    if let Some(min_items) = schema.get("minItems").and_then(|value| value.as_u64())
        && items.len() < min_items as usize
    {
        return Err(miette!("{label} must contain at least {min_items} item(s)"));
    }
    if let Some(max_items) = schema.get("maxItems").and_then(|value| value.as_u64())
        && items.len() > max_items as usize
    {
        return Err(miette!("{label} must contain at most {max_items} item(s)"));
    }
    if let Some(item_schema) = schema.get("items") {
        for (index, item) in items.iter().enumerate() {
            validate_value_against_schema(item, item_schema, &format!("{label}[{index}]"))?;
        }
    }
    Ok(())
}

fn validate_string_value(
    value: &JsonValue,
    schema: &serde_json::Map<String, JsonValue>,
    label: &str,
) -> Result<()> {
    let string = value
        .as_str()
        .ok_or_else(|| miette!("{label} must be a string"))?;
    let char_len = string.chars().count();
    if let Some(min_length) = schema.get("minLength").and_then(|value| value.as_u64())
        && char_len < min_length as usize
    {
        return Err(miette!("{label} must be at least {min_length} characters"));
    }
    if let Some(max_length) = schema.get("maxLength").and_then(|value| value.as_u64())
        && char_len > max_length as usize
    {
        return Err(miette!("{label} must be at most {max_length} characters"));
    }
    Ok(())
}

fn validate_integer_value(
    value: &JsonValue,
    schema: &serde_json::Map<String, JsonValue>,
    label: &str,
) -> Result<()> {
    let number = value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .ok_or_else(|| miette!("{label} must be an integer"))?;
    validate_numeric_bounds(number as f64, schema, label)
}

fn validate_number_value(
    value: &JsonValue,
    schema: &serde_json::Map<String, JsonValue>,
    label: &str,
) -> Result<()> {
    let number = value
        .as_f64()
        .ok_or_else(|| miette!("{label} must be a number"))?;
    validate_numeric_bounds(number, schema, label)
}

fn validate_numeric_bounds(
    value: f64,
    schema: &serde_json::Map<String, JsonValue>,
    label: &str,
) -> Result<()> {
    if let Some(minimum) = schema.get("minimum").and_then(|value| value.as_f64())
        && value < minimum
    {
        return Err(miette!("{label} must be >= {minimum}"));
    }
    if let Some(maximum) = schema.get("maximum").and_then(|value| value.as_f64())
        && value > maximum
    {
        return Err(miette!("{label} must be <= {maximum}"));
    }
    Ok(())
}

impl WorkspaceApp {
    fn load_from_dir(app_dir: &Path, state_root: &Path, folder_name: &str) -> Result<Self> {
        let id = AppId::from_workspace_folder(folder_name.to_string())?;
        let manifest = load_manifest(app_dir)?;
        let entry_relative_path = manifest
            .entry
            .unwrap_or_else(|| AppId::DEFAULT_WORKSPACE_ENTRY.to_string());
        let entry_path = resolve_relative_child_path(app_dir, &entry_relative_path)
            .wrap_err("invalid app entry path")?;
        let entry_source = fs::read_to_string(&entry_path)
            .map_err(|err| miette!("failed to read lua entry {}: {err}", entry_path.display()))?;
        validate_lua_entry(&id, app_dir, &entry_relative_path, &entry_source)?;

        let usage_markdown = fs::read_to_string(app_dir.join("prompt").join("usage.md"))
            .map_err(|err| miette!("failed to read prompt/usage.md for app `{id}`: {err}"))?;
        let how_to_use_markdown = fs::read_to_string(app_dir.join("prompt").join("how_to_use.md"))
            .map_err(|err| miette!("failed to read prompt/how_to_use.md for app `{id}`: {err}"))?;
        let skills = load_skills_from_dir(&app_dir.join("skills"))
            .wrap_err_with(|| format!("failed to load skills for app `{id}`"))?;
        let state_dir = state_root.join(id.as_str());
        let state_file = state_dir.join("state.json");
        let runtime_state = load_runtime_state(&state_file)
            .wrap_err_with(|| format!("failed to load state for app `{id}`"))?;

        Ok(Self {
            id,
            app_dir: app_dir.to_path_buf(),
            state_dir,
            state_file,
            entry_relative_path,
            entry_source,
            usage_markdown,
            how_to_use_markdown,
            skills,
            runtime_state: Mutex::new(runtime_state),
        })
    }

    fn app_stdlib() -> StdLib {
        StdLib::ALL_SAFE
    }

    fn map_lua<T>(&self, action: &str, result: mlua::Result<T>) -> Result<T> {
        result.map_err(|err| miette!("lua app `{}` {action}: {err}", self.id))
    }

    fn lua_context(&self, lua: &Lua) -> Result<Table> {
        let ctx = self.map_lua("failed to create context table", lua.create_table())?;
        self.map_lua(
            "failed to set `app_id` in context",
            ctx.set("app_id", self.id.to_string()),
        )?;
        self.map_lua(
            "failed to set `app_dir` in context",
            ctx.set("app_dir", self.app_dir.display().to_string()),
        )?;
        self.map_lua(
            "failed to set `state_dir` in context",
            ctx.set("state_dir", self.state_dir.display().to_string()),
        )?;
        Ok(ctx)
    }

    fn with_loaded_module<T>(
        &self,
        callback: impl FnOnce(&Lua, Table, Table, &mut WorkspaceAppRuntimeState) -> Result<T>,
    ) -> Result<T> {
        let lua = new_workspace_app_lua_runtime(&self.app_dir, Some(&self.id))
            .map_err(|err| miette!("failed to create lua runtime for app `{}`: {err}", self.id))?;
        let module: Table = lua
            .load(&self.entry_source)
            .set_name(format!("{}:{}", self.id, self.entry_relative_path))
            .eval()
            .map_err(|err| {
                miette!(
                    "failed to evaluate lua app `{}` from {}: {err}",
                    self.id,
                    self.entry_relative_path
                )
            })?;
        let ctx = self.lua_context(&lua)?;
        let mut runtime = self.runtime_state.lock();
        self.ensure_initialized(&lua, &module, &ctx, &mut runtime)?;
        callback(&lua, module, ctx, &mut runtime)
    }

    fn ensure_initialized(
        &self,
        lua: &Lua,
        module: &Table,
        ctx: &Table,
        runtime: &mut WorkspaceAppRuntimeState,
    ) -> Result<()> {
        if runtime.initialized {
            return Ok(());
        }
        fs::create_dir_all(&self.state_dir).map_err(|err| {
            miette!(
                "failed to create app state directory {} before init: {err}",
                self.state_dir.display()
            )
        })?;
        let init_fn = self.map_lua(
            "failed to resolve `init`",
            module.get::<Option<Function>>("init"),
        )?;
        runtime.state = if let Some(init_fn) = init_fn {
            let result = self.map_lua(
                "failed to execute `init`",
                init_fn.call::<LuaValue>(ctx.clone()),
            )?;
            match result {
                LuaValue::Nil => JsonValue::Object(Default::default()),
                value => self.map_lua("failed to decode `init` result", lua.from_value(value))?,
            }
        } else {
            JsonValue::Object(Default::default())
        };
        runtime.initialized = true;
        self.persist_runtime_state(runtime)?;
        Ok(())
    }

    fn summarize_notices(notices: &[String]) -> Option<String> {
        let mut notices = notices
            .iter()
            .map(|notice| notice.trim())
            .filter(|notice| !notice.is_empty())
            .collect::<Vec<_>>();
        if notices.is_empty() {
            return None;
        }
        if notices.len() == 1 {
            return Some(notices.remove(0).to_string());
        }
        let preview = notices
            .iter()
            .take(3)
            .copied()
            .collect::<Vec<_>>()
            .join("; ");
        if notices.len() <= 3 {
            Some(format!("{} notices pending: {}", notices.len(), preview))
        } else {
            Some(format!(
                "{} notices pending: {}; +{} more",
                notices.len(),
                preview,
                notices.len() - 3
            ))
        }
    }

    fn load_tool_descriptors(
        &self,
        lua: &Lua,
        module: &Table,
        ctx: &Table,
        runtime: &WorkspaceAppRuntimeState,
    ) -> Result<Vec<WorkspaceToolDescriptor>> {
        let Some(list_tools_fn) = self.map_lua(
            "failed to resolve `list_tools`",
            module.get::<Option<Function>>("list_tools"),
        )?
        else {
            return Ok(Vec::new());
        };
        let state_value = self.map_lua(
            "failed to encode runtime state for `list_tools`",
            lua.to_value(&runtime.state),
        )?;
        let value = self.map_lua(
            "failed to execute `list_tools`",
            list_tools_fn.call::<LuaValue>((ctx.clone(), state_value)),
        )?;
        let mut descriptors = match value {
            LuaValue::Nil => Vec::new(),
            value => self.map_lua(
                "failed to decode `list_tools` result",
                lua.from_value::<Vec<WorkspaceToolDescriptor>>(value),
            )?,
        };
        for descriptor in &mut descriptors {
            descriptor.input_schema =
                normalize_workspace_input_schema(descriptor.input_schema.clone());
            validate_workspace_tool_schema(
                &descriptor.input_schema,
                &format!("workspace app tool `{}` input_schema", descriptor.name),
            )?;
            if let Some(output_schema) = descriptor.output_schema.as_ref() {
                validate_workspace_tool_schema(
                    output_schema,
                    &format!("workspace app tool `{}` output_schema", descriptor.name),
                )?;
            }
        }
        Ok(descriptors)
    }

    fn persist_runtime_state(&self, runtime: &WorkspaceAppRuntimeState) -> Result<()> {
        if let Some(parent) = self.state_file.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                miette!(
                    "failed to create app state directory {}: {err}",
                    parent.display()
                )
            })?;
        }
        let content = serde_json::to_vec_pretty(&runtime.state)
            .map_err(|err| miette!("failed to encode app state for `{}`: {err}", self.id))?;
        fs::write(&self.state_file, content).map_err(|err| {
            miette!(
                "failed to write app state {}: {err}",
                self.state_file.display()
            )
        })?;
        Ok(())
    }

    fn default_render_state(&self) -> AppStateRender {
        let mut lines = vec![
            "kind=workspace_app".to_string(),
            format!("entry={}", self.entry_relative_path),
            format!("source_dir={}", self.app_dir.display()),
        ];
        if self.skills.is_empty() {
            lines.push("skills=none".to_string());
        } else {
            lines.push(format!(
                "skills={}",
                self.skills
                    .iter()
                    .map(|skill| skill.id.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
        AppStateRender {
            title: self.id.to_string(),
            lines,
        }
    }

    fn error_render_state(&self, error: &miette::Report) -> AppStateRender {
        let mut render = self.default_render_state();
        render.lines.push(format!(
            "last_error={}",
            error.to_string().replace('\n', " | ")
        ));
        render
    }
}

#[async_trait]
impl App for WorkspaceApp {
    fn id(&self) -> AppId {
        self.id.clone()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn render_state(&self) -> AppStateRender {
        match self.with_loaded_module(|lua, module, ctx, runtime| {
            let Some(render_fn) = self.map_lua(
                "failed to resolve `render_state`",
                module.get::<Option<Function>>("render_state"),
            )?
            else {
                return Ok(self.default_render_state());
            };
            let state_value = self.map_lua(
                "failed to encode runtime state for `render_state`",
                lua.to_value(&runtime.state),
            )?;
            let result = self.map_lua(
                "failed to execute `render_state`",
                render_fn.call::<LuaValue>((ctx, state_value)),
            )?;
            let output = match result {
                LuaValue::Nil => WorkspaceRenderOutput::default(),
                value => self.map_lua(
                    "failed to decode `render_state` result",
                    lua.from_value::<WorkspaceRenderOutput>(value),
                )?,
            };
            if let Some(next_state) = output.state {
                runtime.state = next_state;
                self.persist_runtime_state(runtime)?;
            }
            let mut lines = output.lines;
            if !lines.iter().any(|line| line.starts_with("kind=")) {
                lines.insert(0, "kind=workspace_app".to_string());
            }
            Ok(AppStateRender {
                title: output.title.unwrap_or_else(|| self.id.to_string()),
                lines,
            })
        }) {
            Ok(render) => render,
            Err(err) => self.error_render_state(&err),
        }
    }

    fn usage(&self) -> AppUsage {
        AppUsage {
            description: markdown_summary_line(&self.usage_markdown),
            when_to_focus: Vec::new(),
            body_markdown: Some(self.usage_markdown.clone()),
        }
    }

    fn how_to_use(&self) -> AppHowToUse {
        AppHowToUse {
            lines: Vec::new(),
            body_markdown: Some(self.how_to_use_markdown.clone()),
        }
    }

    fn skills(&self) -> Vec<SkillSummary> {
        self.skills.iter().map(SkillDoc::summary).collect()
    }

    fn read_skill(&self, id: &str) -> Result<SkillContent> {
        let skill = self
            .skills
            .iter()
            .find(|skill| skill.id == id)
            .ok_or_else(|| miette!("unknown workspace app skill `{id}`"))?;
        Ok(skill.content())
    }

    fn dynamic_tools(&self) -> Result<Vec<AppDynamicToolSpec>> {
        self.with_loaded_module(|lua, module, ctx, runtime| {
            let descriptors = self.load_tool_descriptors(lua, &module, &ctx, runtime)?;
            Ok(descriptors
                .into_iter()
                .map(|descriptor| AppDynamicToolSpec {
                    name: descriptor.name,
                    description: descriptor.description,
                    input_schema: descriptor.input_schema,
                })
                .collect())
        })
    }

    async fn execute_dynamic_tool(
        &mut self,
        name: &str,
        arguments: JsonValue,
    ) -> Result<AppDynamicToolResult> {
        self.with_loaded_module(|lua, module, ctx, runtime| {
            let descriptors = self.load_tool_descriptors(lua, &module, &ctx, runtime)?;
            let descriptor = descriptors
                .into_iter()
                .find(|descriptor| descriptor.name == name)
                .ok_or_else(|| {
                    miette!("workspace app `{}` does not declare tool `{name}`", self.id)
                })?;
            validate_workspace_tool_value(
                &arguments,
                &descriptor.input_schema,
                &format!("arguments for workspace app tool `{}`", descriptor.name),
            )?;
            let call_tool_fn = self
                .map_lua(
                    "failed to resolve `call_tool`",
                    module.get::<Option<Function>>("call_tool"),
                )?
                .ok_or_else(|| {
                    miette!("workspace app `{}` does not define `call_tool`", self.id)
                })?;
            let state_value = self.map_lua(
                "failed to encode runtime state for `call_tool`",
                lua.to_value(&runtime.state),
            )?;
            let args_value = self.map_lua(
                "failed to encode tool arguments for `call_tool`",
                lua.to_value(&arguments),
            )?;
            let value = self.map_lua(
                "failed to execute `call_tool`",
                call_tool_fn.call::<LuaValue>((ctx, state_value, name.to_string(), args_value)),
            )?;
            let output = self.map_lua(
                "failed to decode `call_tool` result",
                lua.from_value::<WorkspaceToolCallOutput>(value),
            )?;
            if output.summary.trim().is_empty() {
                return Err(miette!(
                    "workspace app `{}` tool `{}` returned an empty summary",
                    self.id,
                    descriptor.name
                ));
            }
            if let Some(output_schema) = descriptor.output_schema.as_ref() {
                validate_workspace_tool_value(
                    &output.payload,
                    output_schema,
                    &format!("payload for workspace app tool `{}`", descriptor.name),
                )?;
            }
            if let Some(next_state) = output.state {
                runtime.state = next_state;
                self.persist_runtime_state(runtime)?;
            }
            Ok(AppDynamicToolResult {
                summary: output.summary,
                payload: output.payload,
                model_content: output.model_content,
                ui_lines: output.ui_lines,
                turn_boundary_reason: output.turn_boundary,
            })
        })
    }

    async fn on_focus(&mut self) -> Result<()> {
        self.with_loaded_module(|lua, module, ctx, runtime| {
            let Some(on_focus_fn) = self.map_lua(
                "failed to resolve `on_focus`",
                module.get::<Option<Function>>("on_focus"),
            )?
            else {
                return Ok(());
            };
            let state_value = self.map_lua(
                "failed to encode runtime state for `on_focus`",
                lua.to_value(&runtime.state),
            )?;
            let value = self.map_lua(
                "failed to execute `on_focus`",
                on_focus_fn.call::<LuaValue>((ctx, state_value)),
            )?;
            if !matches!(value, LuaValue::Nil) {
                runtime.state =
                    self.map_lua("failed to decode `on_focus` result", lua.from_value(value))?;
                self.persist_runtime_state(runtime)?;
            }
            Ok(())
        })
    }

    async fn on_blur(&mut self) -> Result<()> {
        self.with_loaded_module(|lua, module, ctx, runtime| {
            let Some(on_blur_fn) = self.map_lua(
                "failed to resolve `on_blur`",
                module.get::<Option<Function>>("on_blur"),
            )?
            else {
                return Ok(());
            };
            let state_value = self.map_lua(
                "failed to encode runtime state for `on_blur`",
                lua.to_value(&runtime.state),
            )?;
            let value = self.map_lua(
                "failed to execute `on_blur`",
                on_blur_fn.call::<LuaValue>((ctx, state_value)),
            )?;
            if !matches!(value, LuaValue::Nil) {
                runtime.state =
                    self.map_lua("failed to decode `on_blur` result", lua.from_value(value))?;
                self.persist_runtime_state(runtime)?;
            }
            Ok(())
        })
    }

    async fn refresh_notice(&mut self) -> Result<()> {
        self.with_loaded_module(|lua, module, ctx, runtime| {
            let Some(poll_notices_fn) = self.map_lua(
                "failed to resolve `poll_notices`",
                module.get::<Option<Function>>("poll_notices"),
            )?
            else {
                runtime.notice_reason = None;
                return Ok(());
            };
            let state_value = self.map_lua(
                "failed to encode runtime state for `poll_notices`",
                lua.to_value(&runtime.state),
            )?;
            let value = self.map_lua(
                "failed to execute `poll_notices`",
                poll_notices_fn.call::<LuaValue>((ctx, state_value)),
            )?;
            let output = match value {
                LuaValue::Nil => WorkspaceNoticeOutput::default(),
                value => self.map_lua(
                    "failed to decode `poll_notices` result",
                    lua.from_value::<WorkspaceNoticeOutput>(value),
                )?,
            };
            if let Some(next_state) = output.state {
                runtime.state = next_state;
                self.persist_runtime_state(runtime)?;
            }
            runtime.notice_reason = Self::summarize_notices(&output.notices);
            Ok(())
        })
    }

    fn notice_reason(&self) -> Option<String> {
        self.runtime_state.lock().notice_reason.clone()
    }
}

fn load_manifest(app_dir: &Path) -> Result<WorkspaceAppManifest> {
    let manifest_path = app_dir.join("app.toml");
    if !manifest_path.exists() {
        return Ok(WorkspaceAppManifest::default());
    }
    let content = fs::read_to_string(&manifest_path).map_err(|err| {
        miette!(
            "failed to read app.toml at {}: {err}",
            manifest_path.display()
        )
    })?;
    toml::from_str::<WorkspaceAppManifest>(&content).map_err(|err| {
        miette!(
            "failed to parse app.toml at {}: {err}",
            manifest_path.display()
        )
    })
}

fn resolve_relative_child_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute() {
        return Err(miette!("path `{relative}` must be relative"));
    }
    if relative_path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(miette!(
            "path `{relative}` must stay inside the app directory"
        ));
    }
    Ok(root.join(relative_path))
}

fn validate_lua_entry(
    app_id: &AppId,
    app_dir: &Path,
    entry_relative_path: &str,
    source: &str,
) -> Result<()> {
    let lua = new_workspace_app_lua_runtime(&app_dir, Some(app_id))
        .map_err(|err| miette!("failed to create lua validator for app `{app_id}`: {err}"))?;
    lua.load(source)
        .set_name(format!("{app_id}:{entry_relative_path}"))
        .eval::<Table>()
        .map_err(|err| {
            miette!("lua entry validation failed for app `{app_id}` ({entry_relative_path}): {err}")
        })?;
    Ok(())
}

fn load_runtime_state(path: &Path) -> Result<WorkspaceAppRuntimeState> {
    if !path.exists() {
        return Ok(WorkspaceAppRuntimeState {
            initialized: false,
            state: JsonValue::Null,
            notice_reason: None,
        });
    }
    let content = fs::read_to_string(path)
        .map_err(|err| miette!("failed to read app state {}: {err}", path.display()))?;
    let state = serde_json::from_str::<JsonValue>(&content)
        .map_err(|err| miette!("failed to parse app state {}: {err}", path.display()))?;
    Ok(WorkspaceAppRuntimeState {
        initialized: true,
        state,
        notice_reason: None,
    })
}

fn markdown_summary_line(content: &str) -> String {
    content
        .lines()
        .map(str::trim)
        .map(|line| line.trim_start_matches('#').trim())
        .find(|line| !line.is_empty())
        .unwrap_or("Third-party app")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;

    fn write_workspace_app(root: &Path, folder_name: &str, lua_source: &str) {
        let app_dir = root.join("apps").join(folder_name);
        fs::create_dir_all(app_dir.join("runtime")).expect("create runtime dir");
        fs::create_dir_all(app_dir.join("prompt")).expect("create prompt dir");
        fs::create_dir_all(app_dir.join("skills")).expect("create skills dir");
        fs::write(app_dir.join("app.toml"), "entry = \"runtime/app.lua\"\n")
            .expect("write app.toml");
        fs::write(app_dir.join("runtime").join("app.lua"), lua_source).expect("write app.lua");
        fs::write(
            app_dir.join("prompt").join("usage.md"),
            "# Notes\n\nUse this app when you need note-oriented workflows.\n",
        )
        .expect("write usage.md");
        fs::write(
            app_dir.join("prompt").join("how_to_use.md"),
            "Read the current state, then use the app-specific tools.\n",
        )
        .expect("write how_to_use.md");
        fs::write(
            app_dir.join("skills").join("search.md"),
            r#"---
name: Search
when_to_use:
  - Need to locate a note quickly
---

## Workflow

1. Narrow the note set.
2. Confirm the target note.
"#,
        )
        .expect("write search skill");
    }

    #[tokio::test]
    async fn loads_workspace_app_prompts_skills_and_lua_hooks() {
        let root = TempDir::new().expect("create temp workspace root");
        let state_root = TempDir::new().expect("create temp workspace state root");
        write_workspace_app(
            root.path(),
            "notes",
            r#"local app = {}

function app.init(ctx)
  return { count = 1 }
end

function app.render_state(ctx, state)
  return {
    title = "Notes App",
    lines = {
      "count=" .. tostring(state.count or 0),
      "app_id=" .. ctx.app_id,
    },
  }
end

function app.list_tools(ctx, state)
  return {
    {
      name = "increment_notes",
      description = "Increase the in-memory counter for testing",
      input_schema = {
        type = "object",
        properties = {
          amount = { type = "integer" }
        },
        required = { "amount" }
      }
    }
  }
end

function app.call_tool(ctx, state, name, args)
  if name ~= "increment_notes" then
    error("unknown tool: " .. name)
  end
  local next_state = {
    count = (state.count or 0) + (args.amount or 1)
  }
  return {
    summary = "counter updated",
    payload = { count = next_state.count },
    ui_lines = { "count=" .. tostring(next_state.count) },
    state = next_state,
  }
end

return app
"#,
        );

        let mut bootstrap =
            bootstrap_workspace_apps_with_state_root(root.path(), state_root.path());
        assert!(
            bootstrap.errors.is_empty(),
            "unexpected loader errors: {:?}",
            bootstrap.errors
        );
        assert_eq!(bootstrap.apps.len(), 1);

        let app = &mut bootstrap.apps[0];
        assert_eq!(app.id().to_string(), "notes");
        assert_eq!(app.usage().description, "Notes");
        assert_eq!(app.skills().len(), 1);
        assert_eq!(app.skills()[0].id, "search");
        assert_eq!(app.skills()[0].name, "Search");
        assert_eq!(app.render_state().title, "Notes App");
        assert!(
            app.render_state()
                .lines
                .iter()
                .any(|line| line == "count=1")
        );
        let tools = app.dynamic_tools().expect("list tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "increment_notes");
        let result = app
            .execute_dynamic_tool("increment_notes", serde_json::json!({ "amount": 4 }))
            .await
            .expect("execute app tool");
        assert_eq!(result.summary, "counter updated");
        assert_eq!(result.payload, serde_json::json!({ "count": 5 }));
        assert!(
            app.render_state()
                .lines
                .iter()
                .any(|line| line == "count=5")
        );
        assert!(
            app.read_skill("search")
                .expect("skill should load")
                .body
                .contains("Workflow")
        );
    }

    #[tokio::test]
    async fn workspace_registry_reloads_only_after_source_digest_changes() {
        let root = TempDir::new().expect("create temp workspace root");
        let state_root = TempDir::new().expect("create temp workspace state root");
        write_workspace_app(
            root.path(),
            "notes",
            r#"local app = {}

function app.render_state(ctx, state)
  return {
    title = "Notes v1",
    lines = { "version=1" },
  }
end

return app
"#,
        );

        let bootstrap = bootstrap_workspace_apps_with_state_root(root.path(), state_root.path());
        assert!(
            bootstrap.errors.is_empty(),
            "bootstrap errors: {:?}",
            bootstrap.errors
        );
        let mut registry = bootstrap.registry;
        let mut apps = AppManager::new(None, bootstrap.apps)
            .await
            .expect("build app manager");

        let render = apps
            .state_renders()
            .into_iter()
            .find(|(id, _)| id.as_str() == "notes")
            .map(|(_, render)| render)
            .expect("notes render");
        assert_eq!(render.title, "Notes v1");

        registry.record_invalidation(WorkspaceAppInvalidation::Dirty {
            folder_name: "notes".to_string(),
        });
        let first_report = registry
            .sync_dirty_apps(&mut apps)
            .await
            .expect("sync dirty apps");
        assert!(
            first_report.is_empty(),
            "digest should suppress identical reloads"
        );

        write_workspace_app(
            root.path(),
            "notes",
            r#"local app = {}

function app.render_state(ctx, state)
  return {
    title = "Notes v2",
    lines = { "version=2" },
  }
end

return app
"#,
        );

        registry.record_invalidation(WorkspaceAppInvalidation::Dirty {
            folder_name: "notes".to_string(),
        });
        let second_report = registry
            .sync_dirty_apps(&mut apps)
            .await
            .expect("reload changed app");
        assert_eq!(second_report.reloaded.len(), 1);
        assert_eq!(second_report.reloaded[0].as_str(), "notes");
        let render = apps
            .state_renders()
            .into_iter()
            .find(|(id, _)| id.as_str() == "notes")
            .map(|(_, render)| render)
            .expect("notes render after reload");
        assert_eq!(render.title, "Notes v2");
    }

    #[tokio::test]
    async fn workspace_registry_keeps_last_good_app_when_reload_fails() {
        let root = TempDir::new().expect("create temp workspace root");
        let state_root = TempDir::new().expect("create temp workspace state root");
        write_workspace_app(
            root.path(),
            "notes",
            r#"local app = {}

function app.render_state(ctx, state)
  return {
    title = "Stable Notes",
    lines = { "version=stable" },
  }
end

return app
"#,
        );

        let bootstrap = bootstrap_workspace_apps_with_state_root(root.path(), state_root.path());
        assert!(
            bootstrap.errors.is_empty(),
            "bootstrap errors: {:?}",
            bootstrap.errors
        );
        let mut registry = bootstrap.registry;
        let mut apps = AppManager::new(None, bootstrap.apps)
            .await
            .expect("build app manager");

        fs::write(
            root.path()
                .join("apps")
                .join("notes")
                .join("runtime")
                .join("app.lua"),
            "return { render_state = function() return ",
        )
        .expect("write broken lua");

        registry.record_invalidation(WorkspaceAppInvalidation::Dirty {
            folder_name: "notes".to_string(),
        });
        let failed_report = registry
            .sync_dirty_apps(&mut apps)
            .await
            .expect("sync broken app");
        assert!(failed_report.reloaded.is_empty());
        assert_eq!(failed_report.errors.len(), 1);
        let render = apps
            .state_renders()
            .into_iter()
            .find(|(id, _)| id.as_str() == "notes")
            .map(|(_, render)| render)
            .expect("notes render after failed reload");
        assert_eq!(render.title, "Stable Notes");

        registry.record_invalidation(WorkspaceAppInvalidation::Dirty {
            folder_name: "notes".to_string(),
        });
        let repeated_failed_report = registry
            .sync_dirty_apps(&mut apps)
            .await
            .expect("sync repeated broken app");
        assert!(
            repeated_failed_report.is_empty(),
            "same broken digest should not re-run reload"
        );
    }

    #[tokio::test]
    async fn workspace_app_notice_lifecycle_tracks_poll_notices() {
        let root = TempDir::new().expect("create temp workspace root");
        let state_root = TempDir::new().expect("create temp workspace state root");
        write_workspace_app(
            root.path(),
            "notifier",
            r#"local app = {}

function app.init(ctx)
  return { needs_attention = true }
end

function app.render_state(ctx, state)
  return {
    title = "Notifier",
    lines = {
      "needs_attention=" .. tostring(state.needs_attention == true),
    },
  }
end

function app.list_tools(ctx, state)
  return {
    {
      name = "ack_notice",
      description = "Acknowledge the current notice",
      input_schema = {
        type = "object",
        properties = {}
      }
    }
  }
end

function app.call_tool(ctx, state, name, args)
  if name ~= "ack_notice" then
    error("unknown tool: " .. name)
  end
  return {
    summary = "notice acknowledged",
    payload = { acknowledged = true },
    state = { needs_attention = false },
  }
end

function app.poll_notices(ctx, state)
  if state.needs_attention then
    return {
      notices = {
        "Background sync needs review"
      }
    }
  end
  return {
    notices = {}
  }
end

return app
"#,
        );

        let bootstrap = bootstrap_workspace_apps_with_state_root(root.path(), state_root.path());
        assert!(
            bootstrap.errors.is_empty(),
            "bootstrap errors: {:?}",
            bootstrap.errors
        );
        let mut apps = AppManager::new(None, bootstrap.apps)
            .await
            .expect("build app manager");
        let app_id =
            AppId::from_workspace_folder("notifier".to_string()).expect("valid workspace app id");

        apps.refresh_all_notices()
            .await
            .expect("refresh notices for workspace app");
        assert_eq!(
            apps.notice_reason(&app_id).as_deref(),
            Some("Background sync needs review")
        );

        apps.focus(app_id.clone()).await.expect("focus notifier");
        apps.execute_dynamic_tool("ack_notice", serde_json::json!({}))
            .await
            .expect("ack workspace notice");
        apps.refresh_notice_for(&app_id)
            .await
            .expect("refresh notice after acknowledgement");
        assert_eq!(apps.notice_reason(&app_id), None);
    }

    #[tokio::test]
    async fn workspace_app_supports_app_local_require_and_basic_io_os() {
        let root = TempDir::new().expect("create temp workspace root");
        let state_root = TempDir::new().expect("create temp workspace state root");
        write_workspace_app(
            root.path(),
            "filesystem",
            r#"local helpers = require("helpers")
local app = {}

function app.init(ctx)
  local payload = assert(io.open(ctx.app_dir .. "/payload.txt", "r"))
  local text = payload:read("*a")
  payload:close()

  local exec_target = ctx.state_dir .. "/exec.txt"
  local sep = package.config:sub(1, 1)
  local command
  if sep == "\\" then
    command = 'cmd /C echo shell-output> "' .. exec_target .. '"'
  else
    command = '/bin/sh -lc \'printf shell-output > "' .. exec_target .. '"\''
  end
  assert(os.execute(command))

  local exec_file = assert(io.open(exec_target, "r"))
  local exec_text = exec_file:read("*a")
  exec_file:close()

  return {
    payload = helpers.trim(text),
    exec = helpers.trim(exec_text),
  }
end

function app.render_state(ctx, state)
  return {
    title = "Filesystem App",
    lines = {
      "payload=" .. tostring(state.payload),
      "exec=" .. tostring(state.exec),
    },
  }
end

return app
"#,
        );
        let app_dir = root.path().join("apps").join("filesystem");
        fs::write(
            app_dir.join("runtime").join("helpers.lua"),
            r#"local M = {}

function M.trim(value)
  return (value:gsub("^%s+", ""):gsub("%s+$", ""))
end

return M
"#,
        )
        .expect("write helper module");
        fs::write(app_dir.join("payload.txt"), "hello from payload\n").expect("write payload");

        let bootstrap = bootstrap_workspace_apps_with_state_root(root.path(), state_root.path());
        assert!(
            bootstrap.errors.is_empty(),
            "bootstrap errors: {:?}",
            bootstrap.errors
        );
        assert_eq!(bootstrap.apps.len(), 1);

        let render = bootstrap.apps[0].render_state();
        assert_eq!(render.title, "Filesystem App");
        assert!(
            render
                .lines
                .iter()
                .any(|line| line == "payload=hello from payload")
        );
        assert!(render.lines.iter().any(|line| line == "exec=shell-output"));
        let exec_path = state_root.path().join("filesystem").join("exec.txt");
        let exec_text = fs::read_to_string(exec_path).expect("read exec output");
        assert_eq!(exec_text.trim(), "shell-output");
    }

    #[tokio::test]
    async fn workspace_app_rejects_invalid_tool_input_against_schema() {
        let root = TempDir::new().expect("create temp workspace root");
        let state_root = TempDir::new().expect("create temp workspace state root");
        write_workspace_app(
            root.path(),
            "schema-input",
            r#"local app = {}

function app.list_tools(ctx, state)
  return {
    {
      name = "typed_increment",
      description = "Increment with typed input",
      input_schema = {
        type = "object",
        properties = {
          amount = { type = "integer" }
        },
        required = { "amount" }
      }
    }
  }
end

function app.call_tool(ctx, state, name, args)
  return {
    summary = "ok",
    payload = { count = args.amount }
  }
end

return app
"#,
        );

        let bootstrap = bootstrap_workspace_apps_with_state_root(root.path(), state_root.path());
        assert!(
            bootstrap.errors.is_empty(),
            "bootstrap errors: {:?}",
            bootstrap.errors
        );
        let mut apps = AppManager::new(
            Some(AppId::from_workspace_folder("schema-input").expect("valid app id")),
            bootstrap.apps,
        )
        .await
        .expect("build app manager");

        let err = apps
            .execute_dynamic_tool("typed_increment", serde_json::json!({ "amount": "4" }))
            .await
            .expect_err("schema mismatch should fail before lua call");
        let message = err.to_string();
        assert!(message.contains("arguments for workspace app tool `typed_increment`.amount"));
        assert!(message.contains("must be an integer"));
    }

    #[tokio::test]
    async fn workspace_app_rejects_invalid_tool_output_against_schema() {
        let root = TempDir::new().expect("create temp workspace root");
        let state_root = TempDir::new().expect("create temp workspace state root");
        write_workspace_app(
            root.path(),
            "schema-output",
            r#"local app = {}

function app.list_tools(ctx, state)
  return {
    {
      name = "bad_payload",
      description = "Return a payload that violates the declared schema",
      input_schema = {
        type = "object",
        properties = {}
      },
      output_schema = {
        type = "object",
        properties = {
          count = { type = "integer" }
        },
        required = { "count" }
      }
    }
  }
end

function app.call_tool(ctx, state, name, args)
  return {
    summary = "broken",
    payload = { count = "not-an-integer" }
  }
end

return app
"#,
        );

        let bootstrap = bootstrap_workspace_apps_with_state_root(root.path(), state_root.path());
        assert!(
            bootstrap.errors.is_empty(),
            "bootstrap errors: {:?}",
            bootstrap.errors
        );
        let mut apps = AppManager::new(
            Some(AppId::from_workspace_folder("schema-output").expect("valid app id")),
            bootstrap.apps,
        )
        .await
        .expect("build app manager");

        let err = apps
            .execute_dynamic_tool("bad_payload", serde_json::json!({}))
            .await
            .expect_err("output schema mismatch should fail");
        let message = err.to_string();
        assert!(message.contains("payload for workspace app tool `bad_payload`.count"));
        assert!(message.contains("must be an integer"));
    }

    #[tokio::test]
    async fn workspace_app_rejects_skill_frontmatter_with_unknown_fields() {
        let root = TempDir::new().expect("create temp workspace root");
        let state_root = TempDir::new().expect("create temp workspace state root");
        write_workspace_app(
            root.path(),
            "bad-skill-meta",
            r#"local app = {}

function app.render_state(ctx, state)
  return { title = "Bad Skill Meta", lines = {} }
end

return app
"#,
        );
        let app_dir = root.path().join("apps").join("bad-skill-meta");
        fs::write(
            app_dir.join("skills").join("search.md"),
            r#"---
name: Search
unexpected: true
---

## Workflow

1. Search.
"#,
        )
        .expect("write invalid skill");

        let bootstrap = bootstrap_workspace_apps_with_state_root(root.path(), state_root.path());
        assert_eq!(bootstrap.apps.len(), 0);
        assert_eq!(bootstrap.errors.len(), 1);
        assert!(bootstrap.errors[0].contains("unknown field `unexpected`"));
    }

    #[tokio::test]
    async fn workspace_app_rejects_skill_with_empty_content() {
        let root = TempDir::new().expect("create temp workspace root");
        let state_root = TempDir::new().expect("create temp workspace state root");
        write_workspace_app(
            root.path(),
            "bad-skill-body",
            r#"local app = {}

function app.render_state(ctx, state)
  return { title = "Bad Skill Body", lines = {} }
end

return app
"#,
        );
        let app_dir = root.path().join("apps").join("bad-skill-body");
        fs::write(
            app_dir.join("skills").join("search.md"),
            r#"---
name: Search
when_to_use:
  - ""
---
"#,
        )
        .expect("write invalid skill body");

        let bootstrap = bootstrap_workspace_apps_with_state_root(root.path(), state_root.path());
        assert_eq!(bootstrap.apps.len(), 0);
        assert_eq!(bootstrap.errors.len(), 1);
        assert!(
            bootstrap.errors[0].contains("when_to_use")
                || bootstrap.errors[0].contains("must contain non-empty markdown body")
        );
    }
}
