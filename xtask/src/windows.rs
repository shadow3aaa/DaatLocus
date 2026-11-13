use crate::{Context, Result, command, packaged, png_file, raster, xml};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::Path, process::Command};

// Keep upgrade coordination independent of optional native capabilities.
// Burn runs the inverse steps on rollback; uninstall must not replay them.
fn service_lifecycle(companion: &Path, installation: &str) -> Result<(String, String)> {
    let transaction = uuid::Uuid::new_v4();
    let digest: String = Sha256::digest(fs::read(companion)?)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let arguments = |action| {
        xml(&format!(
            "installer {action} --installation \"{installation}\" --transaction {transaction}"
        ))
    };
    let package = |id, install, rollback| {
        format!(
            r#"<ExePackage Id="{id}" DisplayName="Daat Locus" Name="{id}.exe" CacheId="DaatLocus-{id}-{digest}" SourceFile="{}" PerMachine="no" Permanent="no" Vital="yes" DetectCondition="0" InstallCondition="WixBundleAction &lt;&gt; 3" InstallArguments="{}" RepairArguments="{}" UninstallArguments="{}"><ExitCode Value="0" Behavior="success"/><ExitCode Behavior="error"/></ExePackage>"#,
            xml(&companion.to_string_lossy()),
            arguments(install),
            arguments(install),
            arguments(rollback)
        )
    };
    Ok((
        package("ServicePause", "prepare", "resume"),
        package("ServiceResume", "resume", "prepare"),
    ))
}

fn download_installer(
    url: &str,
    path: &Path,
    publisher: &str,
    checksum: Option<&str>,
) -> Result<()> {
    let response = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?
        .get(url)
        .send()?
        .error_for_status()?;
    let mut bytes = Vec::new();
    response
        .take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 32 * 1024 * 1024
        || checksum.is_some_and(|expected| {
            let actual: String = Sha256::digest(&bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            !actual.eq_ignore_ascii_case(expected)
        })
    {
        return Err(format!("Invalid Windows dependency: {}", path.display()).into());
    }
    fs::write(path, bytes)?;
    command(Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference='Stop'; $signature=Get-AuthenticodeSignature -LiteralPath $env:DAAT_LOCUS_INSTALLER; if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.GetNameInfo([System.Security.Cryptography.X509Certificates.X509NameType]::SimpleName, $false) -ne $env:DAAT_LOCUS_PUBLISHER) { exit 1 }"])
        .env_remove("PSModulePath")
        .env("DAAT_LOCUS_INSTALLER", path)
        .env("DAAT_LOCUS_PUBLISHER", publisher))
}

pub(crate) fn package(context: &Context) -> Result<()> {
    let work = context.work()?;
    let icon = work.join("app.ico");
    let mut directory = ico::IconDir::new(ico::ResourceType::Icon);
    for size in [16, 24, 32, 48, 64, 128, 256] {
        directory.add_entry(ico::IconDirEntry::encode(&ico::IconImage::from_rgba_data(
            size,
            size,
            raster(&context.root.join("assets/logo-mark.svg"), size, false)?,
        ))?);
    }
    directory.write(fs::File::create(&icon)?)?;
    let client = context.binary("daat-locus-app");
    if !client.is_file() {
        return Err("Desktop client is missing".into());
    }
    let source = work.join("package.wxs");
    let msi = work.join(format!("{}.msi", context.stem()));
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
            xml(context.meta.installer_version()),
            xml(&icon.to_string_lossy()),
            xml(&context.binary("daat-locus").to_string_lossy()),
            xml(&client.to_string_lossy()),
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
    let winfsp_license = fs::read_to_string(
        context
            .root
            .join("crates/daat-locus-workspace/third-party/WinFsp-LICENSE"),
    )?
    .replace('\\', "\\\\")
    .replace('{', "\\{")
    .replace('}', "\\}")
    .replace('\n', "\\par\n");
    fs::write(
        &license,
        format!(
            "{{\\rtf1\\ansi\\deff0 {{\\fonttbl{{\\f0 Segoe UI;}}}}\\f0\\fs20 {body}\\page WinFsp - Windows File System Proxy\\par Copyright (C) Bill Zissimopoulos\\par https://github.com/winfsp/winfsp\\par\\par {winfsp_license}}}"
        ),
    )?;
    let logo = work.join("logo.png");
    png_file(
        &context.root.join("assets/logo-mark.svg"),
        &logo,
        128,
        false,
    )?;
    let webview = work.join("MicrosoftEdgeWebview2Setup.exe");
    download_installer(
        "https://go.microsoft.com/fwlink/p/?LinkId=2124703",
        &webview,
        "Microsoft Corporation",
        None,
    )?;
    let winfsp = work.join("winfsp-2.2.26286.msi");
    download_installer(
        "https://github.com/winfsp/winfsp/releases/download/v2.2B5/winfsp-2.2.26286.msi",
        &winfsp,
        "NAVIMATICS LLC",
        Some("293ffb9bcc0b625c85079fe85fa77f8fe98170aba51a02ac9c83d58f6c3b0e2f"),
    )?;
    let ui = context.root.join("xtask/windows");
    let workspace = work.join("workspace-support.msi");
    command(
        Command::new("wix")
            .args([
                "build",
                "-arch",
                if context.target.starts_with("aarch64-") {
                    "arm64"
                } else {
                    "x64"
                },
                "-d",
            ])
            .arg(format!("Version={}", context.meta.installer_version()))
            .arg(ui.join("workspace.wxs"))
            .arg("-o")
            .arg(&workspace),
    )?;
    let bootstrapper = work.join("bootstrapper.dll");
    command(
        Command::new("rustc")
            .arg(ui.join("bootstrapper.rs"))
            .args([
                "--edition=2024",
                "--crate-type=cdylib",
                "--target",
                &context.target,
                "-C",
                "opt-level=s",
                "-C",
                "panic=abort",
                "-o",
            ])
            .arg(&bootstrapper),
    )?;
    let theme = fs::read_to_string(ui.join("theme.xml"))?
        .replace("logo.png", "..\\logo.png")
        .replace("app.ico", "..\\app.ico");
    let mut localizations = String::new();
    for (culture, language, font) in [
        ("en-US", "1033", "Segoe UI"),
        ("zh-CN", "2052", "Microsoft YaHei UI"),
    ] {
        let localized_theme = work.join(format!("theme-{culture}.xml"));
        fs::write(&localized_theme, theme.replace("Segoe UI", font))?;
        for (index, directory) in [culture, language].into_iter().enumerate() {
            localizations.push_str(&format!(
                r#"<Payload Id="SetupLocale{language}_{index}" SourceFile="{}" Name="{directory}\thm.wxl"/><Payload Id="SetupTheme{language}_{index}" SourceFile="{}" Name="{directory}\thm.xml"/>"#,
                xml(&ui.join(format!("{culture}.wxl")).to_string_lossy()),
                xml(&localized_theme.to_string_lossy()),
            ));
        }
    }
    let bundle = work.join("bundle.wxs");
    let (pause_service, resume_service) = service_lifecycle(
        &context.binary("daat-locus"),
        "[LocalAppDataFolder]Daat Locus",
    )?;
    let setup = context.output.join(format!("{}-setup.exe", context.stem()));
    fs::write(
        &bundle,
        format!(
            r#"<Wix xmlns="http://wixtoolset.org/schemas/v4/wxs" xmlns:bal="http://wixtoolset.org/schemas/v4/wxs/bal" xmlns:util="http://wixtoolset.org/schemas/v4/wxs/util">
<Bundle Name="Daat Locus" Manufacturer="Daat Locus" Version="{}" UpgradeCode="38B2E30A-C2C8-4DEE-BD10-1520DF7A9E3F" IconSourceFile="{}">
<BootstrapperApplication>
<bal:WixStandardBootstrapperApplication Theme="rtfLicense" ThemeFile="{}" LocalizationFile="{}" LicenseFile="{}" LogoFile="{}" LaunchTarget="[LocalAppDataFolder]Daat Locus\daat-locus-app.exe" SuppressOptionsUI="yes"/>
{localizations}
<Payload SourceFile="{}" bal:BAFunctions="yes"/>
<Payload SourceFile="{}" Name="app.ico"/>
</BootstrapperApplication>
<Variable Name="LicenseAccepted" Type="numeric" Value="0"/>
<Variable Name="WinFspVersion" Type="version" Value="0.0.0.0"/>
<util:ProductSearch Id="WinFspSearch" Variable="WinFspVersion" UpgradeCode="5466A3D8-3AA1-4240-B6A0-3A051940A3EC" Result="version"/>
<Variable Name="WorkspaceUnc64" Type="numeric" Value="0"/>
<Variable Name="WorkspaceUnc32" Type="numeric" Value="0"/>
<util:RegistrySearch Id="WorkspaceUnc64Search" Variable="WorkspaceUnc64" Root="HKLM" Key="SOFTWARE\Microsoft\Command Processor" Value="DisableUNCCheck" Result="value" Bitness="always64"/>
<util:RegistrySearch Id="WorkspaceUnc32Search" Variable="WorkspaceUnc32" Root="HKLM" Key="SOFTWARE\Microsoft\Command Processor" Value="DisableUNCCheck" Result="value" Bitness="always32"/>
<Variable Name="WebView2Machine32" Type="version" Value="0.0.0.0"/>
<Variable Name="WebView2Machine64" Type="version" Value="0.0.0.0"/>
<Variable Name="WebView2User32" Type="version" Value="0.0.0.0"/>
<Variable Name="WebView2User64" Type="version" Value="0.0.0.0"/>
<util:RegistrySearch Id="WebView2Machine32Search" Variable="WebView2Machine32" Root="HKLM" Key="SOFTWARE\Microsoft\EdgeUpdate\Clients\{{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}}" Value="pv" Result="value" Bitness="always32"/>
<util:RegistrySearch Id="WebView2Machine64Search" Variable="WebView2Machine64" Root="HKLM" Key="SOFTWARE\Microsoft\EdgeUpdate\Clients\{{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}}" Value="pv" Result="value" Bitness="always64"/>
<util:RegistrySearch Id="WebView2User32Search" Variable="WebView2User32" Root="HKCU" Key="SOFTWARE\Microsoft\EdgeUpdate\Clients\{{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}}" Value="pv" Result="value" Bitness="always32"/>
<util:RegistrySearch Id="WebView2User64Search" Variable="WebView2User64" Root="HKCU" Key="SOFTWARE\Microsoft\EdgeUpdate\Clients\{{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}}" Value="pv" Result="value" Bitness="always64"/>
<Chain>
{pause_service}
<ExePackage Id="WebView2" DisplayName="Microsoft Edge WebView2" SourceFile="{}" InstallArguments="/silent /install" DetectCondition="WebView2Machine32 &gt; v0.0.0.0 OR WebView2Machine64 &gt; v0.0.0.0 OR WebView2User32 &gt; v0.0.0.0 OR WebView2User64 &gt; v0.0.0.0" Permanent="yes" PerMachine="no" Vital="yes">
<ExitCode Value="0" Behavior="success"/>
<!-- 0x80040828: a concurrent installer has already installed the runtime. -->
<ExitCode Value="-2147219416" Behavior="success"/>
<ExitCode Value="3010" Behavior="scheduleReboot"/>
<ExitCode Value="1641" Behavior="forceReboot"/>
<ExitCode Behavior="error"/>
</ExePackage>
<MsiPackage Id="WinFsp" DisplayName="WinFsp 2.2 Beta 5" SourceFile="{}" Permanent="yes" InstallCondition="WinFspVersion &lt; v2.2.26286 OR (WinFspVersion = v2.2.26286 AND WixBundleAction = 8)"/>
<MsiPackage Id="WorkspaceSupport" DisplayName="Workspace Support" SourceFile="{}" Permanent="yes" InstallCondition="WorkspaceUnc64 &lt;&gt; 1 OR WorkspaceUnc32 &lt;&gt; 1 OR WixBundleAction = 8"/>
<MsiPackage Id="DaatLocus" DisplayName="Daat Locus" SourceFile="{}" Compressed="yes"/>
{resume_service}
</Chain>
</Bundle>
</Wix>"#,
            xml(context.meta.installer_version()),
            xml(&icon.to_string_lossy()),
            xml(&ui.join("theme.xml").to_string_lossy()),
            xml(&ui.join("en-US.wxl").to_string_lossy()),
            xml(&license.to_string_lossy()),
            xml(&logo.to_string_lossy()),
            xml(&bootstrapper.to_string_lossy()),
            xml(&icon.to_string_lossy()),
            xml(&webview.to_string_lossy()),
            xml(&winfsp.to_string_lossy()),
            xml(&workspace.to_string_lossy()),
            xml(&msi.to_string_lossy()),
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
                "-arch",
                if context.target.starts_with("aarch64-") {
                    "arm64"
                } else {
                    "x64"
                },
            ])
            .arg(bundle)
            .arg("-o")
            .arg(&setup),
    )?;
    packaged("Windows setup", &setup)?;
    Ok(())
}

#[cfg(all(test, windows))]
#[path = "windows_installer_tests.rs"]
mod tests;
