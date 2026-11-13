//! Distribution builds select native assets or an independently embedded browser UI.
mod android;
#[cfg(target_os = "macos")]
mod macos_signing;
mod reporting;
mod windows;

use clap::{Args, Parser, Subcommand};
use reporting::{packaged, status};
use serde::Deserialize;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const UI_MARKER: &[u8] = b"DaatLocus.EmbeddedWebUI.v1";
const NATIVE_UI_MARKER: &[u8] = b"DaatLocus.NativeWebUI.v1";
#[derive(Parser)]
#[command(name = "xtask", about = "Build and package Daat Locus.")]
struct Cli {
    #[command(subcommand)]
    command: Task,
}
#[derive(Subcommand)]
enum Task {
    Package {
        #[command(subcommand)]
        format: Format,
    },
}
#[derive(Subcommand)]
enum Format {
    Binary(Package),
    Windows(Package),
    Macos(Package),
    /// Build an Android APK with the embedded backend and WebUI.
    Android(android::Package),
}
#[derive(Args)]
struct Package {
    #[arg(long)]
    target: Option<String>,
    #[arg(long)]
    release_dir: Option<PathBuf>,
    #[arg(long)]
    out_dir: Option<PathBuf>,
    #[arg(long)]
    skip_build: bool,
    /// Override the persistent local macOS signing identity with a Keychain identity.
    #[arg(long)]
    macos_signing_identity: Option<String>,
}
#[derive(Deserialize)]
struct Manifest {
    package: Metadata,
}
#[derive(Deserialize)]
struct Metadata {
    name: String,
    version: String,
    authors: Vec<String>,
}
impl Metadata {
    // Native installer fields accept numeric versions, not SemVer prerelease/build suffixes.
    fn installer_version(&self) -> &str {
        self.version.split(['-', '+']).next().unwrap()
    }
}
struct Context {
    root: PathBuf,
    meta: Metadata,
    target: String,
    release: PathBuf,
    output: PathBuf,
    macos_signing_identity: Option<String>,
}
fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}
fn command(command: &mut Command) -> Result<()> {
    let status = command.status()?;
    if !status.success() {
        return Err(format!("Command failed: {command:?}").into());
    }
    Ok(())
}
fn output(command: &mut Command) -> Result<String> {
    let result = command.output()?;
    if !result.status.success() {
        return Err(String::from_utf8_lossy(&result.stderr).into_owned().into());
    }
    Ok(String::from_utf8(result.stdout)?)
}
fn run() -> Result<()> {
    let started = std::time::Instant::now();
    let cli = Cli::parse();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let format = match cli.command {
        Task::Package { format } => format,
    };
    let meta: Manifest = toml::from_str(&fs::read_to_string(root.join("Cargo.toml"))?)?;
    let (args, kind) = match format {
        Format::Binary(args) => (args, "binary"),
        Format::Windows(args) => (args, "windows"),
        Format::Macos(args) => (args, "macos"),
        Format::Android(args) => {
            android::package(&root, &meta.package, args)?;
            status(
                "Finished",
                &format!("packaging in {:.2}s", started.elapsed().as_secs_f64()),
            );
            return Ok(());
        }
    };
    let target = args.target.unwrap_or_else(|| {
        output(Command::new("rustc").arg("-vV"))
            .expect("Rust toolchain")
            .lines()
            .find_map(|line| line.strip_prefix("host: ").map(str::to_owned))
            .expect("host target")
    });
    validate_target(&target)?;
    if kind == "windows" && (!cfg!(windows) || !target.contains("windows-msvc")) {
        return Err("Windows installers require a Windows MSVC host/target".into());
    }
    if kind == "macos" && (!cfg!(target_os = "macos") || !target.contains("apple-darwin")) {
        return Err("macOS installers require a macOS host/target".into());
    }
    if kind != "macos" && args.macos_signing_identity.is_some() {
        return Err("--macos-signing-identity requires package macos".into());
    }
    status(
        "Packaging",
        &format!(
            "{} v{} ({kind}, {target})",
            meta.package.name, meta.package.version
        ),
    );
    let native = kind != "binary";
    if !args.skip_build {
        command(
            Command::new("bun")
                .current_dir(&root)
                .arg("scripts/build-webui.ts"),
        )?;
        let mut build = Command::new("cargo");
        build
            .current_dir(&root)
            .args([
                "build",
                "--release",
                "--locked",
                "--target",
                &target,
                "-p",
                "daat-locus",
            ])
            .env_remove("DAAT_LOCUS_BUILD_WEBUI");
        if native {
            build.arg("--no-default-features");
        }
        command(&mut build)?;
        if native {
            command(Command::new("cargo").current_dir(&root).args([
                "build",
                "--release",
                "--locked",
                "--target",
                &target,
                "-p",
                "daat-locus-app",
                "--features",
                "custom-protocol",
            ]))?;
        }
    }
    let release = args
        .release_dir
        .map(|path| absolute(&root, path))
        .unwrap_or_else(|| root.join("target").join(&target).join("release"));
    let output = args
        .out_dir
        .map(|path| absolute(&root, path))
        .unwrap_or_else(|| release.join("package"));
    fs::create_dir_all(&output)?;
    let context = Context {
        root,
        meta: meta.package,
        target,
        release,
        output,
        macos_signing_identity: args.macos_signing_identity,
    };
    let binary = context.binary("daat-locus");
    if native {
        require_native(&context.binary("daat-locus-app"))?;
        if contains_ui_marker(fs::File::open(&binary)?)? {
            return Err("Native companion service must be built with --no-default-features to avoid duplicate WebUI assets".into());
        }
    } else {
        require_embedded(&binary)?;
    }
    match kind {
        "binary" => archive_with_assets(&context)?,
        "windows" => windows::package(&context)?,
        _ => macos(&context)?,
    };
    status(
        "Finished",
        &format!("packaging in {:.2}s", started.elapsed().as_secs_f64()),
    );
    Ok(())
}
fn absolute(root: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}
fn validate_target(target: &str) -> Result<()> {
    if target.is_empty()
        || target.contains("..")
        || !target
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err("Invalid target triple".into());
    }
    Ok(())
}

impl Context {
    fn binary(&self, name: &str) -> PathBuf {
        self.release.join(format!(
            "{name}{}",
            if self.target.contains("windows") {
                ".exe"
            } else {
                ""
            }
        ))
    }
    fn stem(&self) -> String {
        format!("{}-{}-{}", self.meta.name, self.meta.version, self.target)
    }
    fn work(&self) -> Result<PathBuf> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let path = self
            .root
            .join("target/packaging")
            .join(format!("{}-{stamp}", std::process::id()));
        fs::create_dir_all(&path)?;
        Ok(path)
    }
}
fn require_embedded(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err("Distribution binaries cannot be symlinks".into());
    }
    if !contains_ui_marker(fs::File::open(path)?)? {
        return Err(format!(
            "{} has no embedded WebUI; rebuild with default settings",
            path.display()
        )
        .into());
    }
    Ok(())
}
fn require_native(path: &Path) -> Result<()> {
    if !contains_native_ui_marker(fs::File::open(path)?)? {
        return Err(
            "Native WebUI client is missing; build daat-locus-app with --features custom-protocol"
                .into(),
        );
    }
    Ok(())
}
fn contains_ui_marker(reader: impl std::io::Read) -> std::io::Result<bool> {
    contains_marker(reader, UI_MARKER)
}
fn contains_native_ui_marker(reader: impl std::io::Read) -> std::io::Result<bool> {
    contains_marker(reader, NATIVE_UI_MARKER)
}
fn contains_marker(mut reader: impl std::io::Read, marker: &[u8]) -> std::io::Result<bool> {
    // Android debug libraries can exceed a gigabyte. Scan without buffering the
    // whole library and retain the overlap when the marker spans two reads.
    let mut buffer = vec![0; 64 * 1024 + marker.len()];
    let mut retained = 0;
    loop {
        let read = reader.read(&mut buffer[retained..])?;
        if read == 0 {
            return Ok(false);
        }
        let length = retained + read;
        if buffer[..length]
            .windows(marker.len())
            .any(|part| part == marker)
        {
            return Ok(true);
        }
        retained = length.min(marker.len() - 1);
        buffer.copy_within(length - retained..length, 0);
    }
}
fn archive_with_assets(context: &Context) -> Result<()> {
    let path = context.output.join(format!("{}.tar.zst", context.stem()));
    let encoder = zstd::Encoder::new(fs::File::create(&path)?, 15)?;
    let mut tar = tar::Builder::new(encoder);
    let binary = context.binary("daat-locus");
    tar.append_path_with_name(
        &binary,
        PathBuf::from(context.stem()).join(binary.file_name().unwrap()),
    )?;
    tar.into_inner()?.finish()?;
    let reader = zstd::Decoder::new(fs::File::open(&path)?)?;
    if tar::Archive::new(reader).entries()?.count() != 1 {
        return Err("Archive verification failed".into());
    }
    packaged("binary archive", &path)?;
    Ok(())
}
fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn raster(source: &Path, size: u32, transparent_mark: bool) -> Result<Vec<u8>> {
    let tree = resvg::usvg::Tree::from_data(&fs::read(source)?, &resvg::usvg::Options::default())?;
    let mut pixels = resvg::tiny_skia::Pixmap::new(size, size).ok_or("Invalid icon dimensions")?;
    let scale = size as f32 / tree.size().width();
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixels.as_mut(),
    );
    let mut rgba = pixels.data().to_vec();
    for pixel in rgba.as_chunks_mut::<4>().0 {
        if transparent_mark {
            // Reuse Android's black mark without its white launcher backdrop.
            pixel[3] = pixel[3].saturating_sub(pixel[0]);
            pixel[..3].fill(0);
            continue;
        }
        let alpha = pixel[3] as u32;
        if alpha > 0 && alpha < 255 {
            for color in &mut pixel[..3] {
                *color = ((*color as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    Ok(rgba)
}
fn png_file(source: &Path, path: &Path, size: u32, transparent_mark: bool) -> Result<()> {
    let mut encoder = png::Encoder::new(fs::File::create(path)?, size, size);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()?
        .write_image_data(&raster(source, size, transparent_mark)?)?;
    Ok(())
}
fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_symlink() {
            return Err("Bundle source cannot contain symlinks".into());
        }
        if ty.is_dir() {
            copy_tree(&entry.path(), &target.join(entry.file_name()))?;
        } else {
            fs::copy(entry.path(), target.join(entry.file_name()))?;
        }
    }
    Ok(())
}
fn sign_macos_app(app: &Path, identity: Option<&str>) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let signing = macos_signing::SigningIdentity::resolve(identity)?;
        signing.sign_app(app)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, identity);
        Err("macOS signing requires macOS".into())
    }
}
fn macos(context: &Context) -> Result<()> {
    let work = context.work()?;
    let app = work.join("Daat Locus.app");
    let bin = app.join("Contents/MacOS");
    let resources = app.join("Contents/Resources");
    fs::create_dir_all(&bin)?;
    fs::create_dir_all(&resources)?;
    for name in ["daat-locus", "daat-locus-app"] {
        fs::copy(context.binary(name), bin.join(name))?;
    }
    let iconset = work.join("AppIcon.iconset");
    fs::create_dir_all(&iconset)?;
    for (size, name) in [
        (16, "icon_16x16.png"),
        (32, "icon_16x16@2x.png"),
        (32, "icon_32x32.png"),
        (64, "icon_32x32@2x.png"),
        (128, "icon_128x128.png"),
        (256, "icon_128x128@2x.png"),
        (256, "icon_256x256.png"),
        (512, "icon_256x256@2x.png"),
        (512, "icon_512x512.png"),
        (1024, "icon_512x512@2x.png"),
    ] {
        png_file(
            &context.root.join("assets/logo-mark.svg"),
            &iconset.join(name),
            size,
            true,
        )?;
    }
    command(
        Command::new("iconutil")
            .args(["-c", "icns"])
            .arg(iconset)
            .arg("-o")
            .arg(resources.join("AppIcon.icns")),
    )?;
    fs::write(
        app.join("Contents/Info.plist"),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleName</key><string>Daat Locus</string><key>CFBundleDisplayName</key><string>Daat Locus</string><key>CFBundleExecutable</key><string>daat-locus-app</string><key>CFBundleIdentifier</key><string>io.shadow3aaa.daatlocus</string><key>CFBundleIconFile</key><string>AppIcon</string><key>CFBundleShortVersionString</key><string>{}</string><key>CFBundleVersion</key><string>{}</string><key>CFBundlePackageType</key><string>APPL</string><key>LSMinimumSystemVersion</key><string>11.0</string></dict></plist>"#,
            xml(context.meta.installer_version()),
            xml(context.meta.installer_version())
        ),
    )?;
    sign_macos_app(&app, context.macos_signing_identity.as_deref())?;
    let stage = work.join("installer-root");
    copy_tree(&app, &stage.join("Applications/Daat Locus.app"))?;
    let cli = stage.join("usr/local/bin");
    fs::create_dir_all(&cli)?;
    fs::write(
        cli.join("daat-locus"),
        "#!/bin/sh\nexec '/Applications/Daat Locus.app/Contents/MacOS/daat-locus' \"$@\"\n",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(cli.join("daat-locus"), fs::Permissions::from_mode(0o755))?;
    }
    let pkg = context.output.join(format!("{}.pkg", context.stem()));
    command(
        Command::new("pkgbuild")
            .arg("--root")
            .arg(stage)
            .args([
                "--identifier",
                "io.shadow3aaa.daatlocus.pkg",
                "--version",
                context.meta.installer_version(),
                "--install-location",
                "/",
            ])
            .arg(&pkg),
    )?;
    copy_tree(&app, &context.output.join("Daat Locus.app"))?;
    packaged("macOS app", &context.output.join("Daat Locus.app"))?;
    packaged("macOS installer", &pkg)?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_app_signature_covers_bundle_resources_and_nested_code() {
        let directory = tempfile::tempdir().unwrap();
        let app = directory.path().join("Fixture.app");
        let bin = app.join("Contents/MacOS");
        let resources = app.join("Contents/Resources");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&resources).unwrap();
        let source = directory.path().join("main.c");
        fs::write(&source, "int main(void) { return 0; }").unwrap();
        command(
            Command::new("xcrun")
                .arg("clang")
                .arg(&source)
                .arg("-o")
                .arg(bin.join("daat-locus-app")),
        )
        .unwrap();
        fs::copy(bin.join("daat-locus-app"), bin.join("daat-locus")).unwrap();
        fs::write(
            app.join("Contents/Info.plist"),
            r#"<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>io.daat-locus.packaging-fixture</string><key>CFBundleExecutable</key><string>daat-locus-app</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>"#,
        )
        .unwrap();
        let resource = resources.join("fixture.txt");
        fs::write(&resource, "original resource").unwrap();
        sign_macos_app(&app, Some("-")).unwrap();
        // Copying into the installer must retain a valid, complete bundle seal.
        let staged = directory.path().join("installed/Fixture.app");
        copy_tree(&app, &staged).unwrap();
        let verify = || {
            Command::new("codesign")
                .args(["--verify", "--deep", "--strict"])
                .arg(&staged)
                .output()
                .unwrap()
                .status
                .success()
        };
        assert!(verify());
        let staged_resource = staged.join("Contents/Resources/fixture.txt");
        fs::write(&staged_resource, "changed resource").unwrap();
        assert!(
            !verify(),
            "changed bundle resources must invalidate the seal"
        );
        fs::write(&staged_resource, "original resource").unwrap();
        fs::write(staged.join("Contents/MacOS/daat-locus"), "invalid code").unwrap();
        assert!(!verify(), "changed companion code must invalidate the seal");
        assert!(
            sign_macos_app(&app, Some("Daat Locus Missing Fixture Identity")).is_err(),
            "an explicit signing identity must never silently fall back to ad-hoc"
        );
    }
    #[test]
    fn embedded_marker_can_span_short_reads() {
        struct Chunks(std::io::Cursor<Vec<u8>>);
        impl std::io::Read for Chunks {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                let count = buffer.len().min(7);
                std::io::Read::read(&mut self.0, &mut buffer[..count])
            }
        }
        let mut data = vec![0; 19];
        data.extend_from_slice(UI_MARKER);
        assert!(contains_ui_marker(Chunks(std::io::Cursor::new(data))).unwrap());
        assert!(!contains_ui_marker(&b"no embedded assets"[..]).unwrap());
    }
    #[test]
    fn target_is_one_component_and_xml_fields_are_escaped() {
        assert!(validate_target("x86_64-pc-windows-msvc").is_ok());
        for value in ["../escape", "/tmp", "x/../y", ""] {
            assert!(validate_target(value).is_err());
        }
        assert_eq!(xml("a&\"b<"), "a&amp;&quot;b&lt;");
    }
    #[test]
    fn archive_has_expected_binary_and_no_extra_tree() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_path_buf();
        let c = Context {
            root: root.clone(),
            meta: Metadata {
                name: "daat-locus".into(),
                version: "0.5.0".into(),
                authors: vec![],
            },
            target: "x86_64-pc-windows-msvc".into(),
            release: root.clone(),
            output: root,
            macos_signing_identity: None,
        };
        fs::write(c.binary("daat-locus"), UI_MARKER).unwrap();
        fs::write(c.binary("daat-locus-app"), b"desktop-client").unwrap();
        require_embedded(&c.binary("daat-locus")).unwrap();
        archive_with_assets(&c).unwrap();
        let reader = zstd::Decoder::new(
            fs::File::open(c.output.join(format!("{}.tar.zst", c.stem()))).unwrap(),
        )
        .unwrap();
        let mut archive = tar::Archive::new(reader);
        let paths: Vec<_> = archive
            .entries()
            .unwrap()
            .map(|entry| entry.unwrap().path().unwrap().into_owned())
            .collect();
        assert_eq!(
            paths,
            vec![PathBuf::from(
                "daat-locus-0.5.0-x86_64-pc-windows-msvc/daat-locus.exe"
            ),]
        );
    }
}
