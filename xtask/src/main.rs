use std::{
    env,
    error::Error,
    fs,
    path::{Component, Path, PathBuf},
    process::{Command, ExitCode},
};

use serde::Deserialize;

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const DEFAULT_RELEASE_OUT_DIR: &str = "dist";

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
        "package-release-binary" => package_release_binary(parse_package_release_args(&args)?)?,
        other => return Err(format!("unknown xtask command `{other}`").into()),
    }
    Ok(())
}

fn print_help() {
    println!(
        "\
Usage:
  cargo xtask package-release-binary [--target TARGET] [--release-dir PATH] [--out-dir PATH]

Commands:
  package-release-binary  Package an already-built release binary for cargo-binstall.",
    );
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

fn parse_package_release_args(raw: &[String]) -> Result<PackageReleaseArgs> {
    let mut target = None;
    let mut release_dir = None;
    let mut out_dir = PathBuf::from(DEFAULT_RELEASE_OUT_DIR);

    let mut index = 0;
    while index < raw.len() {
        match raw[index].as_str() {
            "--target" => target = Some(next_value(raw, &mut index, "--target")?),
            "--release-dir" => {
                release_dir = Some(PathBuf::from(next_value(raw, &mut index, "--release-dir")?))
            }
            "--out-dir" => out_dir = PathBuf::from(next_value(raw, &mut index, "--out-dir")?),
            other => return Err(format!("unknown package-release-binary flag `{other}`").into()),
        }
        index += 1;
    }

    Ok(PackageReleaseArgs {
        target: target.unwrap_or(rustc_host_target()?),
        release_dir,
        out_dir,
    })
}

fn next_value(raw: &[String], index: &mut usize, flag: &str) -> Result<String> {
    *index += 1;
    raw.get(*index)
        .cloned()
        .ok_or_else(|| format!("{flag} requires a value").into())
}

fn package_release_binary(args: PackageReleaseArgs) -> Result<()> {
    ensure_safe_relative_path("target triple", Path::new(&args.target))?;
    let manifest = read_root_manifest()?;
    let repo = repo_root();
    let release_dir = args
        .release_dir
        .unwrap_or_else(|| repo.join("target").join(&args.target).join("release"));
    let binary_name = binary_name(&manifest.package.name);
    let binary_path = release_dir.join(&binary_name);
    if !binary_path.is_file() {
        return Err(format!(
            "release binary missing at {}; build it first with `cargo build --release --target {}`",
            binary_path.display(),
            args.target
        )
        .into());
    }

    let package_dir_name = format!(
        "{}-{}-{}",
        manifest.package.name, manifest.package.version, args.target
    );
    let stage_root = repo.join("target").join("xtask").join("package-release");
    if stage_root.exists() {
        fs::remove_dir_all(&stage_root)?;
    }
    let package_dir = stage_root.join(&package_dir_name);
    fs::create_dir_all(&package_dir)?;
    fs::copy(&binary_path, package_dir.join(&binary_name))?;

    let out_dir = if args.out_dir.is_absolute() {
        args.out_dir.clone()
    } else {
        repo.join(&args.out_dir)
    };
    fs::create_dir_all(&out_dir)?;
    let archive_path = out_dir.join(format!(
        "{}-{}-{}.tar.zst",
        manifest.package.name, manifest.package.version, args.target
    ));
    if archive_path.exists() {
        fs::remove_file(&archive_path)?;
    }
    archive_tar_zst(&stage_root, &archive_path)?;

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

fn archive_tar_zst(stage_root: &Path, archive_path: &Path) -> Result<()> {
    if let Some(parent) = archive_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = fs::File::create(archive_path)?;
    let mut encoder = zstd::stream::write::Encoder::new(file, 19)?;
    encoder.multithread(zstd_worker_count())?;
    let mut builder = tar::Builder::new(encoder);
    builder.append_dir_all(".", stage_root)?;
    let encoder = builder.into_inner()?;
    encoder.finish()?;
    Ok(())
}

fn tar_zst_contains_entry(archive_path: &Path, expected_entry: &str) -> Result<bool> {
    let file = fs::File::open(archive_path)?;
    let decoder = zstd::stream::read::Decoder::new(file)?;
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries()? {
        let entry = entry?;
        if archive_entry_path(&entry.path()?) == expected_entry {
            return Ok(true);
        }
    }
    Ok(false)
}

fn archive_entry_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn zstd_worker_count() -> u32 {
    std::thread::available_parallelism()
        .map(|count| count.get().clamp(1, 8) as u32)
        .unwrap_or(1)
}

fn read_root_manifest() -> Result<RootManifest> {
    let text = fs::read_to_string(repo_root().join("Cargo.toml"))?;
    Ok(toml::from_str(&text)?)
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask manifest has parent")
        .to_path_buf()
}

fn rustc_host_target() -> Result<String> {
    let output = Command::new("rustc").arg("-vV").output()?;
    if !output.status.success() {
        return Err("rustc -vV failed".into());
    }
    let stdout = String::from_utf8(output.stdout)?;
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("host: ").map(str::to_string))
        .ok_or_else(|| "rustc -vV did not report host target".into())
}

fn binary_name(package_name: &str) -> String {
    if cfg!(windows) {
        format!("{package_name}.exe")
    } else {
        package_name.to_string()
    }
}

fn ensure_safe_relative_path(label: &str, path: &Path) -> Result<()> {
    if path.is_absolute() {
        return Err(format!("{label} must be relative, got {}", path.display()).into());
    }
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            _ => {
                return Err(
                    format!("{label} contains unsafe component: {}", path.display()).into(),
                );
            }
        }
    }
    Ok(())
}
