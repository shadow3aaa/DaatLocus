use std::{
    env, fs,
    path::Component,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use sha2::{Digest, Sha256};

const HINDSIGHT_SIDECAR_MANIFEST: &str = "manifest.toml";

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    write_builtin_workflow_bindings(&manifest_dir);
    write_embedded_hindsight_sidecar_bindings(&manifest_dir);
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

fn write_embedded_hindsight_sidecar_bindings(manifest_dir: &Path) {
    println!("cargo:rerun-if-env-changed=DAAT_LOCUS_HINDSIGHT_SIDECAR");
    println!("cargo:rerun-if-env-changed=DAAT_LOCUS_HINDSIGHT_SIDECAR_ENTRY");
    println!("cargo:rerun-if-env-changed=DAAT_LOCUS_EMBED_HINDSIGHT_SIDECAR");
    println!("cargo:rerun-if-env-changed=DAAT_LOCUS_REQUIRE_HINDSIGHT_SIDECAR");
    let target = env::var("TARGET").expect("target triple");
    let default_dir = manifest_dir.join("assets").join("hindsight-sidecars");
    let manifest_path = default_dir.join(HINDSIGHT_SIDECAR_MANIFEST);
    println!("cargo:rerun-if-changed={}", default_dir.display());
    println!("cargo:rerun-if-changed={}", manifest_path.display());
    let explicit_path = env::var_os("DAAT_LOCUS_HINDSIGHT_SIDECAR").map(PathBuf::from);
    let explicit_entry = env::var("DAAT_LOCUS_HINDSIGHT_SIDECAR_ENTRY").ok();
    let discover_local_sidecar = env_flag_enabled("DAAT_LOCUS_EMBED_HINDSIGHT_SIDECAR")
        || env_flag_enabled("DAAT_LOCUS_REQUIRE_HINDSIGHT_SIDECAR");
    let sidecar = explicit_path
        .clone()
        .map(|path| SidecarArchiveSelection {
            path,
            archive_kind: None,
            expected_sha256: None,
            entry: explicit_entry.clone(),
            source: SidecarArchiveSource::ExplicitEnv,
        })
        .or_else(|| {
            discover_local_sidecar
                .then(|| manifest_sidecar_archive(&manifest_path, &default_dir, &target))
                .flatten()
        })
        .or_else(|| {
            discover_local_sidecar
                .then(|| conventional_sidecar_archive(&default_dir, &target))
                .flatten()
        });

    let out_path =
        PathBuf::from(env::var("OUT_DIR").expect("out dir")).join("embedded_hindsight_sidecar.rs");
    let mut code = String::new();
    code.push_str(&format!(
        "pub(crate) const EMBEDDED_HINDSIGHT_SIDECAR_TARGET: &str = {target:?};\n"
    ));

    match sidecar {
        Some(selection) if selection.path.exists() => {
            println!("cargo:rerun-if-changed={}", selection.path.display());
            let archive_kind = selection.archive_kind.as_deref().unwrap_or_else(|| {
                sidecar_archive_kind(&selection.path).unwrap_or_else(|| {
                    panic!(
                        "unsupported hindsight sidecar archive extension: {}",
                        selection.path.display()
                    )
                })
            });
            validate_archive_kind(archive_kind).unwrap_or_else(|err| panic!("{err}"));
            let entry = selection
                .entry
                .clone()
                .unwrap_or_else(|| default_sidecar_entry(&target).to_string());
            ensure_safe_relative_path_string("hindsight sidecar entry", &entry)
                .unwrap_or_else(|err| panic!("{err}"));
            let bytes = fs::read(&selection.path).unwrap_or_else(|err| {
                panic!(
                    "failed to read hindsight sidecar archive {}: {err}",
                    selection.path.display()
                )
            });
            let sha256 = format!("{:x}", Sha256::digest(&bytes));
            if let Some(expected) = &selection.expected_sha256
                && !expected.eq_ignore_ascii_case(&sha256)
            {
                panic!(
                    "hindsight sidecar checksum mismatch for {}: manifest expected {}, got {}",
                    selection.path.display(),
                    expected,
                    sha256,
                )
            }
            let canonical = selection.path.canonicalize().unwrap_or(selection.path);
            code.push_str(&format!(
                "pub(crate) const EMBEDDED_HINDSIGHT_SIDECAR_ARCHIVE_KIND: &str = {archive_kind:?};\n"
            ));
            code.push_str(&format!(
                "pub(crate) const EMBEDDED_HINDSIGHT_SIDECAR_SHA256: &str = {sha256:?};\n"
            ));
            code.push_str(&format!(
                "pub(crate) const EMBEDDED_HINDSIGHT_SIDECAR_ENTRY: &str = {entry:?};\n"
            ));
            code.push_str(&format!(
                "pub(crate) const EMBEDDED_HINDSIGHT_SIDECAR_BYTES: Option<&'static [u8]> = Some(include_bytes!({:?}));\n",
                canonical
            ));
        }
        Some(selection) if selection.source == SidecarArchiveSource::ExplicitEnv => {
            panic!(
                "DAAT_LOCUS_HINDSIGHT_SIDECAR points to missing file: {}",
                selection.path.display()
            );
        }
        Some(selection) if selection.source == SidecarArchiveSource::Manifest => {
            panic!(
                "assets/hindsight-sidecars/manifest.toml points target '{}' to missing archive: {}",
                target,
                selection.path.display()
            );
        }
        _ => {
            if env_flag_enabled("DAAT_LOCUS_REQUIRE_HINDSIGHT_SIDECAR") {
                panic!(
                    "missing Hindsight sidecar for target '{}'; run `cargo xtask build-hindsight-sidecar` for the current platform, import a CI-built archive with `cargo xtask import-hindsight-sidecar`, set DAAT_LOCUS_EMBED_HINDSIGHT_SIDECAR=1 to use local assets, or set DAAT_LOCUS_HINDSIGHT_SIDECAR",
                    target
                );
            }
            code.push_str(
                "pub(crate) const EMBEDDED_HINDSIGHT_SIDECAR_ARCHIVE_KIND: &str = \"\";\n",
            );
            code.push_str("pub(crate) const EMBEDDED_HINDSIGHT_SIDECAR_SHA256: &str = \"\";\n");
            code.push_str(&format!(
                "pub(crate) const EMBEDDED_HINDSIGHT_SIDECAR_ENTRY: &str = {:?};\n",
                default_sidecar_entry(&target)
            ));
            code.push_str(
                "pub(crate) const EMBEDDED_HINDSIGHT_SIDECAR_BYTES: Option<&'static [u8]> = None;\n",
            );
        }
    }

    fs::write(out_path, code).expect("write embedded hindsight sidecar bindings");
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SidecarArchiveSource {
    ExplicitEnv,
    Manifest,
    ConventionalPath,
}

#[derive(Clone, Debug)]
struct SidecarArchiveSelection {
    path: PathBuf,
    archive_kind: Option<String>,
    expected_sha256: Option<String>,
    entry: Option<String>,
    source: SidecarArchiveSource,
}

#[derive(Debug, Deserialize)]
struct HindsightSidecarManifest {
    #[serde(default)]
    sidecar: Vec<HindsightSidecarManifestEntry>,
}

#[derive(Debug, Deserialize)]
struct HindsightSidecarManifestEntry {
    target: String,
    archive: String,
    sha256: String,
    entry: Option<String>,
    archive_kind: Option<String>,
}

fn manifest_sidecar_archive(
    manifest_path: &Path,
    default_dir: &Path,
    target: &str,
) -> Option<SidecarArchiveSelection> {
    if !manifest_path.exists() {
        return None;
    }
    let manifest_text = fs::read_to_string(manifest_path).unwrap_or_else(|err| {
        panic!(
            "failed to read Hindsight sidecar manifest {}: {err}",
            manifest_path.display()
        )
    });
    let manifest: HindsightSidecarManifest = toml::from_str(&manifest_text).unwrap_or_else(|err| {
        panic!(
            "failed to parse Hindsight sidecar manifest {}: {err}",
            manifest_path.display()
        )
    });

    let mut matches = manifest
        .sidecar
        .into_iter()
        .filter(|entry| entry.target == target)
        .collect::<Vec<_>>();
    if matches.len() > 1 {
        panic!("Hindsight sidecar manifest has duplicate entries for target '{target}'");
    }
    let entry = matches.pop()?;
    ensure_safe_relative_path_string("hindsight sidecar archive", &entry.archive)
        .unwrap_or_else(|err| panic!("{err}"));
    if let Some(sidecar_entry) = &entry.entry {
        ensure_safe_relative_path_string("hindsight sidecar entry", sidecar_entry)
            .unwrap_or_else(|err| panic!("{err}"));
    }
    if let Some(archive_kind) = &entry.archive_kind {
        validate_archive_kind(archive_kind).unwrap_or_else(|err| panic!("{err}"));
    }
    Some(SidecarArchiveSelection {
        path: default_dir.join(entry.archive),
        archive_kind: entry.archive_kind,
        expected_sha256: Some(entry.sha256),
        entry: entry.entry,
        source: SidecarArchiveSource::Manifest,
    })
}

fn conventional_sidecar_archive(
    default_dir: &Path,
    target: &str,
) -> Option<SidecarArchiveSelection> {
    default_sidecar_archive_path(default_dir, target).map(|path| SidecarArchiveSelection {
        path,
        archive_kind: None,
        expected_sha256: None,
        entry: None,
        source: SidecarArchiveSource::ConventionalPath,
    })
}

fn default_sidecar_archive_path(default_dir: &Path, target: &str) -> Option<PathBuf> {
    ["tar.zst", "tzst", "tar.gz", "tgz", "zip"]
        .into_iter()
        .map(|extension| default_dir.join(format!("{target}.{extension}")))
        .find(|path| path.exists())
}

fn default_sidecar_entry(target: &str) -> &'static str {
    if target.contains("windows") {
        "bin/hindsight-embed.exe"
    } else {
        "bin/hindsight-embed"
    }
}

fn sidecar_archive_kind(path: &Path) -> Option<&'static str> {
    let file_name = path.file_name()?.to_str()?;
    if file_name.ends_with(".tar.zst") || file_name.ends_with(".tzst") {
        Some("tar.zst")
    } else if file_name.ends_with(".tar.gz") || file_name.ends_with(".tgz") {
        Some("tar.gz")
    } else if file_name.ends_with(".zip") {
        Some("zip")
    } else {
        None
    }
}

fn validate_archive_kind(value: &str) -> Result<(), String> {
    match value {
        "tar.zst" | "tar.gz" | "zip" => Ok(()),
        other => Err(format!(
            "unsupported hindsight sidecar archive kind '{other}'; expected 'tar.zst', 'tar.gz', or 'zip'"
        )),
    }
}

fn ensure_safe_relative_path_string(label: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{label} must not be empty"));
    }
    if value.contains('\\') {
        return Err(format!("{label} must use '/' separators: {value}"));
    }
    let path = Path::new(value);
    if path.is_absolute() {
        return Err(format!("{label} must be relative: {value}"));
    }
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!("{label} escapes its base directory: {value}"));
            }
        }
    }
    Ok(())
}

fn env_flag_enabled(name: &str) -> bool {
    env::var(name)
        .map(|value| {
            matches!(
                value.as_str(),
                "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
            )
        })
        .unwrap_or(false)
}

fn is_builtin_workflow_file(path: &Path) -> bool {
    path.extension().and_then(|value| value.to_str()) == Some("md")
        && path.file_name().and_then(|value| value.to_str()) != Some("README.md")
}
