use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    emit_build_target();
    build_embedded_webui(&manifest_dir);
    write_builtin_workflow_bindings(&manifest_dir);
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
    println!(
        "cargo:rustc-env=DAAT_LOCUS_WEBUI_DIST={}",
        webui_dist.display()
    );
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

    if command_exists("corepack") {
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
    run_webui_command_status(command, "WebUI build");
}

fn run_webui_command(package_manager: &[String], args: &[&str], webui_dir: &Path) {
    let mut command = Command::new(&package_manager[0]);
    command
        .args(&package_manager[1..])
        .args(args)
        .current_dir(webui_dir);
    run_webui_command_status(command, "WebUI command");
}

fn run_webui_command_status(mut command: Command, label: &str) {
    let status = command
        .status()
        .unwrap_or_else(|err| panic!("failed to run {label} {:?}: {err}", command));
    if !status.success() {
        panic!("{label} {:?} failed with status {status}", command);
    }
}

fn command_exists(command: &str) -> bool {
    let command_path = Path::new(command);
    if command_path.components().count() > 1 {
        return executable_candidate_exists(command_path);
    }
    env::var_os("PATH")
        .map(|path| {
            env::split_paths(&path).any(|dir| {
                let candidate = dir.join(command);
                executable_candidate_exists(&candidate)
            })
        })
        .unwrap_or(false)
}

fn executable_candidate_exists(candidate: &Path) -> bool {
    if candidate.is_file() {
        return true;
    }

    #[cfg(windows)]
    {
        env::var_os("PATHEXT")
            .map(|pathext| {
                env::split_paths(&pathext).any(|extension| {
                    let extension = extension.to_string_lossy();
                    let extension = extension.trim_start_matches('.');
                    candidate.with_extension(extension).is_file()
                })
            })
            .unwrap_or(false)
    }

    #[cfg(not(windows))]
    {
        false
    }
}

fn write_builtin_workflow_bindings(manifest_dir: &Path) {
    let workflows_dir = manifest_dir.join("workflows");
    println!("cargo:rerun-if-changed={}", workflows_dir.display());

    let mut sources = Vec::<(String, PathBuf)>::new();
    if let Ok(entries) = fs::read_dir(&workflows_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !is_builtin_workflow_file(&path) {
                continue;
            }
            println!("cargo:rerun-if-changed={}", path.display());
            let stem = path
                .file_stem()
                .and_then(|value| value.to_str())
                .expect("workflow stem")
                .to_string();
            let canonical = path
                .canonicalize()
                .unwrap_or_else(|_| workflows_dir.join(path.file_name().expect("workflow file")));
            sources.push((stem, canonical));
        }
    }
    sources.sort_by(|left, right| left.0.cmp(&right.0));

    let out_path =
        PathBuf::from(env::var("OUT_DIR").expect("out dir")).join("builtin_workflows.rs");
    let mut code =
        String::from("pub(crate) const BUILTIN_WORKFLOW_SOURCES: &[(&str, &str)] = &[\n");
    for (id, path) in &sources {
        code.push_str(&format!("    ({id:?}, include_str!({:?})),\n", path));
    }
    code.push_str("];\n");
    fs::write(out_path, code).expect("write builtin workflow bindings");
}

fn is_builtin_workflow_file(path: &Path) -> bool {
    path.extension().and_then(|value| value.to_str()) == Some("md")
        && path.file_name().and_then(|value| value.to_str()) != Some("README.md")
}
