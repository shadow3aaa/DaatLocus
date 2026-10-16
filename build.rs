use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Duration,
};

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    emit_build_target();
    write_models_dev_catalog();
    println!("cargo:rerun-if-env-changed=DAAT_LOCUS_BUILD_WEBUI");
    if env::var_os("CARGO_FEATURE_EMBEDDED_WEBUI").is_some()
        && env::var("DAAT_LOCUS_BUILD_WEBUI").ok().as_deref() != Some("0")
    {
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
    let out_path =
        PathBuf::from(env::var("OUT_DIR").expect("out dir")).join("models-dev-api.json.zst");
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
    fs::write(&out_path, compress_embedded(body.as_bytes())).unwrap_or_else(|err| {
        panic!(
            "failed to write models.dev catalog {}: {err}",
            out_path.display()
        )
    });
}

fn compress_embedded(bytes: &[u8]) -> Vec<u8> {
    zstd::bulk::compress(bytes, 19).expect("compress embedded resource")
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
    println!("cargo:rerun-if-env-changed=DAAT_LOCUS_WEBUI_DIST");
    let dist = env::var_os("DAAT_LOCUS_WEBUI_DIST")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("webui/dist"));
    println!("cargo:rerun-if-changed={}", dist.display());
    assert!(
        dist.join("index.html").is_file(),
        "WebUI bundle missing: run `bun scripts/build-webui.ts` before building, or use --no-default-features for a headless service"
    );
    let dist = fs::canonicalize(dist).expect("resolve WebUI bundle");
    write_webui_assets(Some(&dist));
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
    let mut generated = String::from("const WEBUI_ASSETS: &[(&str, &str, AssetContent)] = &[\n");
    if let Some(dist) = dist {
        let compressed_dir =
            PathBuf::from(env::var("OUT_DIR").expect("out dir")).join("webui-zstd");
        let marker = PathBuf::from(env::var("OUT_DIR").expect("out dir")).join("webui-build.json");
        let info = serde_json::json!({"format":"DaatLocus.EmbeddedWebUI.v1","version":env::var("CARGO_PKG_VERSION").expect("version"),"target":env::var("TARGET").expect("target")});
        fs::write(&marker, serde_json::to_vec(&info).expect("build info"))
            .expect("write build info");
        generated.push_str(&format!(
            "(\"/.well-known/daat-locus-build.json\",\"application/json\",AssetContent::Plain(include_bytes!({:?}))),\n",
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
            let bytes = fs::read(dist.join(&relative)).expect("read WebUI asset");
            let compressed = compressed_dir.join(&relative);
            fs::create_dir_all(compressed.parent().expect("compressed asset parent"))
                .expect("create compressed WebUI directory");
            fs::write(&compressed, compress_embedded(&bytes))
                .expect("write compressed WebUI asset");
            generated.push_str(&format!(
                "({key:?}, {mime:?}, AssetContent::Zstd {{ bytes: include_bytes!({:?}), size: {} }}),\n",
                compressed.to_string_lossy(), bytes.len()
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
