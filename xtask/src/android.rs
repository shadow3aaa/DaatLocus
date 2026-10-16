use crate::{
    Metadata, Result, absolute, command, contains_native_ui_marker, contains_ui_marker,
    reporting::{packaged, status},
};
use clap::{Args, ValueEnum};
use serde::Deserialize;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Target {
    #[value(name = "aarch64", alias = "aarch64-linux-android", alias = "arm64-v8a")]
    Arm64,
    #[value(name = "x86_64", alias = "x86_64-linux-android")]
    X86_64,
}
impl Target {
    fn name(self) -> &'static str {
        match self {
            Self::Arm64 => "aarch64",
            Self::X86_64 => "x86_64",
        }
    }
    fn abi(self) -> &'static str {
        match self {
            Self::Arm64 => "arm64-v8a",
            Self::X86_64 => "x86_64",
        }
    }
    fn triple(self) -> &'static str {
        match self {
            Self::Arm64 => "aarch64-linux-android",
            Self::X86_64 => "x86_64-linux-android",
        }
    }
}

#[derive(Args)]
pub struct Package {
    /// Android architecture (defaults to ARM64 phones).
    #[arg(long, value_enum, default_value = "aarch64")]
    target: Target,
    /// Use Android's debug signing key instead of private release signing.
    #[arg(long)]
    debug: bool,
    /// Copy an existing Gradle APK after checking its version, ABI and embedded UI.
    #[arg(long)]
    skip_build: bool,
    /// Directory containing output-metadata.json and an existing APK.
    #[arg(long, requires = "skip_build")]
    apk_dir: Option<PathBuf>,
    /// Destination directory; relative paths are resolved from the repository root.
    #[arg(long)]
    out_dir: Option<PathBuf>,
}

pub fn package(root: &Path, meta: &Metadata, args: Package) -> Result<()> {
    let profile = if args.debug { "debug" } else { "release" };
    let source = args
        .apk_dir
        .map(|path| absolute(root, path))
        .unwrap_or_else(|| {
            root.join("clients/app/src-tauri/gen/android/app/build/outputs/apk/universal")
                .join(profile)
        });
    let destination = args
        .out_dir
        .map(|path| absolute(root, path))
        .unwrap_or_else(|| {
            root.join("target")
                .join(args.target.triple())
                .join(profile)
                .join("package")
        });
    status(
        "Packaging",
        &format!(
            "{} v{} (Android {}, {profile})",
            meta.name,
            meta.version,
            args.target.name()
        ),
    );
    if !args.skip_build {
        let mut build = build_command(root, args.target, args.debug);
        if !args.debug {
            let signing = std::env::var_os("DAAT_LOCUS_ANDROID_SIGNING")
                .ok_or("Set DAAT_LOCUS_ANDROID_SIGNING to private release signing properties, or use --debug")?;
            let signing = absolute(root, signing.into());
            if !signing.is_file() {
                return Err("Android signing properties file is missing".into());
            }
            build.env("DAAT_LOCUS_ANDROID_SIGNING", signing);
        }
        status("Building", &format!("Android {} APK", args.target.name()));
        command(&mut build)?;
    }
    let apk = artifact(&source, &meta.version, args.target, args.debug)?;
    fs::create_dir_all(&destination)?;
    let packaged_apk = destination.join(format!(
        "{}-{}-{}{}.apk",
        meta.name,
        meta.version,
        args.target.triple(),
        if args.debug { "-debug" } else { "" }
    ));
    if packaged_apk.canonicalize().ok().as_ref() != Some(&apk.canonicalize()?) {
        fs::copy(&apk, &packaged_apk)?;
    }
    packaged(
        if args.debug {
            "Android debug APK"
        } else {
            "Android release APK"
        },
        &packaged_apk,
    )
}

fn build_command(root: &Path, target: Target, debug: bool) -> Command {
    let mut build = Command::new("bun");
    build
        .current_dir(root.join("clients/app"))
        .args(["run", "android:build", "--target", target.name()])
        .env("DAAT_LOCUS_BUILD_WEBUI", "0");
    if debug {
        build.arg("--debug");
    }
    build
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OutputMetadata {
    application_id: String,
    variant_name: String,
    elements: Vec<Artifact>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Artifact {
    output_file: String,
    version_name: String,
}

fn artifact(directory: &Path, version: &str, target: Target, debug: bool) -> Result<PathBuf> {
    let metadata: OutputMetadata =
        serde_json::from_slice(&fs::read(directory.join("output-metadata.json"))?)?;
    if metadata.application_id != "io.daat_locus.app"
        || metadata.variant_name
            != if debug {
                "universalDebug"
            } else {
                "universalRelease"
            }
        || metadata.elements.len() != 1
    {
        return Err("Unexpected Android APK metadata; rebuild the requested variant".into());
    }
    let entry = &metadata.elements[0];
    if entry.version_name != version
        || entry.output_file.contains(['/', '\\', ':'])
        || !entry.output_file.ends_with(".apk")
        || entry.output_file.contains("..")
        || entry.output_file.ends_with("-unsigned.apk")
    {
        return Err("Android APK has an unexpected version or filename".into());
    }
    let path = directory.join(&entry.output_file);
    if fs::symlink_metadata(&path)?.file_type().is_symlink() {
        return Err("Android APK cannot be a symlink".into());
    }
    let mut archive = zip::ZipArchive::new(fs::File::open(&path)?)?;
    let expected = format!("lib/{}/libdaat_locus_app_lib.so", target.abi());
    let libraries = archive
        .file_names()
        .filter(|name| name.ends_with("/libdaat_locus_app_lib.so"))
        .collect::<Vec<_>>();
    if libraries != [expected.as_str()] {
        return Err("APK ABI does not match --target; rebuild the requested architecture".into());
    }
    if !contains_native_ui_marker(archive.by_name(&expected)?)? {
        return Err(
            "Android APK has no native WebUI; rebuild through the Tauri build pipeline".into(),
        );
    }
    if contains_ui_marker(archive.by_name(&expected)?)? {
        return Err(
            "Android backend duplicates native WebUI assets; disable embedded-webui".into(),
        );
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NATIVE_UI_MARKER;
    use clap::Parser;
    use std::io::Write;

    #[test]
    fn cli_defaults_and_android_build_command_keep_the_existing_pipeline() {
        let parsed = crate::Cli::try_parse_from(["xtask", "package", "android"]).unwrap();
        let crate::Task::Package {
            format: crate::Format::Android(args),
        } = parsed.command
        else {
            panic!("Android command missing")
        };
        assert!(matches!(args.target, Target::Arm64));
        assert!(!args.debug);
        for target in ["x86_64", "x86_64-linux-android", "arm64-v8a"] {
            assert!(
                crate::Cli::try_parse_from([
                    "xtask", "package", "android", "--target", target, "--debug"
                ])
                .is_ok()
            );
        }
        assert!(
            crate::Cli::try_parse_from(["xtask", "package", "android", "--target", "windows"])
                .is_err()
        );
        assert!(
            crate::Cli::try_parse_from(["xtask", "package", "android", "--apk-dir", "existing"])
                .is_err()
        );
        let build = build_command(Path::new("workspace"), Target::X86_64, true);
        assert_eq!(
            build.get_args().collect::<Vec<_>>(),
            ["run", "android:build", "--target", "x86_64", "--debug"]
        );
        assert_eq!(
            build.get_current_dir().unwrap(),
            Path::new("workspace/clients/app")
        );
        assert!(
            build
                .get_envs()
                .any(|(key, value)| key == "DAAT_LOCUS_BUILD_WEBUI"
                    && value == Some(std::ffi::OsStr::new("0")))
        );
    }

    #[test]
    fn apk_validation_rejects_stale_wrong_abi_and_backend_only_artifacts() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_path_buf();
        let metadata = root.join("output-metadata.json");
        let write_metadata = |filename: &str| {
            fs::write(
                &metadata,
                serde_json::to_vec(&serde_json::json!({
                    "applicationId":"io.daat_locus.app", "variantName":"universalDebug",
                    "elements":[{"versionName":"0.5.0","outputFile":filename}]
                }))
                .unwrap(),
            )
            .unwrap()
        };
        write_metadata("app.apk");
        let apk = root.join("app.apk");
        let write_apk = |body: &[u8]| {
            let mut zip = zip::ZipWriter::new(fs::File::create(&apk).unwrap());
            zip.start_file(
                "lib/x86_64/libdaat_locus_app_lib.so",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            zip.write_all(body).unwrap();
            zip.finish().unwrap();
        };
        write_apk(NATIVE_UI_MARKER);
        assert_eq!(artifact(&root, "0.5.0", Target::X86_64, true).unwrap(), apk);
        assert!(artifact(&root, "0.5.1", Target::X86_64, true).is_err());
        assert!(artifact(&root, "0.5.0", Target::Arm64, true).is_err());
        assert!(artifact(&root, "0.5.0", Target::X86_64, false).is_err());
        write_apk(b"backend only");
        assert!(artifact(&root, "0.5.0", Target::X86_64, true).is_err());
        write_apk(&[NATIVE_UI_MARKER, crate::UI_MARKER].concat());
        assert!(artifact(&root, "0.5.0", Target::X86_64, true).is_err());
        for invalid in ["../app.apk", "C:app.apk", "app-unsigned.apk"] {
            write_metadata(invalid);
            assert!(artifact(&root, "0.5.0", Target::X86_64, true).is_err());
        }
    }
}
