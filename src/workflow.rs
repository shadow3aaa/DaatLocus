use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use miette::{Result, miette};
use mlua::{
    Function, HookTriggers, Lua, LuaOptions, LuaSerdeExt, StdLib, Table, ThreadStatus,
    Value as LuaValue,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    app::{AppId, AppToolExecutionContext, AppToolExecutionResult},
    context::Context,
    core::ModelRequestOptions,
    reasoning::runtime::{
        AgentMessage, AgentToolCall, AgentToolInputSpec, AgentToolSpec, AgentTurnItem,
        AgentTurnRequest,
    },
    sandbox::{SandboxAsyncChild, SandboxProcessOptions, SandboxStdio},
    schema_utils::{validate_model_facing_schema, validate_value_against_schema},
};

const WORKFLOW_TOOL_PREFIX: &str = "workflow__";
const WORKER_MAX_TOOL_TURNS: usize = 16;
const LUA_IO_MAX_BYTES: usize = 4 * 1024 * 1024;
const LUA_SHELL_MAX_BYTES: usize = 4 * 1024 * 1024;
const WORKFLOW_INTERRUPTED_ERROR: &str = "workflow interrupted";
const WORKFLOW_LUA_INTERRUPT_INSTRUCTION_INTERVAL: u32 = 1_000;

#[derive(Clone, Default)]
pub struct WorkflowCancellationRegistry {
    active: Arc<parking_lot::Mutex<Option<WorkflowCancellation>>>,
}

impl WorkflowCancellationRegistry {
    pub fn begin(&self) -> WorkflowCancellation {
        let cancellation = WorkflowCancellation::new();
        *self.active.lock() = Some(cancellation.clone());
        cancellation
    }

    pub fn interrupt_active(&self) -> bool {
        let active = self.active.lock().clone();
        if let Some(cancellation) = active {
            cancellation.interrupt();
            true
        } else {
            false
        }
    }

    pub fn clear(&self, cancellation: &WorkflowCancellation) {
        let mut active = self.active.lock();
        if active
            .as_ref()
            .is_some_and(|current| current.is_same(cancellation))
        {
            *active = None;
        }
    }
}

#[derive(Clone, Debug)]
pub struct WorkflowCancellation {
    interrupted: Arc<AtomicBool>,
}

impl WorkflowCancellation {
    pub fn new() -> Self {
        Self {
            interrupted: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn interrupt(&self) {
        self.interrupted.store(true, Ordering::SeqCst);
    }

    fn is_interrupted(&self) -> bool {
        self.interrupted.load(Ordering::SeqCst)
    }

    fn is_same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.interrupted, &other.interrupted)
    }
}

#[derive(Clone, Debug)]
struct WorkflowInterrupted;

impl std::fmt::Display for WorkflowInterrupted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(WORKFLOW_INTERRUPTED_ERROR)
    }
}

impl std::error::Error for WorkflowInterrupted {}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    pub id: String,
    pub path: PathBuf,
    pub input_schema: Value,
    pub output_schema: Value,
}

impl WorkflowDefinition {
    pub fn tool_name(&self) -> String {
        format!("{WORKFLOW_TOOL_PREFIX}{}", self.id)
    }
}

#[derive(Clone, Debug, Default)]
pub struct WorkflowCatalog {
    root: PathBuf,
    definitions: BTreeMap<String, WorkflowDefinition>,
    errors: Vec<WorkflowLoadError>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowLoadError {
    pub path: PathBuf,
    pub message: String,
}

impl WorkflowCatalog {
    pub fn load() -> Self {
        let mut catalog = Self {
            root: crate::daat_locus_paths::daat_locus_paths_sync().workflows_dir(),
            definitions: BTreeMap::new(),
            errors: Vec::new(),
        };
        catalog.reload();
        catalog
    }

    pub fn definitions(&self) -> impl Iterator<Item = &WorkflowDefinition> {
        self.definitions.values()
    }

    pub fn get(&self, id: &str) -> Option<&WorkflowDefinition> {
        self.definitions.get(id)
    }

    pub fn errors(&self) -> &[WorkflowLoadError] {
        &self.errors
    }

    pub fn reload(&mut self) {
        self.definitions.clear();
        self.errors.clear();
        if let Err(err) = fs::create_dir_all(&self.root) {
            self.errors.push(WorkflowLoadError {
                path: self.root.clone(),
                message: format!("failed to create workflow directory: {err}"),
            });
            return;
        }
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(err) => {
                self.errors.push(WorkflowLoadError {
                    path: self.root.clone(),
                    message: format!("failed to read workflow directory: {err}"),
                });
                return;
            }
        };
        let mut paths = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("lua"))
            .collect::<Vec<_>>();
        paths.sort();
        for path in paths {
            match load_workflow_definition(&path) {
                Ok(definition) => {
                    if self.definitions.contains_key(&definition.id) {
                        self.errors.push(WorkflowLoadError {
                            path,
                            message: format!("duplicate workflow id `{}`", definition.id),
                        });
                    } else {
                        self.definitions.insert(definition.id.clone(), definition);
                    }
                }
                Err(err) => self.errors.push(WorkflowLoadError {
                    path,
                    message: err.to_string(),
                }),
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct WorkflowInvocation {
    pub workflow_id: String,
    pub input: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowInvocationStatus {
    Completed,
    Failed,
    Interrupted,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowInvocationResult {
    pub workflow_id: String,
    pub status: WorkflowInvocationStatus,
    pub output: Option<Value>,
    pub message: String,
}

#[derive(Clone, Debug)]
struct WorkerDefinition {
    model: WorkerModel,
    input_schema: Value,
    output_schema: Value,
    instruction: String,
    extra_tools: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum WorkerModel {
    Main,
    Efficient,
}

#[derive(Clone)]
struct LocalToolDefinition {
    name: String,
    input_schema: Value,
    output_schema: Value,
    run: Function,
    lua: Lua,
}

#[derive(Clone, Debug)]
struct WorkerInvocation {
    definition: WorkerDefinition,
    input: Value,
}

#[derive(Clone, Debug)]
enum WorkflowYield {
    Worker(WorkerInvocation),
    Workers(Vec<WorkerInvocation>),
}

#[derive(Clone, Debug)]
struct WorkflowWorkerResult {
    output: Value,
}

#[derive(Clone, Debug)]
struct WorkerTool {
    name: String,
    description: String,
    input_schema: Value,
    kind: WorkerToolKind,
}

#[derive(Clone, Debug)]
enum WorkerToolKind {
    App {
        owner_app_id: AppId,
        app_tool_name: String,
    },
    Local {
        local_name: String,
        output_schema: Value,
    },
}

pub async fn invoke(
    context: &mut Context,
    invocation: WorkflowInvocation,
) -> Result<WorkflowInvocationResult> {
    let cancellation = context.workflow_cancellation.begin();
    let result = invoke_with_cancellation(context, invocation, Some(&cancellation)).await;
    context.workflow_cancellation.clear(&cancellation);
    result
}

pub async fn invoke_with_cancellation(
    context: &mut Context,
    invocation: WorkflowInvocation,
    cancellation: Option<&WorkflowCancellation>,
) -> Result<WorkflowInvocationResult> {
    let definition = context
        .workflows
        .get(&invocation.workflow_id)
        .cloned()
        .ok_or_else(|| miette!("unknown workflow `{}`", invocation.workflow_id))?;
    validate_value_against_schema(
        &invocation.input,
        &definition.input_schema,
        "workflow input",
    )?;
    let source = fs::read_to_string(&definition.path).map_err(|err| {
        miette!(
            "failed to read workflow `{}` at {}: {err}",
            definition.id,
            definition.path.display()
        )
    })?;

    match run_workflow_script(
        &source,
        &definition,
        invocation.input,
        context,
        cancellation,
    )
    .await
    {
        Ok(output) => {
            validate_value_against_schema(&output, &definition.output_schema, "workflow output")?;
            Ok(WorkflowInvocationResult {
                workflow_id: definition.id,
                status: WorkflowInvocationStatus::Completed,
                output: Some(output),
                message: "workflow completed".to_string(),
            })
        }
        Err(err) if is_workflow_interrupted_error(&err) => Ok(WorkflowInvocationResult {
            workflow_id: definition.id,
            status: WorkflowInvocationStatus::Interrupted,
            output: None,
            message: WORKFLOW_INTERRUPTED_ERROR.to_string(),
        }),
        Err(err) => Ok(WorkflowInvocationResult {
            workflow_id: definition.id,
            status: WorkflowInvocationStatus::Failed,
            output: None,
            message: err.to_string(),
        }),
    }
}

fn load_workflow_definition(path: &Path) -> Result<WorkflowDefinition> {
    let source = fs::read_to_string(path)
        .map_err(|err| miette!("failed to read workflow source {}: {err}", path.display()))?;
    let lua = new_lua().map_err(lua_error)?;
    let definition_slot = Arc::new(Mutex::new(None::<WorkflowDefinition>));
    let path = path.to_path_buf();
    let source_name = path.display().to_string();
    lua.scope(|scope| {
        let workflow = lua.create_table()?;
        let slot = definition_slot.clone();
        let definition_path = path.clone();
        workflow.set(
            "define",
            scope.create_function(move |lua, table: Table| {
                let input_schema: Value = lua.from_value(table.get("input")?)?;
                let output_schema: Value = lua.from_value(table.get("output")?)?;
                validate_model_facing_schema(&input_schema).map_err(mlua::Error::external)?;
                validate_model_facing_schema(&output_schema).map_err(mlua::Error::external)?;
                if !matches!(table.get::<LuaValue>("run")?, LuaValue::Function(_)) {
                    return Err(mlua::Error::external("workflow.define requires a run function"));
                }
                let id = definition_path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .ok_or_else(|| {
                        mlua::Error::external("workflow file must have a UTF-8 filename stem")
                    })?
                    .to_string();
                if !is_valid_workflow_name(&id) {
                    return Err(mlua::Error::external(
                        "workflow filename stem must use lowercase letters, digits, and single underscores",
                    ));
                }
                let mut guard = slot
                    .lock()
                    .map_err(|_| mlua::Error::external("workflow definition lock poisoned"))?;
                if guard.is_some() {
                    return Err(mlua::Error::external(
                        "workflow may call workflow.define only once",
                    ));
                }
                *guard = Some(WorkflowDefinition {
                    id,
                    path: definition_path.clone(),
                    input_schema,
                    output_schema,
                });
                Ok(())
            })?,
        )?;
        install_loading_stubs(scope, &workflow)?;
        lua.globals().set("workflow", workflow)?;
        lua.load(&source).set_name(&source_name).exec()
    })
    .map_err(lua_error)?;
    definition_slot
        .lock()
        .map_err(|_| miette!("workflow definition lock poisoned"))?
        .clone()
        .ok_or_else(|| miette!("workflow {} did not call workflow.define", path.display()))
}

fn install_loading_stubs<'scope, 'env>(
    scope: &'scope mlua::Scope<'scope, 'env>,
    workflow: &Table,
) -> mlua::Result<()>
where
    'env: 'scope,
{
    workflow.set("agent", scope.create_function(|_, _: Table| Ok(()))?)?;
    workflow.set(
        "await",
        scope.create_function(|_, _: LuaValue| Ok(LuaValue::Nil))?,
    )?;
    workflow.set(
        "await_all",
        scope.create_function(|lua, _: LuaValue| lua.create_table())?,
    )?;
    workflow.set("tool", scope.create_function(|_, _: Table| Ok(()))?)?;
    Ok(())
}

async fn run_workflow_script(
    source: &str,
    definition: &WorkflowDefinition,
    input: Value,
    context: &mut Context,
    cancellation: Option<&WorkflowCancellation>,
) -> Result<Value> {
    ensure_workflow_not_interrupted(cancellation)?;
    let lua = new_lua().map_err(lua_error)?;
    install_workflow_interrupt_hook(&lua, cancellation).map_err(lua_error)?;
    install_sandboxed_lua_environment(&lua, context).map_err(lua_error)?;
    let run_slot = Arc::new(Mutex::new(None::<Function>));
    let local_tools = Arc::new(Mutex::new(BTreeMap::<String, LocalToolDefinition>::new()));
    let source_name = definition.path.display().to_string();
    let expected_definition = definition.clone();

    let workflow = lua.create_table().map_err(lua_error)?;
    let define_slot = run_slot.clone();
    let define_definition = expected_definition.clone();
    workflow
        .set(
            "define",
            lua.create_function(move |lua, table: Table| {
                let input_schema: Value = lua.from_value(table.get("input")?)?;
                let output_schema: Value = lua.from_value(table.get("output")?)?;
                if input_schema != define_definition.input_schema
                    || output_schema != define_definition.output_schema
                {
                    return Err(mlua::Error::external(
                        "workflow definition changed after loading; reload it before invoking",
                    ));
                }
                let run = table.get::<Function>("run")?;
                *define_slot
                    .lock()
                    .map_err(|_| mlua::Error::external("workflow run-function lock poisoned"))? =
                    Some(run);
                Ok(())
            })
            .map_err(lua_error)?,
        )
        .map_err(lua_error)?;
    install_execution_functions(&lua, &workflow).map_err(lua_error)?;
    let local_tool_slot = local_tools.clone();
    workflow
        .set(
            "tool",
            lua.create_function(move |lua, table: Table| {
                let local_tool = local_tool_definition_from_lua(lua, table)?;
                let mut tools = local_tool_slot
                    .lock()
                    .map_err(|_| mlua::Error::external("workflow local-tool lock poisoned"))?;
                if tools.contains_key(&local_tool.name) {
                    return Err(mlua::Error::external(format!(
                        "workflow declares duplicate local tool `{}`",
                        local_tool.name
                    )));
                }
                tools.insert(local_tool.name.clone(), local_tool);
                Ok(())
            })
            .map_err(lua_error)?,
        )
        .map_err(lua_error)?;
    lua.globals().set("workflow", workflow).map_err(lua_error)?;
    lua.load(source)
        .set_name(&source_name)
        .exec()
        .map_err(lua_error)?;

    let run = run_slot
        .lock()
        .map_err(|_| miette!("workflow run-function lock poisoned"))?
        .clone()
        .ok_or_else(|| {
            miette!(
                "workflow `{}` did not provide a run function",
                definition.id
            )
        })?;
    let lua_input = lua.to_value(&input).map_err(lua_error)?;
    let lua_context = lua.create_table().map_err(lua_error)?;
    let thread = lua.create_thread(run).map_err(lua_error)?;
    let mut yielded: LuaValue = thread.resume((lua_input, lua_context)).map_err(lua_error)?;
    while thread.status() == ThreadStatus::Resumable {
        ensure_workflow_not_interrupted(cancellation)?;
        let request = workflow_yield_from_lua(&lua, yielded).map_err(lua_error)?;
        let result = match request {
            WorkflowYield::Worker(worker) => {
                run_worker(
                    context,
                    worker.definition,
                    worker.input,
                    &local_tools,
                    cancellation,
                )
                .await?
                .output
            }
            WorkflowYield::Workers(workers) => {
                let mut outputs = Vec::with_capacity(workers.len());
                for worker in workers {
                    outputs.push(
                        run_worker(
                            context,
                            worker.definition,
                            worker.input,
                            &local_tools,
                            cancellation,
                        )
                        .await?
                        .output,
                    );
                }
                Value::Array(outputs)
            }
        };
        yielded = thread
            .resume(lua.to_value(&result).map_err(lua_error)?)
            .map_err(lua_error)?;
    }
    lua.from_value(yielded).map_err(|err| {
        miette!(
            "workflow `{}` returned a non-JSON value: {err}",
            definition.id
        )
    })
}

fn install_execution_functions(lua: &Lua, workflow: &Table) -> mlua::Result<()> {
    workflow.set(
        "agent",
        lua.create_function(|lua, table: Table| {
            let definition = worker_definition_from_lua(lua, table)?;
            let factory = lua.create_table()?;
            factory.set("definition", worker_definition_to_lua(lua, &definition)?)?;
            factory.set(
                "run",
                lua.create_function(|lua, (factory, input): (Table, LuaValue)| {
                    let handle = lua.create_table()?;
                    handle.set("definition", factory.get::<Table>("definition")?)?;
                    handle.set("input", input)?;
                    Ok(handle)
                })?,
            )?;
            Ok(factory)
        })?,
    )?;
    workflow.set(
        "await",
        lua.create_function(|lua, handle: Table| {
            if handle.get::<bool>("awaited").unwrap_or(false) {
                return Err::<LuaValue, _>(mlua::Error::external(
                    "workflow handle was already awaited",
                ));
            }
            handle.set("awaited", true)?;
            let coroutine: Table = lua.globals().get("coroutine")?;
            let yield_function: Function = coroutine.get("yield")?;
            yield_function.call(handle)
        })?,
    )?;
    workflow.set(
        "await_all",
        lua.create_function(|lua, handles: Table| -> mlua::Result<LuaValue> {
            for handle in handles.sequence_values::<Table>() {
                let handle = handle?;
                if handle.get::<bool>("awaited").unwrap_or(false) {
                    return Err(mlua::Error::external("workflow handle was already awaited"));
                }
                handle.set("awaited", true)?;
            }
            let request = lua.create_table()?;
            request.set("handles", handles)?;
            let coroutine: Table = lua.globals().get("coroutine")?;
            let yield_function: Function = coroutine.get("yield")?;
            yield_function.call(request)
        })?,
    )?;
    Ok(())
}

fn workflow_yield_from_lua(lua: &Lua, yielded: LuaValue) -> mlua::Result<WorkflowYield> {
    let handle = match yielded {
        LuaValue::Table(handle) => handle,
        value => {
            return Err(mlua::Error::external(format!(
                "workflow yielded an unsupported value `{}`; use workflow.await(handle) or workflow.await_all(handles)",
                value.type_name()
            )));
        }
    };
    if let Ok(handles) = handle.get::<Table>("handles") {
        let mut workers = Vec::new();
        for handle in handles.sequence_values::<Table>() {
            workers.push(worker_invocation_from_lua(lua, handle?)?);
        }
        return Ok(WorkflowYield::Workers(workers));
    }
    Ok(WorkflowYield::Worker(worker_invocation_from_lua(
        lua, handle,
    )?))
}

fn worker_invocation_from_lua(lua: &Lua, handle: Table) -> mlua::Result<WorkerInvocation> {
    let definition = worker_definition_from_lua(lua, handle.get("definition")?)?;
    let input: Value = lua.from_value(handle.get("input")?)?;
    Ok(WorkerInvocation { definition, input })
}

async fn run_worker(
    context: &mut Context,
    definition: WorkerDefinition,
    input: Value,
    local_tools: &Arc<Mutex<BTreeMap<String, LocalToolDefinition>>>,
    cancellation: Option<&WorkflowCancellation>,
) -> Result<WorkflowWorkerResult> {
    ensure_workflow_not_interrupted(cancellation)?;
    validate_value_against_schema(&input, &definition.input_schema, "worker input")?;
    let tools = build_worker_tools(context, &definition, local_tools)?;
    let mut messages = vec![
        AgentMessage::system(worker_system_instruction(&definition.instruction)),
        AgentMessage::user(
            serde_json::to_string_pretty(&input).unwrap_or_else(|_| input.to_string()),
        ),
    ];

    for turn in 0..WORKER_MAX_TOOL_TURNS {
        ensure_workflow_not_interrupted(cancellation)?;
        let request = AgentTurnRequest {
            messages: messages.clone(),
            tools: tools
                .iter()
                .map(|tool| AgentToolSpec {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    input_spec: AgentToolInputSpec::JsonSchema {
                        schema: tool.input_schema.clone(),
                    },
                })
                .collect(),
        };
        let response =
            complete_workflow_worker_turn(context, &definition.model, request, cancellation)
                .await?;
        let mut assistant_messages = Vec::new();
        let mut calls = Vec::new();
        for item in response.items {
            match item {
                AgentTurnItem::AssistantMessage { content } if !content.trim().is_empty() => {
                    assistant_messages.push(content)
                }
                AgentTurnItem::ToolCall { call } => calls.push(call),
                AgentTurnItem::AssistantMessage { .. } => {}
            }
        }
        if calls.is_empty() {
            let output = parse_worker_output(
                response
                    .last_assistant_message
                    .or_else(|| assistant_messages.into_iter().last())
                    .ok_or_else(|| miette!("worker returned no output"))?,
            )?;
            validate_value_against_schema(&output, &definition.output_schema, "worker output")?;
            return Ok(WorkflowWorkerResult { output });
        }

        messages.push(AgentMessage::assistant_tool_call_protocol_with_reasoning(
            (!assistant_messages.is_empty()).then(|| assistant_messages.join("\n\n")),
            response.last_reasoning_content,
            calls.clone(),
        ));
        for call in calls {
            ensure_workflow_not_interrupted(cancellation)?;
            let result =
                execute_worker_tool(context, &call, &tools, local_tools, cancellation).await;
            let content = match result {
                Ok(value) => json!({ "ok": true, "result": value }).to_string(),
                Err(err) => json!({ "ok": false, "error": err.to_string() }).to_string(),
            };
            messages.push(AgentMessage::tool(call.id, call.name, content));
        }
        if turn + 1 == WORKER_MAX_TOOL_TURNS {
            return Err(miette!(
                "worker exceeded the maximum of {WORKER_MAX_TOOL_TURNS} tool turns"
            ));
        }
    }
    Err(miette!("worker did not complete"))
}

fn build_worker_tools(
    context: &Context,
    definition: &WorkerDefinition,
    local_tools: &Arc<Mutex<BTreeMap<String, LocalToolDefinition>>>,
) -> Result<Vec<WorkerTool>> {
    let mut tools = BTreeMap::<String, WorkerTool>::new();
    for (owner_app_id, app_tools) in context.apps.all_tool_specs() {
        for spec in app_tools {
            if matches!(spec.name.as_str(), "get_state" | "next_review") {
                continue;
            }
            validate_model_facing_schema(&spec.input_schema)?;
            let name = owner_app_id.mangle_tool_name(&spec.name);
            if tools.contains_key(&name) {
                return Err(miette!(
                    "workflow worker App tool name `{name}` is duplicated"
                ));
            }
            tools.insert(
                name.clone(),
                WorkerTool {
                    name,
                    description: spec.description,
                    input_schema: spec.input_schema,
                    kind: WorkerToolKind::App {
                        owner_app_id: owner_app_id.clone(),
                        app_tool_name: spec.name,
                    },
                },
            );
        }
    }
    let declared = local_tools
        .lock()
        .map_err(|_| miette!("workflow local-tool lock poisoned"))?;
    for name in &definition.extra_tools {
        let local = declared
            .get(name)
            .ok_or_else(|| miette!("workflow worker references undeclared local tool `{name}`"))?;
        if tools.contains_key(name) {
            return Err(miette!("workflow worker tool name `{name}` is duplicated"));
        }
        tools.insert(
            name.clone(),
            WorkerTool {
                name: name.clone(),
                description: format!("Workflow-local tool `{name}`."),
                input_schema: local.input_schema.clone(),
                kind: WorkerToolKind::Local {
                    local_name: name.clone(),
                    output_schema: local.output_schema.clone(),
                },
            },
        );
    }
    Ok(tools.into_values().collect())
}

async fn complete_workflow_worker_turn(
    context: &Context,
    model: &WorkerModel,
    request: AgentTurnRequest,
    cancellation: Option<&WorkflowCancellation>,
) -> Result<crate::reasoning::runtime::AgentTurnStreamResult> {
    ensure_workflow_not_interrupted(cancellation)?;
    let request_future = match model {
        WorkerModel::Main => {
            let provider = context.model_provider.as_ref();
            let options = ModelRequestOptions::for_agent_turn(provider, &request, None)?;
            provider.complete_agent_turn(request, options)
        }
        WorkerModel::Efficient => {
            let provider = context.efficient_model_provider.as_ref();
            let options = ModelRequestOptions::for_agent_turn(provider, &request, None)?;
            provider.complete_agent_turn(request, options)
        }
    };
    tokio::pin!(request_future);
    if let Some(cancellation) = cancellation {
        tokio::select! {
            response = &mut request_future => response,
            _ = wait_for_workflow_interrupt(cancellation) => Err(miette!(WorkflowInterrupted)),
        }
    } else {
        request_future.await
    }
}

async fn wait_for_workflow_interrupt(cancellation: &WorkflowCancellation) {
    while !cancellation.is_interrupted() {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

async fn execute_worker_tool(
    context: &mut Context,
    call: &AgentToolCall,
    tools: &[WorkerTool],
    local_tools: &Arc<Mutex<BTreeMap<String, LocalToolDefinition>>>,
    cancellation: Option<&WorkflowCancellation>,
) -> Result<Value> {
    ensure_workflow_not_interrupted(cancellation)?;
    let tool = tools
        .iter()
        .find(|tool| tool.name == call.name)
        .ok_or_else(|| miette!("worker attempted undeclared tool `{}`", call.name))?;
    validate_value_against_schema(&call.arguments, &tool.input_schema, "worker tool input")?;
    match &tool.kind {
        WorkerToolKind::App {
            owner_app_id,
            app_tool_name,
        } => {
            ensure_workflow_not_interrupted(cancellation)?;
            let app_context = AppToolExecutionContext {
                execution_cwd: context.execution_cwd.clone(),
                sandbox_policy: context.sandbox_policy.clone(),
                dashboard_tx: context.dashboard_tx.clone(),
                tool_output_max_tokens: context
                    .config
                    .main_model_config()
                    .tool_output_max_tokens
                    .max(1),
                turn_epoch: context.runtime_turn_epoch,
            };
            let app_call = call.with_name(app_tool_name.clone());
            let result = context
                .apps
                .execute_tool_for_app(owner_app_id, &app_call, &app_context)
                .await?;
            Ok(worker_app_result_value(result))
        }
        WorkerToolKind::Local {
            local_name,
            output_schema,
        } => {
            let local = local_tools
                .lock()
                .map_err(|_| miette!("workflow local-tool lock poisoned"))?
                .get(local_name)
                .cloned()
                .ok_or_else(|| miette!("workflow local tool `{local_name}` disappeared"))?;
            let lua_input = local.lua.to_value(&call.arguments).map_err(lua_error)?;
            let lua_output: LuaValue = local.run.call(lua_input).map_err(lua_error)?;
            let output: Value = local.lua.from_value(lua_output).map_err(lua_error)?;
            ensure_workflow_not_interrupted(cancellation)?;
            validate_value_against_schema(&output, output_schema, "workflow local tool output")?;
            Ok(output)
        }
    }
}

fn worker_app_result_value(result: AppToolExecutionResult) -> Value {
    json!({
        "summary": result.summary,
        "payload": result.payload,
        "model_content": result.model_content,
    })
}

fn worker_system_instruction(instruction: &str) -> String {
    format!(
        "You are an isolated workflow worker. You receive only the workflow instruction, typed input, host-provided App tools, and explicitly declared workflow-local tools. Do not claim or finish user events, do not assume access to session conversation history, and do not call tools outside your declared surface. When the work is complete, return exactly one JSON object matching your declared output schema with no markdown.\n\nWorkflow instruction:\n{instruction}"
    )
}

fn parse_worker_output(content: String) -> Result<Value> {
    serde_json::from_str(content.trim()).map_err(|err| {
        miette!(
            "worker returned invalid JSON output: {err}; output={}",
            content.trim()
        )
    })
}

fn worker_definition_from_lua(lua: &Lua, table: Table) -> mlua::Result<WorkerDefinition> {
    let model = match table
        .get::<Option<String>>("model")?
        .as_deref()
        .unwrap_or("main")
    {
        "main" => WorkerModel::Main,
        "efficient" => WorkerModel::Efficient,
        other => {
            return Err(mlua::Error::external(format!(
                "workflow.agent model must be `main` or `efficient`, got `{other}`"
            )));
        }
    };
    let input_schema: Value = lua.from_value(table.get("input")?)?;
    let output_schema: Value = lua.from_value(table.get("output")?)?;
    validate_model_facing_schema(&input_schema).map_err(mlua::Error::external)?;
    validate_model_facing_schema(&output_schema).map_err(mlua::Error::external)?;
    let instruction = table.get::<String>("instruction")?;
    if instruction.trim().is_empty() {
        return Err(mlua::Error::external(
            "workflow.agent instruction must not be empty",
        ));
    }
    if !matches!(table.get::<LuaValue>("capabilities")?, LuaValue::Nil) {
        return Err(mlua::Error::external(
            "workflow.agent `capabilities` has been removed; the host automatically provides allowed App tools",
        ));
    }
    let extra_tools = string_list_from_lua(table.get::<Option<Table>>("extra_tools")?)?;
    Ok(WorkerDefinition {
        model,
        input_schema,
        output_schema,
        instruction,
        extra_tools,
    })
}

fn worker_definition_to_lua(lua: &Lua, definition: &WorkerDefinition) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set(
        "model",
        match definition.model {
            WorkerModel::Main => "main",
            WorkerModel::Efficient => "efficient",
        },
    )?;
    table.set("input", lua.to_value(&definition.input_schema)?)?;
    table.set("output", lua.to_value(&definition.output_schema)?)?;
    table.set("instruction", definition.instruction.clone())?;
    let extra_tools = lua.create_table()?;
    for local_tool in &definition.extra_tools {
        extra_tools.push(local_tool.clone())?;
    }
    table.set("extra_tools", extra_tools)?;
    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker_schema() -> Value {
        json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false,
        })
    }

    fn worker_table(lua: &Lua) -> Table {
        let table = lua.create_table().expect("create worker table");
        let schema = lua
            .to_value(&worker_schema())
            .expect("serialize worker schema");
        table
            .set("input", schema.clone())
            .expect("set worker input");
        table.set("output", schema).expect("set worker output");
        table
            .set("instruction", "Return the required output.")
            .expect("set worker instruction");
        table
    }

    #[test]
    fn worker_definition_rejects_removed_capabilities() {
        let lua = new_lua().expect("create Lua runtime");
        let table = worker_table(&lua);
        let capabilities = lua.create_table().expect("create capabilities table");
        capabilities
            .push("browser__browser_snapshot")
            .expect("add capability");
        table
            .set("capabilities", capabilities)
            .expect("set capabilities");

        let error = worker_definition_from_lua(&lua, table)
            .expect_err("removed capabilities should be rejected");

        assert!(
            error
                .to_string()
                .contains("`capabilities` has been removed")
        );
    }

    #[test]
    fn worker_definition_accepts_automatic_app_tools_without_capabilities() {
        let lua = new_lua().expect("create Lua runtime");
        let table = worker_table(&lua);

        let definition = worker_definition_from_lua(&lua, table)
            .expect("worker without capabilities should load");

        assert!(definition.extra_tools.is_empty());
    }
}

fn local_tool_definition_from_lua(lua: &Lua, table: Table) -> mlua::Result<LocalToolDefinition> {
    let name = table.get::<String>("name")?;
    if !is_valid_workflow_name(&name) {
        return Err(mlua::Error::external(
            "workflow.tool name must use lowercase letters, digits, and single underscores",
        ));
    }
    let input_schema: Value = lua.from_value(table.get("input")?)?;
    let output_schema: Value = lua.from_value(table.get("output")?)?;
    validate_model_facing_schema(&input_schema).map_err(mlua::Error::external)?;
    validate_model_facing_schema(&output_schema).map_err(mlua::Error::external)?;
    let run = table
        .get::<Function>("run")
        .map_err(|_| mlua::Error::external("workflow.tool requires a run function"))?;
    Ok(LocalToolDefinition {
        name,
        input_schema,
        output_schema,
        run,
        lua: lua.clone(),
    })
}

fn string_list_from_lua(table: Option<Table>) -> mlua::Result<Vec<String>> {
    let Some(table) = table else {
        return Ok(Vec::new());
    };
    let mut values = BTreeSet::new();
    for item in table.sequence_values::<String>() {
        let value = item?;
        if value.trim().is_empty() {
            return Err(mlua::Error::external(
                "workflow local-tool names must not be empty",
            ));
        }
        values.insert(value);
    }
    Ok(values.into_iter().collect())
}

fn is_valid_workflow_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_lowercase())
        && !name.ends_with('_')
        && !name.contains("__")
        && chars.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
}

fn new_lua() -> mlua::Result<Lua> {
    let libraries =
        StdLib::COROUTINE | StdLib::TABLE | StdLib::STRING | StdLib::UTF8 | StdLib::MATH;
    Lua::new_with(libraries, LuaOptions::default())
}

fn install_sandboxed_lua_environment(lua: &Lua, context: &Context) -> mlua::Result<()> {
    let execution_cwd = context.execution_cwd.clone();
    let sandbox_policy = context.sandbox_policy.clone();
    let io_table = lua.create_table()?;
    let open_cwd = execution_cwd.clone();
    let open_policy = sandbox_policy.clone();
    io_table.set(
        "open",
        lua.create_function(move |lua, (path, mode): (String, Option<String>)| {
            let mode = mode.unwrap_or_else(|| "r".to_string());
            let resolved = open_policy.resolve_path(Path::new(&path), Some(&open_cwd));
            let writes = mode.contains('w') || mode.contains('a') || mode.contains('+');
            if writes {
                open_policy
                    .ensure_path_writable(&resolved, "workflow io.open target")
                    .map_err(mlua::Error::external)?;
            } else {
                open_policy
                    .ensure_path_readable(&resolved, "workflow io.open target")
                    .map_err(mlua::Error::external)?;
            }
            let file = match mode.as_str() {
                "r" | "rb" => fs::File::open(&resolved),
                "w" | "wb" => fs::File::create(&resolved),
                "a" | "ab" => std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&resolved),
                _ => {
                    return Err(mlua::Error::external(
                        "workflow io.open supports only r, rb, w, wb, a, or ab modes",
                    ));
                }
            }
            .map_err(mlua::Error::external)?;
            let userdata = lua.create_userdata(WorkflowLuaFile {
                file: Arc::new(Mutex::new(file)),
                readable: !writes,
                writable: writes,
            })?;
            Ok(LuaValue::UserData(userdata))
        })?,
    )?;
    let popen_cwd = execution_cwd.clone();
    let popen_policy = sandbox_policy.clone();
    io_table.set(
        "popen",
        lua.create_function(move |lua, command: String| {
            let output =
                run_sandboxed_shell(&popen_policy, &popen_cwd, &command, LUA_SHELL_MAX_BYTES)
                    .map_err(mlua::Error::external)?;
            let userdata = lua.create_userdata(WorkflowLuaPipe {
                output: Arc::new(Mutex::new(Some(output))),
            })?;
            Ok(LuaValue::UserData(userdata))
        })?,
    )?;
    lua.globals().set("io", io_table)?;

    let os_table = lua.create_table()?;
    let execute_cwd = execution_cwd;
    let execute_policy = sandbox_policy;
    os_table.set(
        "execute",
        lua.create_function(move |_, command: String| {
            let output =
                run_sandboxed_shell(&execute_policy, &execute_cwd, &command, LUA_SHELL_MAX_BYTES)
                    .map_err(mlua::Error::external)?;
            Ok((
                output.status.success(),
                "exit".to_string(),
                output.status.code().unwrap_or(-1),
            ))
        })?,
    )?;
    lua.globals().set("os", os_table)?;
    Ok(())
}

#[derive(Clone)]
struct WorkflowLuaFile {
    file: Arc<Mutex<fs::File>>,
    readable: bool,
    writable: bool,
}

impl mlua::UserData for WorkflowLuaFile {
    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method_mut("read", |_, this, _: ()| {
            if !this.readable {
                return Err(mlua::Error::external("workflow file is not readable"));
            }
            let mut file = this
                .file
                .lock()
                .map_err(|_| mlua::Error::external("workflow file lock poisoned"))?;
            let mut bytes = Vec::new();
            use std::io::Read;
            file.read_to_end(&mut bytes)
                .map_err(mlua::Error::external)?;
            if bytes.len() > LUA_IO_MAX_BYTES {
                return Err(mlua::Error::external(
                    "workflow io.read exceeded the output limit",
                ));
            }
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        });
        methods.add_method_mut("write", |_, this, data: String| {
            if !this.writable {
                return Err(mlua::Error::external("workflow file is not writable"));
            }
            use std::io::Write;
            this.file
                .lock()
                .map_err(|_| mlua::Error::external("workflow file lock poisoned"))?
                .write_all(data.as_bytes())
                .map_err(mlua::Error::external)?;
            Ok(())
        });
        methods.add_method("close", |_, _, _: ()| Ok(true));
    }
}

#[derive(Clone)]
struct WorkflowLuaPipe {
    output: Arc<Mutex<Option<WorkflowShellOutput>>>,
}

impl mlua::UserData for WorkflowLuaPipe {
    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method_mut("read", |_, this, _: ()| {
            let output = this
                .output
                .lock()
                .map_err(|_| mlua::Error::external("workflow shell output lock poisoned"))?
                .take()
                .unwrap_or_default();
            Ok(output.text)
        });
        methods.add_method_mut("close", |_, this, _: ()| {
            let output = this
                .output
                .lock()
                .map_err(|_| mlua::Error::external("workflow shell output lock poisoned"))?
                .take()
                .unwrap_or_default();
            Ok((
                output.status.success(),
                "exit".to_string(),
                output.status.code().unwrap_or(-1),
            ))
        });
    }
}

struct WorkflowShellOutput {
    text: String,
    status: std::process::ExitStatus,
}

impl Default for WorkflowShellOutput {
    fn default() -> Self {
        Self {
            text: String::new(),
            status: successful_exit_status(),
        }
    }
}

fn run_sandboxed_shell(
    policy: &crate::sandbox::RuntimeSandboxPolicy,
    execution_cwd: &Path,
    command: &str,
    max_bytes: usize,
) -> Result<WorkflowShellOutput> {
    let (program, args) = workflow_shell_invocation(command);
    let runtime = tokio::runtime::Handle::try_current()
        .map_err(|err| miette!("workflow shell requires an active Tokio runtime: {err}"))?;
    tokio::task::block_in_place(|| {
        runtime.block_on(async {
            let mut child = SandboxAsyncChild::spawn_shell(
                policy,
                program,
                args,
                SandboxProcessOptions {
                    current_dir: Some(execution_cwd.to_path_buf()),
                    stdin: SandboxStdio::Null,
                    stdout: SandboxStdio::Piped,
                    stderr: SandboxStdio::Piped,
                },
            )
            .map_err(|err| miette!("failed to start workflow shell command: {err}"))?;
            let mut stdout = child
                .take_stdout()
                .ok_or_else(|| miette!("workflow shell command did not provide stdout"))?;
            let mut stderr = child
                .take_stderr()
                .ok_or_else(|| miette!("workflow shell command did not provide stderr"))?;
            let stdout_task = tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut bytes = Vec::new();
                stdout.read_to_end(&mut bytes).await.map(|_| bytes)
            });
            let stderr_task = tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut bytes = Vec::new();
                stderr.read_to_end(&mut bytes).await.map(|_| bytes)
            });
            let status = loop {
                if let Some(status) = child
                    .try_wait()
                    .map_err(|err| miette!("failed to wait for workflow shell command: {err}"))?
                {
                    break status;
                }
            };
            let stdout = stdout_task
                .await
                .map_err(|err| miette!("workflow shell stdout task failed: {err}"))?
                .map_err(|err| miette!("failed to read workflow shell stdout: {err}"))?;
            let stderr = stderr_task
                .await
                .map_err(|err| miette!("workflow shell stderr task failed: {err}"))?
                .map_err(|err| miette!("failed to read workflow shell stderr: {err}"))?;
            if stdout.len().saturating_add(stderr.len()) > max_bytes {
                return Err(miette!(
                    "workflow shell output exceeded the {} byte limit",
                    max_bytes
                ));
            }
            let mut text = String::from_utf8_lossy(&stdout).into_owned();
            if !stderr.is_empty() {
                text.push_str(&String::from_utf8_lossy(&stderr));
            }
            Ok(WorkflowShellOutput { text, status })
        })
    })
}

fn workflow_shell_invocation(command: &str) -> (&'static str, Vec<String>) {
    if cfg!(windows) {
        (
            "powershell.exe",
            vec![
                "-NoLogo".to_string(),
                "-NoProfile".to_string(),
                "-Command".to_string(),
                command.to_string(),
            ],
        )
    } else {
        ("bash", vec!["-lc".to_string(), command.to_string()])
    }
}

#[cfg(unix)]
fn successful_exit_status() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(0)
}

#[cfg(windows)]
fn successful_exit_status() -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(0)
}

fn ensure_workflow_not_interrupted(cancellation: Option<&WorkflowCancellation>) -> Result<()> {
    if cancellation.is_some_and(WorkflowCancellation::is_interrupted) {
        Err(miette!(WorkflowInterrupted))
    } else {
        Ok(())
    }
}

fn is_workflow_interrupted_error(error: &miette::Report) -> bool {
    error.to_string().contains(WORKFLOW_INTERRUPTED_ERROR)
}

fn install_workflow_interrupt_hook(
    lua: &Lua,
    cancellation: Option<&WorkflowCancellation>,
) -> mlua::Result<()> {
    let Some(cancellation) = cancellation.cloned() else {
        return Ok(());
    };
    lua.set_global_hook(
        HookTriggers {
            every_nth_instruction: Some(WORKFLOW_LUA_INTERRUPT_INSTRUCTION_INTERVAL),
            ..HookTriggers::default()
        },
        move |_, _| {
            if cancellation.is_interrupted() {
                Err(mlua::Error::external(WorkflowInterrupted))
            } else {
                Ok(mlua::VmState::Continue)
            }
        },
    )
}

fn lua_error(err: mlua::Error) -> miette::Report {
    miette!("Lua workflow error: {err}")
}
