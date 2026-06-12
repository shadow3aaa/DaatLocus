use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    emit_build_target();
    build_embedded_webui(&manifest_dir);
    write_prompt_bindings(&manifest_dir);
    write_builtin_primitive_bindings(&manifest_dir);
}

fn emit_build_target() {
    let target = env::var("TARGET").expect("target triple");
    println!("cargo:rustc-env=DAAT_LOCUS_BUILD_TARGET={target}");
}

fn build_embedded_webui(manifest_dir: &Path) {
    let webui_dir = manifest_dir.join("webui");
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("out dir"));
    let webui_work = out_dir.join("webui-work");
    let webui_dist = out_dir.join("webui-dist");
    println!("cargo:rerun-if-env-changed=DAAT_LOCUS_WEBUI_PM");
    emit_webui_rerun_inputs(&webui_dir);

    if !webui_dir.join("package.json").is_file() {
        panic!("WebUI package.json not found: {}", webui_dir.display());
    }

    prepare_webui_worktree(&webui_dir, &webui_work);
    let package_manager = webui_package_manager();
    run_webui_command(&package_manager, &["install", "--immutable"], &webui_work);
    run_webui_build_command(&package_manager, &webui_work, &webui_dist);

    let dist_index = webui_dist.join("index.html");
    if !dist_index.is_file() {
        panic!(
            "WebUI build did not produce required entry {}",
            dist_index.display()
        );
    }
}

fn emit_webui_rerun_inputs(webui_dir: &Path) {
    for path in [
        webui_dir.join(".yarnrc.yml"),
        webui_dir.join("index.html"),
        webui_dir.join("package.json"),
        webui_dir.join("tailwind.config.cjs"),
        webui_dir.join("tsconfig.json"),
        webui_dir.join("vite.config.ts"),
        webui_dir.join("yarn.lock"),
    ] {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    emit_rerun_if_changed_recursively(&webui_dir.join("src"));
}

fn prepare_webui_worktree(source_dir: &Path, work_dir: &Path) {
    if work_dir.exists() {
        fs::remove_dir_all(work_dir).unwrap_or_else(|err| {
            panic!(
                "failed to remove WebUI work dir {}: {err}",
                work_dir.display()
            )
        });
    }
    fs::create_dir_all(work_dir).unwrap_or_else(|err| {
        panic!(
            "failed to create WebUI work dir {}: {err}",
            work_dir.display()
        )
    });

    for relative in [
        ".yarnrc.yml",
        "index.html",
        "package.json",
        "tailwind.config.cjs",
        "tsconfig.json",
        "vite.config.ts",
        "yarn.lock",
    ] {
        copy_webui_file(source_dir, work_dir, relative);
    }
    copy_webui_dir(&source_dir.join("src"), &work_dir.join("src"));
}

fn copy_webui_file(source_dir: &Path, work_dir: &Path, relative: &str) {
    let source = source_dir.join(relative);
    let destination = work_dir.join(relative);
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|err| {
            panic!(
                "failed to create WebUI work dir {}: {err}",
                parent.display()
            )
        });
    }
    fs::copy(&source, &destination).unwrap_or_else(|err| {
        panic!(
            "failed to copy WebUI input {} to {}: {err}",
            source.display(),
            destination.display()
        )
    });
}

fn copy_webui_dir(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap_or_else(|err| {
        panic!(
            "failed to create WebUI work dir {}: {err}",
            destination.display()
        )
    });

    let mut entries = fs::read_dir(source)
        .unwrap_or_else(|err| panic!("failed to read WebUI input dir {}: {err}", source.display()))
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect::<Vec<_>>();
    entries.sort();

    for entry in entries {
        let destination_entry = destination.join(entry.file_name().expect("WebUI input file name"));
        if entry.is_dir() {
            copy_webui_dir(&entry, &destination_entry);
        } else {
            fs::copy(&entry, &destination_entry).unwrap_or_else(|err| {
                panic!(
                    "failed to copy WebUI input {} to {}: {err}",
                    entry.display(),
                    destination_entry.display()
                )
            });
        }
    }
}

fn emit_rerun_if_changed_recursively(path: &Path) {
    println!("cargo:rerun-if-changed={}", path.display());
    if !path.is_dir() {
        return;
    }

    let mut entries = fs::read_dir(path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect::<Vec<_>>();
    entries.sort();
    for entry in entries {
        emit_rerun_if_changed_recursively(&entry);
    }
}

fn webui_package_manager() -> Vec<String> {
    if let Ok(value) = env::var("DAAT_LOCUS_WEBUI_PM") {
        let parts = value
            .split_whitespace()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        if !parts.is_empty() {
            return parts;
        }
    }

    default_webui_package_manager()
}

#[cfg(windows)]
fn default_webui_package_manager() -> Vec<String> {
    if resolve_command("corepack").is_some() {
        return vec![
            "cmd".to_string(),
            "/C".to_string(),
            "corepack".to_string(),
            "yarn".to_string(),
        ];
    }
    vec!["cmd".to_string(), "/C".to_string(), "yarn".to_string()]
}

#[cfg(not(windows))]
fn default_webui_package_manager() -> Vec<String> {
    if resolve_command("corepack").is_some() {
        return vec!["corepack".to_string(), "yarn".to_string()];
    }
    vec!["yarn".to_string()]
}

fn run_webui_build_command(package_manager: &[String], webui_dir: &Path, out_dir: &Path) {
    let mut command = Command::new(&package_manager[0]);
    command
        .args(&package_manager[1..])
        .arg("build")
        .env("DAAT_LOCUS_WEBUI_OUT_DIR", out_dir)
        .current_dir(webui_dir);
    configure_webui_command(&mut command);
    run_webui_command_status(command, "WebUI build");
}

fn run_webui_command(package_manager: &[String], args: &[&str], webui_dir: &Path) {
    let mut command = Command::new(&package_manager[0]);
    command
        .args(&package_manager[1..])
        .args(args)
        .current_dir(webui_dir);
    configure_webui_command(&mut command);
    run_webui_command_status(command, "WebUI command");
}

#[cfg(windows)]
fn configure_webui_command(command: &mut Command) {
    command.env("YARN_NODE_LINKER", "node-modules");
}

#[cfg(not(windows))]
fn configure_webui_command(_command: &mut Command) {}

fn run_webui_command_status(mut command: Command, label: &str) {
    let status = command
        .status()
        .unwrap_or_else(|err| panic!("failed to run {label} {:?}: {err}", command));
    if !status.success() {
        panic!("{label} {:?} failed with status {status}", command);
    }
}

fn resolve_command(command: &str) -> Option<PathBuf> {
    let command_path = Path::new(command);
    if command_path.components().count() > 1 {
        return executable_candidate(command_path);
    }
    env::var_os("PATH").and_then(|path| {
        env::split_paths(&path).find_map(|dir| {
            let candidate = dir.join(command);
            executable_candidate(&candidate)
        })
    })
}

fn executable_candidate(candidate: &Path) -> Option<PathBuf> {
    if candidate.is_file() {
        return Some(candidate.to_path_buf());
    }

    #[cfg(windows)]
    {
        let pathext = env::var_os("PATHEXT").unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into());
        pathext.to_string_lossy().split(';').find_map(|extension| {
            let extension = extension.trim().trim_start_matches('.');
            if extension.is_empty() {
                return None;
            }
            let candidate = candidate.with_extension(extension);
            candidate.is_file().then_some(candidate)
        })
    }

    #[cfg(not(windows))]
    {
        None
    }
}

struct PromptBinding {
    prompt_id: String,
    const_name: String,
    source_const_name: String,
    content: String,
    kind: PromptBindingKind,
}

enum PromptBindingKind {
    Raw,
    App(AppPromptBinding),
    Persona(PersonaPromptBinding),
}

struct AppPromptBinding {
    description: String,
    when_to_focus: Vec<String>,
    how_to_use: String,
}

struct PersonaPromptBinding {
    name: String,
    language: String,
    identity_summary: String,
}

fn write_prompt_bindings(manifest_dir: &Path) {
    let prompts_dir = manifest_dir.join("prompts");
    println!("cargo:rerun-if-changed={}", prompts_dir.display());
    if !prompts_dir.is_dir() {
        panic!("prompt directory not found: {}", prompts_dir.display());
    }

    let mut prompt_files = Vec::<PathBuf>::new();
    collect_prompt_markdown_files(&prompts_dir, &mut prompt_files);
    prompt_files.sort();
    if prompt_files.is_empty() {
        panic!(
            "prompt directory contains no markdown files: {}",
            prompts_dir.display()
        );
    }

    let mut const_names = BTreeSet::new();
    let mut prompts = Vec::<PromptBinding>::new();
    for path in prompt_files {
        println!("cargo:rerun-if-changed={}", path.display());
        let relative = path
            .strip_prefix(&prompts_dir)
            .expect("prompt file under prompt dir");
        let prompt_id = prompt_id_from_relative_path(relative);
        let const_name = prompt_const_name_from_relative_path(relative);
        if !const_names.insert(const_name.clone()) {
            panic!("duplicate generated prompt constant name {const_name}");
        }
        let content = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read prompt {}: {err}", path.display()));
        let content = trim_trailing_line_endings(&content).to_string();
        let kind = if is_app_prompt_file(relative) {
            PromptBindingKind::App(parse_app_prompt_binding(&path, &content))
        } else if is_persona_prompt_file(relative) {
            PromptBindingKind::Persona(parse_persona_prompt_binding(&path, &content))
        } else {
            PromptBindingKind::Raw
        };
        let source_const_name = if matches!(
            &kind,
            PromptBindingKind::App(_) | PromptBindingKind::Persona(_)
        ) {
            let source_const_name = format!("{const_name}_SOURCE");
            if !const_names.insert(source_const_name.clone()) {
                panic!("duplicate generated prompt constant name {source_const_name}");
            }
            source_const_name
        } else {
            const_name.clone()
        };
        prompts.push(PromptBinding {
            prompt_id,
            const_name,
            source_const_name,
            content,
            kind,
        });
    }

    let out_path = PathBuf::from(env::var("OUT_DIR").expect("out dir")).join("prompt_bindings.rs");
    let mut code = String::from("// @generated by build.rs. Do not edit by hand.\n\n");
    for prompt in &prompts {
        match &prompt.kind {
            PromptBindingKind::Raw => {
                code.push_str(&format!(
                    "pub(crate) const {}: &str = {:?};\n\n",
                    prompt.const_name, prompt.content
                ));
            }
            PromptBindingKind::App(app) => {
                code.push_str(&format!(
                    "pub(crate) const {}: &str = {:?};\n\n",
                    prompt.source_const_name, prompt.content
                ));
                code.push_str(&format!(
                    "pub(crate) const {}: super::AppPrompt = super::AppPrompt {{\n",
                    prompt.const_name
                ));
                code.push_str(&format!("    description: {:?},\n", app.description));
                code.push_str("    when_to_focus: &[\n");
                for item in &app.when_to_focus {
                    code.push_str(&format!("        {:?},\n", item));
                }
                code.push_str("    ],\n");
                code.push_str(&format!("    how_to_use: {:?},\n", app.how_to_use));
                code.push_str("};\n\n");
            }
            PromptBindingKind::Persona(persona) => {
                code.push_str(&format!(
                    "pub(crate) const {}: &str = {:?};\n\n",
                    prompt.source_const_name, prompt.content
                ));
                code.push_str(&format!(
                    "pub(crate) const {}: super::PromptPersona = super::PromptPersona {{\n",
                    prompt.const_name
                ));
                code.push_str(&format!("    name: {:?},\n", persona.name));
                code.push_str(&format!("    language: {:?},\n", persona.language));
                code.push_str(&format!(
                    "    identity_summary: {:?},\n",
                    persona.identity_summary
                ));
                code.push_str("};\n\n");
            }
        }
    }
    code.push_str("#[allow(dead_code)]\npub(crate) const PROMPT_SOURCES: &[(&str, &str)] = &[\n");
    for prompt in &prompts {
        code.push_str(&format!(
            "    ({:?}, {}),\n",
            prompt.prompt_id, prompt.source_const_name
        ));
    }
    code.push_str("];\n");
    fs::write(out_path, code).expect("write prompt bindings");
}

fn is_app_prompt_file(relative: &Path) -> bool {
    let components = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    components.len() == 2
        && components[0] == "apps"
        && relative.extension().and_then(|value| value.to_str()) == Some("md")
}

fn is_persona_prompt_file(relative: &Path) -> bool {
    let components = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    components.len() == 2
        && components[0] == "persona"
        && relative.extension().and_then(|value| value.to_str()) == Some("md")
}

fn collect_prompt_markdown_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries = fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("failed to read prompt dir {}: {err}", dir.display()))
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect::<Vec<_>>();
    entries.sort();

    for entry in entries {
        if entry.is_dir() {
            collect_prompt_markdown_files(&entry, out);
        } else if entry.extension().and_then(|value| value.to_str()) == Some("md") {
            out.push(entry);
        }
    }
}

fn prompt_id_from_relative_path(relative: &Path) -> String {
    relative
        .with_extension("")
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn prompt_const_name_from_relative_path(relative: &Path) -> String {
    if let Some(app_stem) = app_prompt_stem(relative) {
        return format!("APP_{}", sanitize_const_name(&app_stem));
    }
    if let Some(persona_stem) = persona_prompt_stem(relative) {
        return format!("PERSONA_{}", sanitize_const_name(&persona_stem));
    }
    let raw = relative
        .with_extension("")
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("_");
    sanitize_const_name(&raw)
}

fn app_prompt_stem(relative: &Path) -> Option<String> {
    if !is_app_prompt_file(relative) {
        return None;
    }
    relative
        .file_stem()
        .and_then(|value| value.to_str())
        .map(ToOwned::to_owned)
}

fn persona_prompt_stem(relative: &Path) -> Option<String> {
    if !is_persona_prompt_file(relative) {
        return None;
    }
    relative
        .file_stem()
        .and_then(|value| value.to_str())
        .map(ToOwned::to_owned)
}

fn sanitize_const_name(raw: &str) -> String {
    let mut name = String::new();
    let mut previous_was_underscore = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            name.push(ch.to_ascii_uppercase());
            previous_was_underscore = false;
        } else if !previous_was_underscore {
            name.push('_');
            previous_was_underscore = true;
        }
    }
    let name = name.trim_matches('_').to_string();
    if name.is_empty() {
        panic!("prompt path produced empty constant name from {raw:?}");
    }
    name
}

fn trim_trailing_line_endings(input: &str) -> &str {
    input.trim_end_matches(['\r', '\n'])
}

fn parse_app_prompt_binding(path: &Path, content: &str) -> AppPromptBinding {
    parse_app_prompt_binding_inner(content)
        .unwrap_or_else(|err| panic!("invalid app prompt doc {}: {err}", path.display()))
}

fn parse_app_prompt_binding_inner(content: &str) -> Result<AppPromptBinding, String> {
    let (frontmatter, body) = split_prompt_frontmatter(content)
        .ok_or_else(|| "expected leading frontmatter delimited by ---".to_string())?;
    let mut description = None::<String>;
    let mut when_to_focus = Vec::<String>::new();
    let mut current_list_key = None::<&str>;

    for line in frontmatter.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("description:") {
            let value = value.trim();
            if value.is_empty() {
                return Err("description cannot be empty".to_string());
            }
            description = Some(value.to_string());
            current_list_key = None;
            continue;
        }
        if trimmed == "when_to_focus:" {
            current_list_key = Some("when_to_focus");
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("- ") {
            match current_list_key {
                Some("when_to_focus") => when_to_focus.push(value.trim().to_string()),
                _ => return Err(format!("list item without supported key: {line}")),
            }
            continue;
        }
        return Err(format!("unsupported frontmatter line: {line}"));
    }

    let description = description.ok_or_else(|| "missing description".to_string())?;
    if when_to_focus.is_empty() {
        return Err("missing when_to_focus items".to_string());
    }
    let how_to_use = body.trim().to_string();
    if how_to_use.is_empty() {
        return Err("missing how-to-use body".to_string());
    }
    Ok(AppPromptBinding {
        description,
        when_to_focus,
        how_to_use,
    })
}

fn parse_persona_prompt_binding(path: &Path, content: &str) -> PersonaPromptBinding {
    parse_persona_prompt_binding_inner(content)
        .unwrap_or_else(|err| panic!("invalid persona prompt doc {}: {err}", path.display()))
}

fn parse_persona_prompt_binding_inner(content: &str) -> Result<PersonaPromptBinding, String> {
    let (frontmatter, body) = split_prompt_frontmatter(content)
        .ok_or_else(|| "expected leading frontmatter delimited by ---".to_string())?;
    let mut name = None::<String>;
    let mut language = default_prompt_persona_language().to_string();

    for line in frontmatter.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("name:") {
            let value = value.trim();
            if value.is_empty() {
                return Err("name cannot be empty".to_string());
            }
            name = Some(value.to_string());
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("language:") {
            let value = value.trim();
            language = if value.is_empty() {
                default_prompt_persona_language().to_string()
            } else {
                value.to_string()
            };
            continue;
        }
        return Err(format!("unsupported frontmatter line: {line}"));
    }

    let name = name.ok_or_else(|| "missing name".to_string())?;
    let identity_summary = body.trim().to_string();
    if identity_summary.is_empty() {
        return Err("missing persona body".to_string());
    }
    Ok(PersonaPromptBinding {
        name,
        language,
        identity_summary,
    })
}

fn default_prompt_persona_language() -> &'static str {
    "configured-locale"
}

fn split_prompt_frontmatter(content: &str) -> Option<(&str, &str)> {
    let content = content.strip_prefix("---\r\n").or_else(|| {
        content
            .strip_prefix("---\n")
            .or_else(|| content.strip_prefix("---"))
    })?;
    let delimiter = content
        .find("\n---\n")
        .map(|index| (index, 5))
        .or_else(|| content.find("\r\n---\r\n").map(|index| (index, 7)))
        .or_else(|| content.find("\n---\r\n").map(|index| (index, 6)))
        .or_else(|| content.find("\r\n---\n").map(|index| (index, 6)))?;
    let (frontmatter, rest) = content.split_at(delimiter.0);
    Some((frontmatter, &rest[delimiter.1..]))
}

fn write_builtin_primitive_bindings(manifest_dir: &Path) {
    let workflows_dir = manifest_dir.join("workflows");
    println!("cargo:rerun-if-changed={}", workflows_dir.display());

    let mut sources = Vec::<(String, PathBuf)>::new();
    if let Ok(entries) = fs::read_dir(&workflows_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !is_builtin_primitive_file(&path) {
                continue;
            }
            println!("cargo:rerun-if-changed={}", path.display());
            let stem = path
                .file_stem()
                .and_then(|value| value.to_str())
                .expect("workflow stem")
                .to_string();
            let canonical = path.canonicalize().unwrap_or_else(|_| {
                workflows_dir.join(path.file_name().expect("primitive spec file"))
            });
            sources.push((stem, canonical));
        }
    }
    sources.sort_by(|left, right| left.0.cmp(&right.0));

    let out_path =
        PathBuf::from(env::var("OUT_DIR").expect("out dir")).join("builtin_workflows.rs");
    let mut code =
        String::from("pub(crate) const BUILTIN_PRIMITIVE_SOURCES: &[(&str, &str)] = &[\n");
    for (id, path) in &sources {
        code.push_str(&format!("    ({id:?}, include_str!({:?})),\n", path));
    }
    code.push_str("];\n");
    fs::write(out_path, code).expect("write builtin primitive bindings");
}

fn is_builtin_primitive_file(path: &Path) -> bool {
    path.extension().and_then(|value| value.to_str()) == Some("md")
        && path.file_name().and_then(|value| value.to_str()) != Some("README.md")
}
