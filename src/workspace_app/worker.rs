use std::{
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    path::{Path, PathBuf},
};

use miette::{Result, miette};
use mlua::{Function, Lua, LuaSerdeExt, Table, Value as LuaValue};

use crate::{
    app::{AppDynamicToolResult, AppDynamicToolSpec, AppId, AppStateRender},
    workspace_app::{
        WorkspaceAppConfigOutput, WorkspaceAppRuntimeState, WorkspaceLuaRuntime,
        WorkspaceNoticeOutput, WorkspaceRenderOutput, WorkspaceToolCallOutput,
        WorkspaceToolDescriptor, load_runtime_state, load_workspace_lua_runtime,
        normalize_workspace_input_schema,
        protocol::{
            WorkerHello, WorkerRequest, WorkerRequestOp, WorkerResponse, WorkerResponsePayload,
            WorkerResponseResult,
        },
        validate_workspace_tool_schema, validate_workspace_tool_value,
    },
};

pub(crate) struct WorkspaceAppWorkerArgs {
    pub app_id: String,
    pub app_dir: PathBuf,
    pub state_dir: PathBuf,
    pub entry: String,
    pub connect_addr: String,
    pub token: String,
}

pub(crate) fn run_workspace_app_worker(args: WorkspaceAppWorkerArgs) -> Result<()> {
    let app_id = AppId::from_workspace_folder(args.app_id)?;
    let app_id_label = app_id.to_string();
    let stream = TcpStream::connect(&args.connect_addr).map_err(|err| {
        miette!(
            "workspace app worker `{app_id}` failed to connect to {}: {err}",
            args.connect_addr
        )
    })?;
    let mut writer = stream.try_clone().map_err(|err| {
        miette!("workspace app worker `{app_id}` failed to clone IPC stream: {err}")
    })?;
    let mut reader = BufReader::new(stream);

    write_json_line(
        &mut writer,
        &WorkerHello {
            token: args.token,
            app_id: app_id.to_string(),
        },
    )?;
    let mut runtime = LuaWorkerHost::new(app_id, args.app_dir, args.state_dir, args.entry);

    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line).map_err(|err| {
            miette!("workspace app worker `{app_id_label}` failed to read IPC request: {err}")
        })?;
        if bytes == 0 {
            return Ok(());
        }
        let request = serde_json::from_str::<WorkerRequest>(&line).map_err(|err| {
            miette!("workspace app worker `{app_id_label}` received invalid IPC request: {err}")
        })?;
        let shutdown = matches!(request.op, WorkerRequestOp::Shutdown);
        let response = runtime.handle_request(request);
        write_json_line(&mut writer, &response)?;
        if shutdown {
            return Ok(());
        }
    }
}

fn write_json_line<T: serde::Serialize>(writer: &mut TcpStream, value: &T) -> Result<()> {
    serde_json::to_writer(&mut *writer, value)
        .map_err(|err| miette!("failed to encode workspace app worker IPC message: {err}"))?;
    writer
        .write_all(b"\n")
        .map_err(|err| miette!("failed to write workspace app worker IPC message: {err}"))?;
    writer
        .flush()
        .map_err(|err| miette!("failed to flush workspace app worker IPC message: {err}"))?;
    Ok(())
}

struct LuaWorkerRuntime {
    id: AppId,
    app_dir: PathBuf,
    state_dir: PathBuf,
    state_file: PathBuf,
    entry_relative_path: String,
    lua_runtime: WorkspaceLuaRuntime,
    runtime: WorkspaceAppRuntimeState,
    init_ran: bool,
}

struct LuaWorkerHost {
    id: AppId,
    app_dir: PathBuf,
    state_dir: PathBuf,
    entry_relative_path: String,
    runtime: Option<LuaWorkerRuntime>,
}

impl LuaWorkerHost {
    fn new(id: AppId, app_dir: PathBuf, state_dir: PathBuf, entry_relative_path: String) -> Self {
        Self {
            id,
            app_dir,
            state_dir,
            entry_relative_path,
            runtime: None,
        }
    }

    fn handle_request(&mut self, request: WorkerRequest) -> WorkerResponse {
        let id = request.id;
        let result = match request.op {
            WorkerRequestOp::Configure => self.configure().map(WorkerResponsePayload::Config),
            WorkerRequestOp::Initialize => self.initialize().map(|()| WorkerResponsePayload::Unit),
            WorkerRequestOp::Shutdown => Ok(WorkerResponsePayload::Unit),
            op => self.runtime_mut().and_then(|runtime| runtime.handle_op(op)),
        };
        WorkerResponse {
            id,
            result: match result {
                Ok(payload) => WorkerResponseResult::Ok { payload },
                Err(err) => WorkerResponseResult::Err {
                    message: err.to_string(),
                },
            },
        }
    }

    fn configure(&mut self) -> Result<WorkspaceAppConfigOutput> {
        if self.runtime.is_none() {
            self.runtime = Some(LuaWorkerRuntime::load(
                self.id.clone(),
                self.app_dir.clone(),
                self.state_dir.clone(),
                self.entry_relative_path.clone(),
            )?);
        }
        self.runtime
            .as_ref()
            .expect("runtime should exist after load")
            .run_config()
    }

    fn initialize(&mut self) -> Result<()> {
        if self.runtime.is_none() {
            self.runtime = Some(LuaWorkerRuntime::load(
                self.id.clone(),
                self.app_dir.clone(),
                self.state_dir.clone(),
                self.entry_relative_path.clone(),
            )?);
        }
        self.runtime
            .as_mut()
            .expect("runtime should exist after load")
            .run_init()
    }

    fn runtime_mut(&mut self) -> Result<&mut LuaWorkerRuntime> {
        self.runtime.as_mut().ok_or_else(|| {
            miette!(
                "workspace app `{}` worker received request before initialization",
                self.id
            )
        })
    }
}

impl LuaWorkerRuntime {
    fn load(
        id: AppId,
        app_dir: PathBuf,
        state_dir: PathBuf,
        entry_relative_path: String,
    ) -> Result<Self> {
        let entry_path = super::resolve_relative_child_path(&app_dir, &entry_relative_path)?;
        let entry_source = std::fs::read_to_string(&entry_path).map_err(|err| {
            miette!(
                "failed to read lua entry {} for app `{id}`: {err}",
                entry_path.display()
            )
        })?;
        let lua_runtime =
            load_workspace_lua_runtime(&id, &app_dir, &entry_relative_path, &entry_source)?;
        let state_file = state_dir.join("state.json");
        let runtime = load_runtime_state(&state_file)
            .map_err(|err| miette!("failed to load state for app `{id}`: {err:?}"))?;
        let worker = Self {
            id,
            app_dir,
            state_dir,
            state_file,
            entry_relative_path,
            lua_runtime,
            runtime,
            init_ran: false,
        };
        Ok(worker)
    }

    fn handle_op(&mut self, op: WorkerRequestOp) -> Result<WorkerResponsePayload> {
        match op {
            WorkerRequestOp::Configure => self.run_config().map(WorkerResponsePayload::Config),
            WorkerRequestOp::Initialize => Err(miette!(
                "workspace app `{}` worker is already initialized",
                self.id
            )),
            WorkerRequestOp::RenderState => {
                self.render_state().map(WorkerResponsePayload::RenderState)
            }
            WorkerRequestOp::ListTools => self.list_tools().map(WorkerResponsePayload::ToolSpecs),
            WorkerRequestOp::CallTool { name, arguments } => self
                .call_tool(&name, arguments)
                .map(WorkerResponsePayload::ToolResult),
            WorkerRequestOp::OnFocus => self.on_focus().map(|()| WorkerResponsePayload::Unit),
            WorkerRequestOp::OnBlur => self.on_blur().map(|()| WorkerResponsePayload::Unit),
            WorkerRequestOp::PollNotices => self.poll_notices().map(WorkerResponsePayload::Notice),
            WorkerRequestOp::Shutdown => Ok(WorkerResponsePayload::Unit),
        }
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

    fn run_config(&self) -> Result<WorkspaceAppConfigOutput> {
        let module = &self.lua_runtime.module;
        let lua = &self.lua_runtime.lua;
        let ctx = self.lua_context(lua)?;
        let config_fn = self.map_lua(
            "failed to resolve `config`",
            module.get::<Option<Function>>("config"),
        )?;
        let Some(config_fn) = config_fn else {
            return Ok(WorkspaceAppConfigOutput::default());
        };
        let value = self.map_lua(
            "failed to execute `config`",
            config_fn.call::<LuaValue>(ctx),
        )?;
        match value {
            LuaValue::Nil => Ok(WorkspaceAppConfigOutput::default()),
            value => self.map_lua(
                "failed to decode `config` result",
                lua.from_value::<WorkspaceAppConfigOutput>(value),
            ),
        }
    }

    fn run_init(&mut self) -> Result<()> {
        if self.init_ran {
            return Ok(());
        }
        std::fs::create_dir_all(&self.state_dir).map_err(|err| {
            miette!(
                "failed to create app state directory {} before init: {err}",
                self.state_dir.display()
            )
        })?;
        let module = &self.lua_runtime.module;
        let lua = &self.lua_runtime.lua;
        let ctx = self.lua_context(lua)?;
        let init_fn = self.map_lua(
            "failed to resolve `init`",
            module.get::<Option<Function>>("init"),
        )?;
        if let Some(init_fn) = init_fn {
            let state_value = self.map_lua(
                "failed to encode runtime state for `init`",
                lua.to_value(&self.runtime.state),
            )?;
            let result = self.map_lua(
                "failed to execute `init`",
                init_fn.call::<LuaValue>((ctx.clone(), state_value)),
            )?;
            if !matches!(result, LuaValue::Nil) {
                self.runtime.state =
                    self.map_lua("failed to decode `init` result", lua.from_value(result))?;
                self.persist_runtime_state()?;
            }
        }
        self.init_ran = true;
        Ok(())
    }

    fn render_state(&mut self) -> Result<AppStateRender> {
        let lua = &self.lua_runtime.lua;
        let module = &self.lua_runtime.module;
        let ctx = self.lua_context(lua)?;
        let Some(render_fn) = self.map_lua(
            "failed to resolve `render_state`",
            module.get::<Option<Function>>("render_state"),
        )?
        else {
            return Ok(self.default_render_state());
        };
        let state_value = self.map_lua(
            "failed to encode runtime state for `render_state`",
            lua.to_value(&self.runtime.state),
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
            self.runtime.state = next_state;
            self.persist_runtime_state()?;
        }
        let mut lines = output.lines;
        if !lines.iter().any(|line| line.starts_with("kind=")) {
            lines.insert(0, "kind=workspace_app".to_string());
        }
        Ok(AppStateRender {
            title: output.title.unwrap_or_else(|| self.id.to_string()),
            lines,
        })
    }

    fn list_tools(&mut self) -> Result<Vec<AppDynamicToolSpec>> {
        Ok(self
            .load_tool_descriptors()?
            .into_iter()
            .map(|descriptor| AppDynamicToolSpec {
                name: descriptor.name,
                description: descriptor.description,
                input_schema: descriptor.input_schema,
            })
            .collect())
    }

    fn load_tool_descriptors(&self) -> Result<Vec<WorkspaceToolDescriptor>> {
        let lua = &self.lua_runtime.lua;
        let module = &self.lua_runtime.module;
        let ctx = self.lua_context(lua)?;
        let Some(list_tools_fn) = self.map_lua(
            "failed to resolve `list_tools`",
            module.get::<Option<Function>>("list_tools"),
        )?
        else {
            return Ok(Vec::new());
        };
        let state_value = self.map_lua(
            "failed to encode runtime state for `list_tools`",
            lua.to_value(&self.runtime.state),
        )?;
        let value = self.map_lua(
            "failed to execute `list_tools`",
            list_tools_fn.call::<LuaValue>((ctx, state_value)),
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

    fn call_tool(
        &mut self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<AppDynamicToolResult> {
        let descriptors = self.load_tool_descriptors()?;
        let descriptor = descriptors
            .into_iter()
            .find(|descriptor| descriptor.name == name)
            .ok_or_else(|| miette!("workspace app `{}` does not declare tool `{name}`", self.id))?;
        validate_workspace_tool_value(
            &arguments,
            &descriptor.input_schema,
            &format!("arguments for workspace app tool `{}`", descriptor.name),
        )?;
        let lua = &self.lua_runtime.lua;
        let module = &self.lua_runtime.module;
        let ctx = self.lua_context(lua)?;
        let call_tool_fn = self
            .map_lua(
                "failed to resolve `call_tool`",
                module.get::<Option<Function>>("call_tool"),
            )?
            .ok_or_else(|| miette!("workspace app `{}` does not define `call_tool`", self.id))?;
        let state_value = self.map_lua(
            "failed to encode runtime state for `call_tool`",
            lua.to_value(&self.runtime.state),
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
            self.runtime.state = next_state;
            self.persist_runtime_state()?;
        }
        Ok(AppDynamicToolResult {
            summary: output.summary,
            payload: output.payload,
            model_content: output.model_content,
            ui_lines: output.ui_lines,
            turn_boundary_reason: output.turn_boundary,
        })
    }

    fn on_focus(&mut self) -> Result<()> {
        self.run_state_hook("on_focus")
    }

    fn on_blur(&mut self) -> Result<()> {
        self.run_state_hook("on_blur")
    }

    fn run_state_hook(&mut self, name: &str) -> Result<()> {
        let lua = &self.lua_runtime.lua;
        let module = &self.lua_runtime.module;
        let ctx = self.lua_context(lua)?;
        let Some(hook_fn) = self.map_lua(
            &format!("failed to resolve `{name}`"),
            module.get::<Option<Function>>(name),
        )?
        else {
            return Ok(());
        };
        let state_value = self.map_lua(
            &format!("failed to encode runtime state for `{name}`"),
            lua.to_value(&self.runtime.state),
        )?;
        let value = self.map_lua(
            &format!("failed to execute `{name}`"),
            hook_fn.call::<LuaValue>((ctx, state_value)),
        )?;
        if !matches!(value, LuaValue::Nil) {
            self.runtime.state = self.map_lua(
                &format!("failed to decode `{name}` result"),
                lua.from_value(value),
            )?;
            self.persist_runtime_state()?;
        }
        Ok(())
    }

    fn poll_notices(&mut self) -> Result<Option<String>> {
        let lua = &self.lua_runtime.lua;
        let module = &self.lua_runtime.module;
        let ctx = self.lua_context(lua)?;
        let Some(poll_notices_fn) = self.map_lua(
            "failed to resolve `poll_notices`",
            module.get::<Option<Function>>("poll_notices"),
        )?
        else {
            self.runtime.notice_reason = None;
            return Ok(None);
        };
        let state_value = self.map_lua(
            "failed to encode runtime state for `poll_notices`",
            lua.to_value(&self.runtime.state),
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
            self.runtime.state = next_state;
            self.persist_runtime_state()?;
        }
        self.runtime.notice_reason = summarize_notices(&output.notices);
        Ok(self.runtime.notice_reason.clone())
    }

    fn persist_runtime_state(&self) -> Result<()> {
        if let Some(parent) = self.state_file.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                miette!(
                    "failed to create app state directory {}: {err}",
                    parent.display()
                )
            })?;
        }
        let content = serde_json::to_vec_pretty(&self.runtime.state)
            .map_err(|err| miette!("failed to encode app state for `{}`: {err}", self.id))?;
        std::fs::write(&self.state_file, content).map_err(|err| {
            miette!(
                "failed to write app state {}: {err}",
                self.state_file.display()
            )
        })?;
        Ok(())
    }

    fn default_render_state(&self) -> AppStateRender {
        AppStateRender {
            title: self.id.to_string(),
            lines: vec![
                "kind=workspace_app".to_string(),
                format!("entry={}", self.entry_relative_path),
                format!("source_dir={}", self.app_dir.display()),
            ],
        }
    }
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

fn _assert_paths_are_normal(path: &Path) -> bool {
    path.components()
        .all(|component| !matches!(component, std::path::Component::ParentDir))
}
