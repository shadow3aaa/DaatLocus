use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
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
