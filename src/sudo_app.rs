//! Explicit, reviewed execution outside the device sandbox.
use crate::{
    app::{App, AppId, AppToolSpec},
    model_protocol::AgentToolCall,
};
use daat_locus_macros::model_schema;
use miette::{Result, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecuteArgs {
    /// Fully qualified App tool name, for example terminal__terminal_exec.
    pub tool: String,
    /// JSON object matching the target tool's input schema.
    pub arguments_json: String,
    pub reason: String,
    /// Explicitly allow restarting an incompatible native App connection. Never replay prior tool effects.
    pub restart_native: bool,
    /// Resume an immutable Sudo application after Core's decision; otherwise null.
    pub approval_id: Option<String>,
}

/// A single immutable invocation, never a remembered permission fingerprint.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Invocation {
    pub tool: String,
    pub arguments: Value,
    pub administrator: bool,
    pub restart_native: bool,
    pub cwd: PathBuf,
    pub app_revision: Option<u64>,
    pub app_source: Option<Value>,
}
impl Invocation {
    pub fn call(&self, id: &str) -> AgentToolCall {
        AgentToolCall {
            id: id.into(),
            name: self.tool.clone(),
            arguments: self.arguments.clone(),
        }
    }
}

pub struct SudoApp;
impl SudoApp {
    pub(crate) fn request(call: &AgentToolCall, cwd: PathBuf) -> Result<(ExecuteArgs, Invocation)> {
        let administrator = match call.name.as_str() {
            "sudo__execute" => false,
            "sudo__execute_as_admin" => true,
            _ => return Err(miette!("Unknown Sudo operation")),
        };
        let args: ExecuteArgs = serde_json::from_value(call.arguments.clone())
            .map_err(|_| miette!("Invalid Sudo arguments"))?;
        if args.reason.trim().is_empty()
            || args.reason.len() > 8192
            || args.arguments_json.len() > 16 * 1024
        {
            return Err(miette!(
                "Provide bounded target arguments and a clear reason"
            ));
        }
        let (app, _) = args
            .tool
            .split_once(AppId::TOOL_NAME_SEPARATOR)
            .ok_or_else(|| miette!("Sudo requires a fully qualified App tool"))?;
        if app == "sudo" {
            return Err(miette!("Nested Sudo invocations are not allowed"));
        }
        let arguments: Value = serde_json::from_str(&args.arguments_json)
            .map_err(|_| miette!("Target arguments must be valid JSON"))?;
        if !arguments.is_object() {
            return Err(miette!("Target arguments must be a JSON object"));
        }
        let invocation = Invocation {
            tool: args.tool.clone(),
            arguments,
            administrator,
            restart_native: args.restart_native,
            cwd,
            app_revision: None,
            app_source: None,
        };
        Ok((args, invocation))
    }
}
#[async_trait::async_trait]
impl App for SudoApp {
    fn id(&self) -> AppId {
        AppId::new("sudo").expect("static App ID")
    }
    fn can_passivate(&self) -> bool {
        true
    }
    fn tool_specs(&self) -> Vec<AppToolSpec> {
        [
            ("execute", "Call any installed App tool outside the sandbox as the current OS user. Wait for automatic review; a refusal is submitted to Core asynchronously. This does not request administrator privileges."),
            ("execute_as_admin", "Call any installed App tool outside the sandbox with OS administrator privileges. Automatic review is required before the native elevation prompt; a refusal is submitted to Core asynchronously."),
        ].into_iter().map(|(name, description)| AppToolSpec {
            name: name.into(), description: description.into(),
            input_schema: crate::schema_utils::model_schema_for::<ExecuteArgs>(),
        }).collect()
    }
}
