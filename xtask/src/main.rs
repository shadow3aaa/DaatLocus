//! Distribution builds ship the desktop client and independent embedded-WebUI service.
mod android;
mod reporting;

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
struct Context {
    root: PathBuf,
    meta: Metadata,
    target: String,
    release: PathBuf,
    output: PathBuf,
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
    let Cli {
        command: Task::Package { format },
    } = Cli::parse();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
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
    status(
        "Packaging",
        &format!(
            "{} v{} ({kind}, {target})",
            meta.package.name, meta.package.version
        ),
    );
    if !args.skip_build {
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
        if desktop_target(&target) {
            build.args(["-p", "daat-locus-app"]);
        }
        command(&mut build)?;
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
    };
    let binary = context.binary("daat-locus");
    require_embedded(&binary)?;
    if desktop_target(&context.target) && !context.binary("daat-locus-app").is_file() {
        return Err("Desktop client is missing; build daat-locus-app".into());
    }
    match kind {
        "binary" => archive(&context)?,
        "windows" => windows(&context)?,
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
fn desktop_target(target: &str) -> bool {
    target.contains("windows-msvc") || target.contains("apple-darwin")
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
fn contains_ui_marker(mut reader: impl std::io::Read) -> std::io::Result<bool> {
    // Android debug libraries can exceed a gigabyte. Scan without buffering the
    // whole library and retain the overlap when the marker spans two reads.
    let mut buffer = vec![0; 64 * 1024 + UI_MARKER.len()];
    let mut retained = 0;
    loop {
        let read = reader.read(&mut buffer[retained..])?;
        if read == 0 {
            return Ok(false);
        }
        let length = retained + read;
        if buffer[..length]
            .windows(UI_MARKER.len())
            .any(|part| part == UI_MARKER)
        {
            return Ok(true);
        }
        retained = length.min(UI_MARKER.len() - 1);
        buffer.copy_within(length - retained..length, 0);
    }
}
fn archive(context: &Context) -> Result<()> {
    let path = context.output.join(format!("{}.tar.zst", context.stem()));
    let encoder = zstd::Encoder::new(fs::File::create(&path)?, 15)?;
    let mut tar = tar::Builder::new(encoder);
    let binary = context.binary("daat-locus");
    tar.append_path_with_name(
        &binary,
        PathBuf::from(context.stem()).join(binary.file_name().unwrap()),
    )?;
    if desktop_target(&context.target) {
        let client = context.binary("daat-locus-app");
        tar.append_path_with_name(
            &client,
            PathBuf::from(context.stem()).join(client.file_name().unwrap()),
        )?;
    }
    tar.into_inner()?.finish()?;
    let reader = zstd::Decoder::new(fs::File::open(&path)?)?;
    if tar::Archive::new(reader).entries()?.count()
        != if desktop_target(&context.target) {
            2
        } else {
            1
        }
    {
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
fn raster(root: &Path, size: u32) -> Result<Vec<u8>> {
    let tree = resvg::usvg::Tree::from_data(
        &fs::read(root.join("assets/logo.svg"))?,
        &resvg::usvg::Options::default(),
    )?;
    let mut pixels = resvg::tiny_skia::Pixmap::new(size, size).ok_or("Invalid icon dimensions")?;
    let scale = size as f32 / tree.size().width();
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixels.as_mut(),
    );
    let mut rgba = pixels.data().to_vec();
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let alpha = pixel[3] as u32;
        if alpha > 0 && alpha < 255 {
            for color in &mut pixel[..3] {
                *color = ((*color as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    Ok(rgba)
}
fn png_file(root: &Path, path: &Path, size: u32) -> Result<()> {
    let mut encoder = png::Encoder::new(fs::File::create(path)?, size, size);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()?
        .write_image_data(&raster(root, size)?)?;
    Ok(())
}
fn windows(context: &Context) -> Result<()> {
    let work = context.work()?;
    let icon = work.join("app.ico");
    let mut directory = ico::IconDir::new(ico::ResourceType::Icon);
    for size in [16, 24, 32, 48, 64, 128, 256] {
        directory.add_entry(ico::IconDirEntry::encode(&ico::IconImage::from_rgba_data(
            size,
            size,
            raster(&context.root, size)?,
        ))?);
    }
    directory.write(fs::File::create(&icon)?)?;
    let client = context.binary("daat-locus-app");
    if !client.is_file() {
        return Err("Desktop client is missing".into());
    }
    let source = work.join("package.wxs");
    let msi = context.output.join(format!("{}.msi", context.stem()));
    fs::write(
        &source,
        format!(
            r#"<Wix xmlns="http://wixtoolset.org/schemas/v4/wxs" xmlns:util="http://wixtoolset.org/schemas/v4/wxs/util">
<Package Name="Daat Locus" Manufacturer="{}" Version="{}" UpgradeCode="B84D8EAD-E478-4567-9883-68D7395CBAE6" Scope="perUser">
<MajorUpgrade AllowSameVersionUpgrades="yes" DowngradeErrorMessage="A newer version is installed."/><MediaTemplate EmbedCab="yes"/>
<Icon Id="AppIcon" SourceFile="{}"/><Property Id="ARPPRODUCTICON" Value="AppIcon"/>
<StandardDirectory Id="LocalAppDataFolder"><Directory Id="INSTALLFOLDER" Name="Daat Locus">
<Component Id="Main"><File Id="MainExe" Source="{}" KeyPath="yes"/></Component>
<Component Id="Launcher"><File Id="LauncherExe" Source="{}" KeyPath="yes"/></Component>
<Component Id="Path"><Environment Id="UserPath" Name="PATH" Value="[INSTALLFOLDER]" Permanent="no" Part="last" Action="set" System="no"/><RegistryValue Root="HKCU" Key="Software\DaatLocus" Name="Path" Value="1" Type="integer" KeyPath="yes"/></Component>
</Directory></StandardDirectory>
<StandardDirectory Id="ProgramMenuFolder"><Directory Id="MenuFolder" Name="Daat Locus"><Component Id="Shortcut">
<Shortcut Id="Launch" Name="Daat Locus" Target="[#LauncherExe]" WorkingDirectory="INSTALLFOLDER" Icon="AppIcon"/>
<RemoveFolder Id="RemoveMenu" On="uninstall"/><RegistryValue Root="HKCU" Key="Software\DaatLocus" Name="Menu" Value="1" Type="integer" KeyPath="yes"/>
</Component></Directory></StandardDirectory>
<Feature Id="Product"><ComponentRef Id="Main"/><ComponentRef Id="Launcher"/><ComponentRef Id="Path"/><ComponentRef Id="Shortcut"/></Feature>
</Package></Wix>"#,
            xml(context
                .meta
                .authors
                .first()
                .map_or("Daat Locus", String::as_str)),
            xml(&context.meta.version),
            xml(&icon.to_string_lossy()),
            xml(&context.binary("daat-locus").to_string_lossy()),
            xml(&client.to_string_lossy())
        ),
    )?;
    command(
        Command::new("wix")
            .args([
                "build",
                "-ext",
                "WixToolset.Util.wixext",
                "-arch",
                if context.target.starts_with("aarch64") {
                    "arm64"
                } else {
                    "x64"
                },
            ])
            .arg(&source)
            .arg("-o")
            .arg(&msi),
    )?;
    let license = work.join("license.rtf");
    let body = fs::read_to_string(context.root.join("LICENSE"))?
        .replace('\\', "\\\\")
        .replace('{', "\\{")
        .replace('}', "\\}")
        .replace('\n', "\\par\n");
    fs::write(&license, format!("{{\\rtf1\\ansi {body}}}"))?;
    let logo = work.join("logo.png");
    png_file(&context.root, &logo, 128)?;
    let webview = work.join("MicrosoftEdgeWebview2Setup.exe");
    let response = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()?
        .get("https://go.microsoft.com/fwlink/p/?LinkId=2124703")
        .send()?
        .error_for_status()?;
    use std::io::Read;
    let mut bytes = Vec::new();
    response.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 4 * 1024 * 1024 || !bytes.starts_with(b"MZ") {
        return Err("Invalid WebView2 bootstrapper".into());
    }
    fs::write(&webview, bytes)?;
    // Validate the official bootstrapper before embedding any executable installer.
    command(Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference='Stop'; $signature=Get-AuthenticodeSignature -LiteralPath $env:DAAT_LOCUS_WEBVIEW_BOOTSTRAPPER; if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'CN=Microsoft Corporation') { exit 1 }"])
        .env_remove("PSModulePath")
        .env("DAAT_LOCUS_WEBVIEW_BOOTSTRAPPER", &webview))?;
    let bundle = work.join("bundle.wxs");
    let setup = context.output.join(format!("{}-setup.exe", context.stem()));
    fs::write(
        &bundle,
        format!(
            r#"<Wix xmlns="http://wixtoolset.org/schemas/v4/wxs" xmlns:bal="http://wixtoolset.org/schemas/v4/wxs/bal" xmlns:util="http://wixtoolset.org/schemas/v4/wxs/util">
<Bundle Name="Daat Locus" Manufacturer="Daat Locus" Version="{}" UpgradeCode="38B2E30A-C2C8-4DEE-BD10-1520DF7A9E3F" IconSourceFile="{}">
<BootstrapperApplication><bal:WixStandardBootstrapperApplication Theme="rtfLargeLicense" LicenseFile="{}" LogoFile="{}" SuppressOptionsUI="yes"/></BootstrapperApplication>
<Variable Name="WebView2Machine32" Type="version" Value="0.0.0.0"/>
<Variable Name="WebView2Machine64" Type="version" Value="0.0.0.0"/>
<Variable Name="WebView2User32" Type="version" Value="0.0.0.0"/>
<Variable Name="WebView2User64" Type="version" Value="0.0.0.0"/>
<util:RegistrySearch Id="WebView2Machine32Search" Variable="WebView2Machine32" Root="HKLM" Key="SOFTWARE\Microsoft\EdgeUpdate\Clients\{{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}}" Value="pv" Result="value" Bitness="always32"/>
<util:RegistrySearch Id="WebView2Machine64Search" Variable="WebView2Machine64" Root="HKLM" Key="SOFTWARE\Microsoft\EdgeUpdate\Clients\{{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}}" Value="pv" Result="value" Bitness="always64"/>
<util:RegistrySearch Id="WebView2User32Search" Variable="WebView2User32" Root="HKCU" Key="SOFTWARE\Microsoft\EdgeUpdate\Clients\{{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}}" Value="pv" Result="value" Bitness="always32"/>
<util:RegistrySearch Id="WebView2User64Search" Variable="WebView2User64" Root="HKCU" Key="SOFTWARE\Microsoft\EdgeUpdate\Clients\{{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}}" Value="pv" Result="value" Bitness="always64"/>
<Chain>
<ExePackage Id="WebView2" SourceFile="{}" InstallArguments="/silent /install" DetectCondition="WebView2Machine32 &gt; v0.0.0.0 OR WebView2Machine64 &gt; v0.0.0.0 OR WebView2User32 &gt; v0.0.0.0 OR WebView2User64 &gt; v0.0.0.0" Permanent="yes" PerMachine="no" Vital="yes">
<ExitCode Value="0" Behavior="success"/>
<!-- 0x80040828: a concurrent installer has already installed the runtime. -->
<ExitCode Value="-2147219416" Behavior="success"/>
<ExitCode Value="3010" Behavior="scheduleReboot"/>
<ExitCode Value="1641" Behavior="forceReboot"/>
<ExitCode Behavior="error"/>
</ExePackage>
<MsiPackage SourceFile="{}" Compressed="yes"/>
</Chain>
</Bundle>
</Wix>"#,
            xml(&context.meta.version),
            xml(&icon.to_string_lossy()),
            xml(&license.to_string_lossy()),
            xml(&logo.to_string_lossy()),
            xml(&webview.to_string_lossy()),
            xml(&msi.to_string_lossy())
        ),
    )?;
    command(
        Command::new("wix")
            .args([
                "build",
                "-ext",
                "WixToolset.BootstrapperApplications.wixext",
                "-ext",
                "WixToolset.Util.wixext",
            ])
            .arg(bundle)
            .arg("-o")
            .arg(&setup),
    )?;
    packaged("Windows MSI", &msi)?;
    packaged("Windows setup", &setup)?;
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
        png_file(&context.root, &iconset.join(name), size)?;
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
            r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleName</key><string>Daat Locus</string><key>CFBundleDisplayName</key><string>Daat Locus</string><key>CFBundleExecutable</key><string>daat-locus-app</string><key>CFBundleIdentifier</key><string>io.daat-locus.app</string><key>CFBundleIconFile</key><string>AppIcon</string><key>CFBundleShortVersionString</key><string>{}</string><key>CFBundleVersion</key><string>{}</string><key>CFBundlePackageType</key><string>APPL</string><key>LSMinimumSystemVersion</key><string>11.0</string></dict></plist>"#,
            xml(&context.meta.version),
            xml(&context.meta.version)
        ),
    )?;
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
                "io.daat-locus.pkg",
                "--version",
                &context.meta.version,
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
        };
        fs::write(c.binary("daat-locus"), UI_MARKER).unwrap();
        fs::write(c.binary("daat-locus-app"), b"desktop-client").unwrap();
        require_embedded(&c.binary("daat-locus")).unwrap();
        archive(&c).unwrap();
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
            vec![
                PathBuf::from("daat-locus-0.5.0-x86_64-pc-windows-msvc/daat-locus.exe"),
                PathBuf::from("daat-locus-0.5.0-x86_64-pc-windows-msvc/daat-locus-app.exe")
            ]
        );
    }
}
