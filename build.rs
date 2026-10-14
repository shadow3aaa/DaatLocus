use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    emit_build_target();
    write_models_dev_catalog();
    println!("cargo:rerun-if-env-changed=DAAT_LOCUS_BUILD_WEBUI");
    if env::var("DAAT_LOCUS_BUILD_WEBUI").ok().as_deref() != Some("0") {
        build_embedded_webui(&manifest_dir);
    } else {
        write_webui_assets(None);
    }
}

fn emit_build_target() {
    let target = env::var("TARGET").expect("target triple");
    println!("cargo:rustc-env=DAAT_LOCUS_BUILD_TARGET={target}");
}

const MODELS_DEV_API_URL: &str = "https://models.dev/api.json";

fn write_models_dev_catalog() {
    let out_path = PathBuf::from(env::var("OUT_DIR").expect("out dir")).join("models-dev-api.json");
    println!("cargo:rerun-if-env-changed=DAAT_LOCUS_MODEL_CATALOG_PATH");
    let body = if let Some(path) = env::var_os("DAAT_LOCUS_MODEL_CATALOG_PATH") {
        let path = PathBuf::from(path);
        println!("cargo:rerun-if-changed={}", path.display());
        fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read model catalog {}: {err}", path.display()))
    } else {
        fetch_models_dev_catalog()
            .unwrap_or_else(|err| panic!("failed to download {MODELS_DEV_API_URL}: {err}"))
    };
    validate_models_dev_catalog(&body);
    fs::write(&out_path, body).unwrap_or_else(|err| {
        panic!(
            "failed to write models.dev catalog {}: {err}",
            out_path.display()
        )
    });
}

fn fetch_models_dev_catalog() -> Result<String, reqwest::Error> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("daat-locus-build")
        .build()?;
    client
        .get(MODELS_DEV_API_URL)
        .send()?
        .error_for_status()?
        .text()
}

fn validate_models_dev_catalog(body: &str) {
    let root: serde_json::Value = serde_json::from_str(body)
        .unwrap_or_else(|err| panic!("model catalog contains invalid JSON: {err}"));
    assert!(
        root.as_object()
            .is_some_and(|providers| !providers.is_empty()),
        "model catalog contains no providers"
    );
}

fn build_embedded_webui(manifest_dir: &Path) {
    let webui_dir = manifest_dir.join("webui");
    let assets_dir = manifest_dir.join("assets");
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("out dir"));
    let webui_work = out_dir.join("webui-work");
    let webui_dist = out_dir.join("webui-dist");
    emit_webui_rerun_inputs(&webui_dir, &assets_dir);

    assert!(
        webui_dir.join("package.json").is_file(),
        "WebUI package.json not found: {}",
        webui_dir.display()
    );

    let bun_command = webui_bun_command();
    // Build the shared cryptographic client in the real workspace. Its separate
    // target directory prevents nested Cargo builds from locking this build.
    emit_rerun_if_changed_recursively(&manifest_dir.join("crates/daat-locus-auth"));
    run_webui_command(&bun_command, &["run", "build:auth"], &webui_dir);
    prepare_webui_worktree(&webui_dir, &webui_work);
    run_webui_command(&bun_command, &["install", "--frozen-lockfile"], &webui_work);
    run_webui_build_command(&bun_command, &webui_work, &webui_dist, &assets_dir);

    let dist_index = webui_dist.join("index.html");
    assert!(
        dist_index.is_file(),
        "WebUI build did not produce required entry {}",
        dist_index.display()
    );
    write_webui_assets(Some(&webui_dist));
}

fn write_webui_assets(dist: Option<&Path>) {
    fn collect(root: &Path, dir: &Path, files: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).expect("read WebUI assets") {
            let entry = entry.expect("WebUI asset entry");
            let kind = entry.file_type().expect("WebUI asset type");
            assert!(!kind.is_symlink(), "WebUI assets cannot be symlinks");
            if kind.is_dir() {
                collect(root, &entry.path(), files);
            } else if kind.is_file() {
                files.push(
                    entry
                        .path()
                        .strip_prefix(root)
                        .expect("WebUI asset root")
                        .to_path_buf(),
                );
            }
        }
    }
    let mut generated = String::from("const WEBUI_ASSETS: &[(&str, &str, &[u8])] = &[\n");
    if let Some(dist) = dist {
        let marker = PathBuf::from(env::var("OUT_DIR").expect("out dir")).join("webui-build.json");
        let info = serde_json::json!({"format":"DaatLocus.EmbeddedWebUI.v1","version":env::var("CARGO_PKG_VERSION").expect("version"),"target":env::var("TARGET").expect("target")});
        fs::write(&marker, serde_json::to_vec(&info).expect("build info"))
            .expect("write build info");
        generated.push_str(&format!(
            "(\"/.well-known/daat-locus-build.json\",\"application/json\",include_bytes!({:?})),\n",
            marker
        ));
        let mut files = Vec::new();
        collect(dist, dist, &mut files);
        files.sort();
        for relative in files {
            let key = format!("/{}", relative.to_string_lossy().replace('\\', "/"));
            let mime = match relative.extension().and_then(|s| s.to_str()) {
                Some("html") => "text/html; charset=utf-8",
                Some("css") => "text/css; charset=utf-8",
                Some("js" | "mjs") => "text/javascript; charset=utf-8",
                Some("wasm") => "application/wasm",
                Some("json" | "map") => "application/json",
                Some("svg") => "image/svg+xml",
                Some("png") => "image/png",
                Some("jpg" | "jpeg") => "image/jpeg",
                Some("webp") => "image/webp",
                Some("gif") => "image/gif",
                Some("ico") => "image/x-icon",
                Some("woff") => "font/woff",
                Some("woff2") => "font/woff2",
                Some("txt") => "text/plain; charset=utf-8",
                _ => "application/octet-stream",
            };
            generated.push_str(&format!(
                "({key:?}, {mime:?}, include_bytes!({:?})),\n",
                dist.join(relative).to_string_lossy()
            ));
        }
    }
    generated.push_str("];\n");
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").expect("out dir")).join("webui-assets.rs"),
        generated,
    )
    .expect("write WebUI asset manifest");
}

fn emit_webui_rerun_inputs(webui_dir: &Path, assets_dir: &Path) {
    for path in [
        webui_dir.join("bun.lock"),
        webui_dir.join("components.json"),
        webui_dir.join("index.html"),
        webui_dir.join("package.json"),
        webui_dir.join("tailwind.config.cjs"),
        webui_dir.join("tsconfig.json"),
        webui_dir.join("vite.config.ts"),
        assets_dir.join("logo-mark.svg"),
    ] {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    emit_rerun_if_changed_recursively(&webui_dir.join("src"));
    emit_rerun_if_changed_recursively(&webui_dir.join("scripts"));
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
        "bun.lock",
        "components.json",
        "index.html",
        "package.json",
        "tailwind.config.cjs",
        "tsconfig.json",
        "vite.config.ts",
    ] {
        copy_webui_file(source_dir, work_dir, relative);
    }
    copy_webui_dir(&source_dir.join("src"), &work_dir.join("src"));
    copy_webui_dir(&source_dir.join("scripts"), &work_dir.join("scripts"));
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
    if path.file_name().is_some_and(|name| name == "generated") {
        return;
    }
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

#[cfg(windows)]
fn webui_bun_command() -> Vec<String> {
    assert!(
        resolve_command("bun").is_some(),
        "Bun is required to build the embedded WebUI."
    );

    vec!["cmd".to_string(), "/C".to_string(), "bun".to_string()]
}

#[cfg(not(windows))]
fn webui_bun_command() -> Vec<String> {
    if resolve_command("bun").is_none() {
        panic!("Bun is required to build the embedded WebUI.");
    }

    vec!["bun".to_string()]
}

fn run_webui_build_command(
    bun_command: &[String],
    webui_dir: &Path,
    out_dir: &Path,
    assets_dir: &Path,
) {
    let mut command = Command::new(&bun_command[0]);
    command
        .args(&bun_command[1..])
        .args(["run", "build"])
        .env("DAAT_LOCUS_WEBUI_OUT_DIR", out_dir)
        .env("DAAT_LOCUS_ASSETS_DIR", assets_dir)
        .env("DAAT_LOCUS_AUTH_WASM_READY", "1")
        .current_dir(webui_dir);
    run_webui_command_status(command, "WebUI build");
}

fn run_webui_command(bun_command: &[String], args: &[&str], webui_dir: &Path) {
    let mut command = Command::new(&bun_command[0]);
    command
        .args(&bun_command[1..])
        .args(args)
        .current_dir(webui_dir);
    run_webui_command_status(command, "WebUI command");
}

fn run_webui_command_status(mut command: Command, label: &str) {
    let status = command
        .status()
        .unwrap_or_else(|err| panic!("failed to run {label} {command:?}: {err}"));
    assert!(
        status.success(),
        "{label} {command:?} failed with status {status}"
    );
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
