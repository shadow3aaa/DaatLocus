use std::{
    env,
    error::Error,
    ffi::OsString,
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    process::{Command, ExitCode},
};

use flate2::{Compression, write::GzEncoder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::{ZipWriter, write::SimpleFileOptions};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const SIDECAR_MANIFEST: &str = "manifest.toml";
const DEFAULT_DIST_NAME: &str = "hindsight-embed";
const DEFAULT_RELEASE_OUT_DIR: &str = "dist";
const HINDSIGHT_PYTHON: &str = "3.12";
const HINDSIGHT_EMBED_PACKAGE: &str = "hindsight-embed==0.5.4";
const HINDSIGHT_API_PACKAGE: &str = "hindsight-api-slim[embedded-db,local-ml]==0.5.4";
const HINDSIGHT_PACKAGE_VERSION: &str = "0.5.4";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let mut args = env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help" | "help") {
        print_help();
        return Ok(());
    }

    let command = args.remove(0);
    match command.as_str() {
        "build-hindsight-sidecar" => build_hindsight_sidecar(parse_build_args(&args)?)?,
        "import-hindsight-sidecar" => import_hindsight_sidecar(parse_import_args(&args)?)?,
        "verify-hindsight-sidecars" => verify_hindsight_sidecars()?,
        "smoke-hindsight-sidecar" => smoke_hindsight_sidecar(parse_target_arg(&args)?)?,
        "package-release-binary" => package_release_binary(parse_package_release_args(&args)?)?,
        other => {
            return Err(format!("unknown xtask command `{other}`").into());
        }
    }
    Ok(())
}

fn print_help() {
    println!(
        "\
Usage:
  cargo xtask build-hindsight-sidecar [--spec PATH | --entry-script PATH] [--target TARGET]
  cargo xtask import-hindsight-sidecar --target TARGET --archive PATH [--entry PATH]
  cargo xtask verify-hindsight-sidecars
  cargo xtask smoke-hindsight-sidecar [--target TARGET]
  cargo xtask package-release-binary [--target TARGET] [--release-dir PATH] [--out-dir PATH]

Commands:
  build-hindsight-sidecar    Build the current host sidecar with PyInstaller and update assets.
  import-hindsight-sidecar   Import a CI-built sidecar archive into assets.
  verify-hindsight-sidecars  Verify manifest checksums and archive layouts.
  smoke-hindsight-sidecar    Extract and run the current-host sidecar entry.
  package-release-binary     Package target/release/daat-locus as a cargo-binstall archive.
"
    );
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SidecarManifest {
    #[serde(default)]
    sidecar: Vec<SidecarManifestEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SidecarManifestEntry {
    target: String,
    archive: String,
    archive_kind: String,
    sha256: String,
    entry: String,
    built_with: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    hindsight_version: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArchiveKind {
    TarZst,
    TarGz,
    Zip,
}

impl ArchiveKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::TarZst => "tar.zst",
            Self::TarGz => "tar.gz",
            Self::Zip => "zip",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::TarZst => "tar.zst",
            Self::TarGz => "tar.gz",
            Self::Zip => "zip",
        }
    }
}

#[derive(Debug)]
struct BuildArgs {
    target: String,
    pyinstaller: PyInstallerCommand,
    spec: Option<PathBuf>,
    entry_script: Option<PathBuf>,
    name: String,
    hindsight_version: Option<String>,
}

#[derive(Debug)]
struct PyInstallerCommand {
    program: OsString,
    args: Vec<OsString>,
}

impl PyInstallerCommand {
    fn explicit(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        command
    }

    fn display(&self) -> String {
        let mut parts = Vec::with_capacity(self.args.len() + 1);
        parts.push(PathBuf::from(&self.program).display().to_string());
        parts.extend(
            self.args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned()),
        );
        parts.join(" ")
    }
}

#[derive(Debug)]
struct ImportArgs {
    target: String,
    archive: PathBuf,
    entry: Option<String>,
    built_with: String,
    hindsight_version: Option<String>,
}

#[derive(Debug)]
struct PackageReleaseArgs {
    target: String,
    release_dir: Option<PathBuf>,
    out_dir: PathBuf,
}

#[derive(Debug, Deserialize)]
struct RootManifest {
    package: RootPackage,
}

#[derive(Debug, Deserialize)]
struct RootPackage {
    name: String,
    version: String,
}

fn parse_build_args(raw: &[String]) -> Result<BuildArgs> {
    let repo_root = repo_root();
    let default_spec = repo_root
        .join("hindsight-sidecar")
        .join("hindsight-embed.spec");
    let mut target = None;
    let mut pyinstaller = default_pyinstaller_command();
    let mut spec = default_spec.exists().then_some(default_spec);
    let mut entry_script = None;
    let mut name = DEFAULT_DIST_NAME.to_string();
    let mut hindsight_version = Some(HINDSIGHT_PACKAGE_VERSION.to_string());

    let mut index = 0;
    while index < raw.len() {
        match raw[index].as_str() {
            "--target" => {
                target = Some(next_value(raw, &mut index, "--target")?);
            }
            "--pyinstaller" => {
                pyinstaller =
                    PyInstallerCommand::explicit(next_value(raw, &mut index, "--pyinstaller")?);
            }
            "--spec" => {
                spec = Some(PathBuf::from(next_value(raw, &mut index, "--spec")?));
                entry_script = None;
            }
            "--entry-script" => {
                spec = None;
                entry_script = Some(PathBuf::from(next_value(
                    raw,
                    &mut index,
                    "--entry-script",
                )?));
            }
            "--name" => {
                name = next_value(raw, &mut index, "--name")?;
            }
            "--hindsight-version" => {
                hindsight_version = Some(next_value(raw, &mut index, "--hindsight-version")?);
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => return Err(format!("unknown build-hindsight-sidecar flag `{other}`").into()),
        }
        index += 1;
    }

    if spec.is_some() && entry_script.is_some() {
        return Err("pass only one of --spec or --entry-script".into());
    }
    if spec.is_none() && entry_script.is_none() {
        return Err("missing PyInstaller input; pass --spec PATH or --entry-script PATH".into());
    }

    Ok(BuildArgs {
        target: target.unwrap_or(rustc_host_target()?),
        pyinstaller,
        spec,
        entry_script,
        name,
        hindsight_version,
    })
}

fn parse_import_args(raw: &[String]) -> Result<ImportArgs> {
    let mut target = None;
    let mut archive = None;
    let mut entry = None;
    let mut built_with = "pyinstaller".to_string();
    let mut hindsight_version = None;

    let mut index = 0;
    while index < raw.len() {
        match raw[index].as_str() {
            "--target" => {
                target = Some(next_value(raw, &mut index, "--target")?);
            }
            "--archive" => {
                archive = Some(PathBuf::from(next_value(raw, &mut index, "--archive")?));
            }
            "--entry" => {
                entry = Some(next_value(raw, &mut index, "--entry")?);
            }
            "--built-with" => {
                built_with = next_value(raw, &mut index, "--built-with")?;
            }
            "--hindsight-version" => {
                hindsight_version = Some(next_value(raw, &mut index, "--hindsight-version")?);
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => return Err(format!("unknown import-hindsight-sidecar flag `{other}`").into()),
        }
        index += 1;
    }

    Ok(ImportArgs {
        target: target.ok_or("--target is required")?,
        archive: archive.ok_or("--archive is required")?,
        entry,
        built_with,
        hindsight_version,
    })
}

fn parse_target_arg(raw: &[String]) -> Result<String> {
    let mut target = None;
    let mut index = 0;
    while index < raw.len() {
        match raw[index].as_str() {
            "--target" => {
                target = Some(next_value(raw, &mut index, "--target")?);
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => return Err(format!("unknown target flag `{other}`").into()),
        }
        index += 1;
    }
    target.map_or_else(rustc_host_target, Ok)
}

fn parse_package_release_args(raw: &[String]) -> Result<PackageReleaseArgs> {
    let mut target = None;
    let mut release_dir = None;
    let mut out_dir = None;

    let mut index = 0;
    while index < raw.len() {
        match raw[index].as_str() {
            "--target" => {
                target = Some(next_value(raw, &mut index, "--target")?);
            }
            "--release-dir" => {
                release_dir = Some(PathBuf::from(next_value(raw, &mut index, "--release-dir")?));
            }
            "--out-dir" => {
                out_dir = Some(PathBuf::from(next_value(raw, &mut index, "--out-dir")?));
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => return Err(format!("unknown package-release-binary flag `{other}`").into()),
        }
        index += 1;
    }

    Ok(PackageReleaseArgs {
        target: target.unwrap_or(rustc_host_target()?),
        release_dir,
        out_dir: out_dir.unwrap_or_else(|| PathBuf::from(DEFAULT_RELEASE_OUT_DIR)),
    })
}

fn next_value(raw: &[String], index: &mut usize, flag: &str) -> Result<String> {
    *index += 1;
    raw.get(*index)
        .cloned()
        .ok_or_else(|| format!("{flag} requires a value").into())
}

fn build_hindsight_sidecar(args: BuildArgs) -> Result<()> {
    let host = rustc_host_target()?;
    if args.target != host {
        return Err(format!(
            "PyInstaller cannot cross-build sidecars: requested target `{}`, host is `{host}`. Use CI on the target platform, then `cargo xtask import-hindsight-sidecar`.",
            args.target
        )
        .into());
    }

    let repo_root = repo_root();
    let assets_dir = assets_dir();
    fs::create_dir_all(&assets_dir)?;

    let work_root = repo_root
        .join("target")
        .join("xtask")
        .join("hindsight-sidecar")
        .join(&args.target);
    reset_dir(&work_root)?;

    let dist_dir = work_root.join("dist");
    let build_dir = work_root.join("build");
    let spec_dir = work_root.join("spec");
    let stage_root = work_root.join("stage");

    run_pyinstaller(&args, &dist_dir, &build_dir, &spec_dir)?;

    let dist_app_dir = dist_dir.join(&args.name);
    if !dist_app_dir.is_dir() {
        return Err(format!(
            "PyInstaller output directory not found: {}. If the spec uses a different name, pass --name.",
            dist_app_dir.display()
        )
        .into());
    }

    let staged_bin_dir = stage_root.join("bin");
    fs::create_dir_all(&staged_bin_dir)?;
    copy_dir_contents(&dist_app_dir, &staged_bin_dir)?;

    let entry = default_sidecar_entry(&args.target);
    ensure_safe_relative_path("sidecar entry", &entry)?;
    if !stage_root.join(&entry).is_file() {
        return Err(format!(
            "staged sidecar is missing entry `{entry}` under {}",
            stage_root.display()
        )
        .into());
    }

    let archive_kind = ArchiveKind::TarZst;
    let archive_name = format!("{}.{}", args.target, archive_kind.extension());
    let archive_path = assets_dir.join(&archive_name);
    if archive_path.exists() {
        fs::remove_file(&archive_path)?;
    }
    archive_stage(&stage_root, &archive_path, archive_kind)?;
    verify_archive_contains_entry(&archive_path, archive_kind, &entry)?;

    let sha256 = sha256_file(&archive_path)?;
    upsert_manifest_entry(SidecarManifestEntry {
        target: args.target.clone(),
        archive: archive_name,
        archive_kind: archive_kind.as_str().to_string(),
        sha256,
        entry,
        built_with: "pyinstaller".to_string(),
        hindsight_version: args.hindsight_version,
    })?;

    println!(
        "built Hindsight sidecar for {} at {}",
        args.target,
        archive_path.display()
    );
    Ok(())
}

fn run_pyinstaller(
    args: &BuildArgs,
    dist_dir: &Path,
    build_dir: &Path,
    spec_dir: &Path,
) -> Result<()> {
    let mut command = args.pyinstaller.command();
    command
        .arg("--noconfirm")
        .arg("--clean")
        .arg("--distpath")
        .arg(dist_dir)
        .arg("--workpath")
        .arg(build_dir);

    if let Some(spec) = &args.spec {
        command.arg(spec);
    } else if let Some(entry_script) = &args.entry_script {
        command
            .arg("--onedir")
            .arg("--name")
            .arg(&args.name)
            .arg("--specpath")
            .arg(spec_dir)
            .arg(entry_script);
    }

    let status = command.status().map_err(|err| {
        format!(
            "failed to spawn PyInstaller command `{}`: {err}",
            args.pyinstaller.display()
        )
    })?;
    if !status.success() {
        return Err(format!("PyInstaller failed with status {status}").into());
    }
    Ok(())
}

fn import_hindsight_sidecar(args: ImportArgs) -> Result<()> {
    if !args.archive.is_file() {
        return Err(format!("archive does not exist: {}", args.archive.display()).into());
    }

    let archive_kind = archive_kind_from_path(&args.archive).ok_or_else(|| {
        format!(
            "unsupported sidecar archive extension: {}",
            args.archive.display()
        )
    })?;
    let entry = args
        .entry
        .unwrap_or_else(|| default_sidecar_entry(&args.target));
    ensure_safe_relative_path("sidecar entry", &entry)?;
    verify_archive_contains_entry(&args.archive, archive_kind, &entry)?;

    let assets_dir = assets_dir();
    fs::create_dir_all(&assets_dir)?;
    let archive_name = format!("{}.{}", args.target, archive_kind.extension());
    let dest = assets_dir.join(&archive_name);
    copy_file_if_different(&args.archive, &dest)?;
    let sha256 = sha256_file(&dest)?;

    upsert_manifest_entry(SidecarManifestEntry {
        target: args.target.clone(),
        archive: archive_name,
        archive_kind: archive_kind.as_str().to_string(),
        sha256,
        entry,
        built_with: args.built_with,
        hindsight_version: args.hindsight_version,
    })?;

    println!(
        "imported Hindsight sidecar for {} into {}",
        args.target,
        dest.display()
    );
    Ok(())
}

fn verify_hindsight_sidecars() -> Result<()> {
    let manifest = load_manifest()?;
    let assets_dir = assets_dir();
    for entry in &manifest.sidecar {
        ensure_safe_relative_path("sidecar archive", &entry.archive)?;
        ensure_safe_relative_path("sidecar entry", &entry.entry)?;
        let archive_path = assets_dir.join(&entry.archive);
        if !archive_path.is_file() {
            return Err(format!(
                "manifest target `{}` points to missing archive {}",
                entry.target,
                archive_path.display()
            )
            .into());
        }
        let actual = sha256_file(&archive_path)?;
        if !actual.eq_ignore_ascii_case(&entry.sha256) {
            return Err(format!(
                "checksum mismatch for {}: manifest {}, actual {}",
                archive_path.display(),
                entry.sha256,
                actual
            )
            .into());
        }
        let archive_kind = archive_kind_from_manifest(entry)?;
        verify_archive_contains_entry(&archive_path, archive_kind, &entry.entry)?;
    }
    println!("verified {} Hindsight sidecar(s)", manifest.sidecar.len());
    Ok(())
}

fn smoke_hindsight_sidecar(target: String) -> Result<()> {
    let host = rustc_host_target()?;
    if target != host {
        return Err(format!(
            "cannot smoke-test target `{target}` on host `{host}`; import and verify are cross-platform, execution smoke tests are not"
        )
        .into());
    }

    let manifest = load_manifest()?;
    let entry = manifest
        .sidecar
        .iter()
        .find(|entry| entry.target == target)
        .cloned()
        .ok_or_else(|| format!("manifest has no sidecar entry for target `{target}`"))?;
    let archive_kind = archive_kind_from_manifest(&entry)?;
    let archive_path = assets_dir().join(&entry.archive);
    verify_archive_contains_entry(&archive_path, archive_kind, &entry.entry)?;

    let smoke_root = repo_root()
        .join("target")
        .join("xtask")
        .join("hindsight-sidecar-smoke")
        .join(&target);
    let extract_root = smoke_root.join("extract");
    let home_root = smoke_root.join("home");
    reset_dir(&smoke_root)?;
    fs::create_dir_all(&extract_root)?;
    fs::create_dir_all(&home_root)?;

    extract_archive(&archive_path, archive_kind, &extract_root)?;
    let executable = extract_root.join(&entry.entry);
    if !executable.is_file() {
        return Err(format!(
            "extracted sidecar is missing entry {}",
            executable.display()
        )
        .into());
    }

    run_sidecar_command(&executable, ["--help"], None)?;
    let profile = "daat-locus-sidecar-smoke";
    run_sidecar_command(
        &executable,
        [
            "profile",
            "create",
            profile,
            "--port",
            "18888",
            "--env",
            "HINDSIGHT_API_DATABASE_URL=pg0://daat-locus-sidecar-smoke",
        ],
        Some(&home_root),
    )?;
    run_sidecar_command(
        &executable,
        ["profile", "delete", profile],
        Some(&home_root),
    )?;

    println!(
        "smoke-tested Hindsight sidecar for {target} using {}",
        archive_path.display()
    );
    Ok(())
}

fn package_release_binary(args: PackageReleaseArgs) -> Result<()> {
    let package = load_root_package()?;
    let release_dir = match args.release_dir {
        Some(path) => repo_relative_path(path),
        None => default_release_dir(&args.target)?,
    };
    let binary_name = release_binary_name(&package.name, &args.target);
    let binary_path = release_dir.join(&binary_name);
    if !binary_path.is_file() {
        return Err(format!(
            "release binary does not exist: {}. Run `DAAT_LOCUS_REQUIRE_HINDSIGHT_SIDECAR=1 cargo build --release --locked` first.",
            binary_path.display()
        )
        .into());
    }

    let package_dir_name = release_package_dir_name(&package.name, &package.version, &args.target);
    let work_root = repo_root()
        .join("target")
        .join("xtask")
        .join("release-package")
        .join(&args.target);
    let stage_root = work_root.join("stage");
    let package_dir = stage_root.join(&package_dir_name);
    reset_dir(&work_root)?;
    fs::create_dir_all(&package_dir)?;

    let staged_binary = package_dir.join(&binary_name);
    fs::copy(&binary_path, &staged_binary)?;
    fs::set_permissions(&staged_binary, fs::metadata(&binary_path)?.permissions())?;

    let out_dir = repo_relative_path(args.out_dir);
    fs::create_dir_all(&out_dir)?;
    let archive_name = release_archive_name(&package.name, &package.version, &args.target);
    let archive_path = out_dir.join(&archive_name);
    if archive_path.exists() {
        fs::remove_file(&archive_path)?;
    }
    archive_stage(&stage_root, &archive_path, ArchiveKind::TarZst)?;

    let archive_entry = format!("{package_dir_name}/{binary_name}");
    if !tar_zst_contains_entry(&archive_path, &archive_entry)? {
        return Err(format!(
            "release archive {} does not contain required entry `{archive_entry}`",
            archive_path.display()
        )
        .into());
    }

    println!(
        "packaged release binary for {} at {}",
        args.target,
        archive_path.display()
    );
    Ok(())
}

fn run_sidecar_command<const N: usize>(
    executable: &Path,
    args: [&str; N],
    home: Option<&Path>,
) -> Result<()> {
    let mut command = Command::new(executable);
    command.args(args);
    if let Some(home) = home {
        command.env("HOME", home);
        command.env("USERPROFILE", home);
    }
    command.env("PYTHONUTF8", "1");
    command.env("PYTHONIOENCODING", "utf-8");
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "sidecar command `{}` failed with status {status}",
            executable.display()
        )
        .into())
    }
}

fn upsert_manifest_entry(entry: SidecarManifestEntry) -> Result<()> {
    let mut manifest = load_manifest()?;
    manifest
        .sidecar
        .retain(|existing| existing.target != entry.target);
    manifest.sidecar.push(entry);
    manifest
        .sidecar
        .sort_by(|left, right| left.target.cmp(&right.target));
    write_manifest(&manifest)
}

fn load_manifest() -> Result<SidecarManifest> {
    let path = manifest_path();
    if !path.exists() {
        return Ok(SidecarManifest::default());
    }
    let text = fs::read_to_string(&path)?;
    Ok(toml::from_str(&text)?)
}

fn write_manifest(manifest: &SidecarManifest) -> Result<()> {
    let path = manifest_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, toml::to_string_pretty(manifest)?)?;
    Ok(())
}

fn archive_kind_from_manifest(entry: &SidecarManifestEntry) -> Result<ArchiveKind> {
    match entry.archive_kind.as_str() {
        "tar.zst" => Ok(ArchiveKind::TarZst),
        "tar.gz" => Ok(ArchiveKind::TarGz),
        "zip" => Ok(ArchiveKind::Zip),
        other => Err(format!(
            "manifest target `{}` uses unsupported archive_kind `{other}`",
            entry.target
        )
        .into()),
    }
}

fn archive_kind_from_path(path: &Path) -> Option<ArchiveKind> {
    let file_name = path.file_name()?.to_str()?;
    if file_name.ends_with(".tar.zst") || file_name.ends_with(".tzst") {
        Some(ArchiveKind::TarZst)
    } else if file_name.ends_with(".tar.gz") || file_name.ends_with(".tgz") {
        Some(ArchiveKind::TarGz)
    } else if file_name.ends_with(".zip") {
        Some(ArchiveKind::Zip)
    } else {
        None
    }
}

fn archive_stage(stage_root: &Path, archive_path: &Path, kind: ArchiveKind) -> Result<()> {
    match kind {
        ArchiveKind::TarZst => archive_stage_tar_zst(stage_root, archive_path),
        ArchiveKind::TarGz => archive_stage_tar_gz(stage_root, archive_path),
        ArchiveKind::Zip => archive_stage_zip(stage_root, archive_path),
    }
}

fn extract_archive(archive_path: &Path, kind: ArchiveKind, target_dir: &Path) -> Result<()> {
    match kind {
        ArchiveKind::TarZst => extract_tar_zst(archive_path, target_dir),
        ArchiveKind::TarGz => extract_tar_gz(archive_path, target_dir),
        ArchiveKind::Zip => extract_zip(archive_path, target_dir),
    }
}

fn extract_tar_zst(archive_path: &Path, target_dir: &Path) -> Result<()> {
    let file = fs::File::open(archive_path)?;
    let decoder = zstd::stream::read::Decoder::new(file)?;
    extract_tar(decoder, target_dir)
}

fn extract_tar_gz(archive_path: &Path, target_dir: &Path) -> Result<()> {
    let file = fs::File::open(archive_path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    extract_tar(decoder, target_dir)
}

fn extract_tar<R: std::io::Read>(reader: R, target_dir: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(reader);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        ensure_safe_relative_path("tar entry", &slash_path_without_cur_dir(&path))?;
        let out_path = target_dir.join(&path);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }
        entry.unpack(out_path)?;
    }
    Ok(())
}

fn extract_zip(archive_path: &Path, target_dir: &Path) -> Result<()> {
    let file = fs::File::open(archive_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        let Some(path) = file.enclosed_name() else {
            return Err(format!("zip entry `{}` is not safely enclosed", file.name()).into());
        };
        ensure_safe_relative_path("zip entry", &slash_path_without_cur_dir(&path))?;
        let out_path = target_dir.join(path);
        if file.is_dir() {
            fs::create_dir_all(&out_path)?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = fs::File::create(&out_path)?;
        std::io::copy(&mut file, &mut out)?;
        #[cfg(unix)]
        if let Some(mode) = file.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&out_path, fs::Permissions::from_mode(mode))?;
        }
    }
    Ok(())
}

fn archive_stage_tar_gz(stage_root: &Path, archive_path: &Path) -> Result<()> {
    let file = fs::File::create(archive_path)?;
    let encoder = GzEncoder::new(file, Compression::default());
    let encoder = archive_stage_tar(stage_root, encoder)?;
    encoder.finish()?;
    Ok(())
}

fn archive_stage_tar_zst(stage_root: &Path, archive_path: &Path) -> Result<()> {
    let file = fs::File::create(archive_path)?;
    let mut encoder = zstd::stream::write::Encoder::new(file, 19)?;
    encoder.multithread(zstd_worker_count())?;
    let encoder = archive_stage_tar(stage_root, encoder)?;
    encoder.finish()?;
    Ok(())
}

fn zstd_worker_count() -> u32 {
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .clamp(1, 8) as u32
}

fn archive_stage_tar<W: std::io::Write>(stage_root: &Path, writer: W) -> Result<W> {
    let encoder = writer;
    let mut builder = tar::Builder::new(encoder);
    for entry in fs::read_dir(stage_root)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            builder.append_dir_all(Path::new(&name), &path)?;
        } else {
            builder.append_path_with_name(&path, Path::new(&name))?;
        }
    }
    let encoder = builder.into_inner()?;
    Ok(encoder)
}

fn archive_stage_zip(stage_root: &Path, archive_path: &Path) -> Result<()> {
    let file = fs::File::create(archive_path)?;
    let mut writer = ZipWriter::new(file);
    add_zip_entries(&mut writer, stage_root, stage_root)?;
    writer.finish()?;
    Ok(())
}

fn add_zip_entries(writer: &mut ZipWriter<fs::File>, base: &Path, dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let rel = slash_path(path.strip_prefix(base)?);
        if path.is_dir() {
            writer.add_directory(format!("{rel}/"), zip_options_for_path(&path)?)?;
            add_zip_entries(writer, base, &path)?;
        } else {
            writer.start_file(rel, zip_options_for_path(&path)?)?;
            let mut file = fs::File::open(&path)?;
            std::io::copy(&mut file, writer)?;
        }
    }
    Ok(())
}

fn zip_options_for_path(path: &Path) -> Result<SimpleFileOptions> {
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Ok(options.unix_permissions(fs::metadata(path)?.permissions().mode()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(options)
    }
}

fn verify_archive_contains_entry(
    archive_path: &Path,
    kind: ArchiveKind,
    expected_entry: &str,
) -> Result<()> {
    ensure_safe_relative_path("sidecar entry", expected_entry)?;
    let found = match kind {
        ArchiveKind::TarZst => tar_zst_contains_entry(archive_path, expected_entry)?,
        ArchiveKind::TarGz => tar_gz_contains_entry(archive_path, expected_entry)?,
        ArchiveKind::Zip => zip_contains_entry(archive_path, expected_entry)?,
    };
    if found {
        Ok(())
    } else {
        Err(format!(
            "sidecar archive {} does not contain required entry `{expected_entry}`",
            archive_path.display()
        )
        .into())
    }
}

fn tar_zst_contains_entry(archive_path: &Path, expected_entry: &str) -> Result<bool> {
    let file = fs::File::open(archive_path)?;
    let decoder = zstd::stream::read::Decoder::new(file)?;
    tar_contains_entry(decoder, expected_entry)
}

fn tar_gz_contains_entry(archive_path: &Path, expected_entry: &str) -> Result<bool> {
    let file = fs::File::open(archive_path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    tar_contains_entry(decoder, expected_entry)
}

fn tar_contains_entry<R: std::io::Read>(reader: R, expected_entry: &str) -> Result<bool> {
    let mut archive = tar::Archive::new(reader);
    for entry in archive.entries()? {
        let entry = entry?;
        if archive_path_matches(&entry.path()?, expected_entry) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn zip_contains_entry(archive_path: &Path, expected_entry: &str) -> Result<bool> {
    let file = fs::File::open(archive_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    for index in 0..archive.len() {
        let file = archive.by_index(index)?;
        let Some(path) = file.enclosed_name() else {
            return Err(format!("zip entry `{}` is not safely enclosed", file.name()).into());
        };
        if archive_path_matches(&path, expected_entry) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn archive_path_matches(path: &Path, expected_entry: &str) -> bool {
    slash_path_without_cur_dir(path) == expected_entry
}

fn copy_dir_contents(from: &Path, to: &Path) -> Result<()> {
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let source = entry.path();
        let dest = to.join(entry.file_name());
        if source.is_dir() {
            fs::create_dir_all(&dest)?;
            copy_dir_contents(&source, &dest)?;
        } else {
            fs::copy(&source, &dest)?;
            fs::set_permissions(&dest, fs::metadata(&source)?.permissions())?;
        }
    }
    Ok(())
}

fn copy_file_if_different(from: &Path, to: &Path) -> Result<()> {
    if let (Ok(from_canon), Ok(to_canon)) = (from.canonicalize(), to.canonicalize())
        && from_canon == to_canon
    {
        return Ok(());
    }
    fs::copy(from, to)?;
    Ok(())
}

fn reset_dir(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path)?;
    }
    fs::create_dir_all(path)?;
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn ensure_safe_relative_path(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(format!("{label} must not be empty").into());
    }
    if value.contains('\\') {
        return Err(format!("{label} must use '/' separators: {value}").into());
    }
    let path = Path::new(value);
    if path.is_absolute() {
        return Err(format!("{label} must be relative: {value}").into());
    }
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!("{label} escapes its base directory: {value}").into());
            }
        }
    }
    Ok(())
}

fn default_sidecar_entry(target: &str) -> String {
    if is_windows_target(target) {
        "bin/hindsight-embed.exe".to_string()
    } else {
        "bin/hindsight-embed".to_string()
    }
}

fn is_windows_target(target: &str) -> bool {
    target.contains("windows")
}

fn rustc_host_target() -> Result<String> {
    let output = Command::new("rustc").arg("-vV").output()?;
    if !output.status.success() {
        return Err("rustc -vV failed".into());
    }
    let stdout = String::from_utf8(output.stdout)?;
    for line in stdout.lines() {
        if let Some(host) = line.strip_prefix("host: ") {
            return Ok(host.trim().to_string());
        }
    }
    Err("rustc -vV output did not contain a host target".into())
}

fn default_pyinstaller_command() -> PyInstallerCommand {
    if command_exists("uvx") {
        return PyInstallerCommand {
            program: OsString::from("uvx"),
            args: vec![
                OsString::from("--python"),
                OsString::from(HINDSIGHT_PYTHON),
                OsString::from("--from"),
                OsString::from("pyinstaller"),
                OsString::from("--with"),
                OsString::from(HINDSIGHT_EMBED_PACKAGE),
                OsString::from("--with"),
                OsString::from(HINDSIGHT_API_PACKAGE),
                OsString::from("pyinstaller"),
            ],
        };
    }
    if command_exists("uv") {
        return PyInstallerCommand {
            program: OsString::from("uv"),
            args: vec![
                OsString::from("tool"),
                OsString::from("run"),
                OsString::from("--python"),
                OsString::from(HINDSIGHT_PYTHON),
                OsString::from("--from"),
                OsString::from("pyinstaller"),
                OsString::from("--with"),
                OsString::from(HINDSIGHT_EMBED_PACKAGE),
                OsString::from("--with"),
                OsString::from(HINDSIGHT_API_PACKAGE),
                OsString::from("pyinstaller"),
            ],
        };
    }
    if command_exists("pyinstaller") {
        return PyInstallerCommand::explicit("pyinstaller");
    }
    PyInstallerCommand::explicit("pyinstaller")
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

fn executable_candidate_exists(path: &Path) -> bool {
    if path.is_file() && is_executable(path) {
        return true;
    }
    #[cfg(windows)]
    {
        if path.extension().is_some() {
            return false;
        }
        return windows_path_extensions().into_iter().any(|extension| {
            let candidate = path.with_extension(extension);
            candidate.is_file() && is_executable(&candidate)
        });
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(windows)]
fn windows_path_extensions() -> Vec<String> {
    env::var_os("PATHEXT")
        .map(|value| {
            env::split_paths(&value)
                .filter_map(|path| path.as_os_str().to_str().map(str::to_string))
                .map(|extension| extension.trim_start_matches('.').to_string())
                .filter(|extension| !extension.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|extensions| !extensions.is_empty())
        .unwrap_or_else(|| {
            ["COM", "EXE", "BAT", "CMD"]
                .into_iter()
                .map(str::to_string)
                .collect()
        })
}

fn load_root_package() -> Result<RootPackage> {
    let manifest_path = repo_root().join("Cargo.toml");
    let text = fs::read_to_string(&manifest_path)?;
    let manifest: RootManifest = toml::from_str(&text)?;
    Ok(manifest.package)
}

fn default_release_dir(target: &str) -> Result<PathBuf> {
    let target_dir = repo_root().join("target");
    if target == rustc_host_target()? {
        Ok(target_dir.join("release"))
    } else {
        Ok(target_dir.join(target).join("release"))
    }
}

fn release_binary_name(name: &str, target: &str) -> String {
    if is_windows_target(target) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

fn release_package_dir_name(name: &str, version: &str, target: &str) -> String {
    format!("{name}-{version}-{target}")
}

fn release_archive_name(name: &str, version: &str, target: &str) -> String {
    format!(
        "{}.tar.zst",
        release_package_dir_name(name, version, target)
    )
}

fn repo_relative_path(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        repo_root().join(path)
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask is under repo root")
        .to_path_buf()
}

fn assets_dir() -> PathBuf {
    repo_root().join("assets").join("hindsight-sidecars")
}

fn manifest_path() -> PathBuf {
    assets_dir().join(SIDECAR_MANIFEST)
}

fn slash_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn slash_path_without_cur_dir(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            Component::CurDir => None,
            _ => Some(component.as_os_str().to_string_lossy().into_owned()),
        })
        .collect::<Vec<_>>()
        .join("/")
}
