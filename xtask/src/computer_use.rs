//! Windows distribution assets; explicit trust provisioning belongs to the elevated helper.
use crate::{Context, Result, command, xml};
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf, process::Command};

pub(crate) struct Assets {
    pub hook: PathBuf,
    pub setup: PathBuf,
    pub driver: PathBuf,
    pub package_hash: String,
}

pub(crate) const CERTIFICATE_THUMBPRINT: &str = "C2D252AEFAF27171C36AF1BD6DAF2FDC5441E462";
pub(crate) const TIMESTAMP_SERVER: &str = "http://timestamp.digicert.com";
const TRUST_KEY: &str = "SOFTWARE\\DaatLocus\\VirtualDisplay\\E40E701A47B15F108A4D9D4D7152A153EB84A3FED1F7FF287581B98D63062B6B";

pub(crate) fn prepare(
    context: &Context,
    driver: Option<PathBuf>,
    certificate: Option<String>,
    timestamp_server: &str,
    skip: bool,
) -> Result<Assets> {
    if context.target != "x86_64-pc-windows-msvc" {
        return Err("ComputerUse distribution currently requires Windows x64".into());
    }
    if certificate
        .as_ref()
        .is_some_and(|value| !value.eq_ignore_ascii_case(CERTIFICATE_THUMBPRINT))
    {
        return Err("Windows bundled driver signing must use the pinned Daat Locus Virtual Display identity".into());
    }
    if !skip {
        command(Command::new("cargo").current_dir(&context.root).args([
            "build",
            "--locked",
            "--release",
            "--target",
            &context.target,
            "-p",
            "daat-locus-workspace-hook",
        ]))?;
        command(Command::new("cargo").current_dir(&context.root).args([
            "build",
            "--locked",
            "--release",
            "--target",
            &context.target,
            "-p",
            "daat-locus-virtual-workspace",
            "--features",
            "driver-setup",
            "--bin",
            "daat-locus-display-setup",
        ]))?;
    }
    let driver = match driver {
        Some(path) => crate::absolute(&context.root, path),
        None if !skip => {
            crate::idd_driver::distribution(&context.root, certificate.clone(), timestamp_server)?
        }
        None => context.root.join("target/virtual-workspace-driver"),
    };
    let package_hash = Sha256::digest(fs::read(driver.join("DaatLocusVirtualDisplay.cat"))?)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let assets = Assets {
        hook: context.release.join("daat_locus_workspace_hook.dll"),
        setup: context.binary("daat-locus-display-setup"),
        driver,
        package_hash,
    };
    let signtool = sdk_tool("signtool.exe")?;
    if let Some(certificate) = certificate {
        for path in [&assets.hook, &assets.setup] {
            sign_file(&signtool, path, &certificate, timestamp_server)?;
        }
    }
    for path in [&assets.hook, &assets.setup] {
        command(
            Command::new(&signtool)
                .args(["verify", "/pa", "/v"])
                .arg(path),
        )?;
    }
    let cat = assets.driver.join("DaatLocusVirtualDisplay.cat");
    command(
        Command::new(&signtool)
            .args(["verify", "/pa", "/v", "/c"])
            .arg(&cat)
            .arg(assets.driver.join("DaatLocusVirtualDisplay.dll"))
            .arg(assets.driver.join("DaatLocusVirtualDisplay.inf")),
    )?;
    command(
        Command::new(&assets.setup)
            .args(["verify", "--package"])
            .arg(&assets.driver),
    )?;
    if !assets.driver.join("LICENSE.microsoft").is_file() {
        return Err("IDD redistribution license missing".into());
    }
    Ok(assets)
}

pub(crate) fn sign_file(
    signtool: &std::path::Path,
    path: &std::path::Path,
    certificate: &str,
    timestamp_server: &str,
) -> Result<()> {
    let servers = std::iter::once(timestamp_server)
        .chain((timestamp_server == TIMESTAMP_SERVER).then_some("http://timestamp.sectigo.com"));
    for server in servers {
        let result = command(
            Command::new(signtool)
                .args([
                    "sign",
                    "/sha1",
                    certificate,
                    "/fd",
                    "SHA256",
                    "/tr",
                    server,
                    "/td",
                    "SHA256",
                ])
                .arg(path),
        );
        match result {
            Ok(()) => return Ok(()),
            Err(error) if server == TIMESTAMP_SERVER => {
                eprintln!("Timestamped signing failed; retrying with Sectigo: {error}");
            }
            Err(error) => return Err(error),
        }
    }
    Err("Timestamped signing failed".into())
}

pub(crate) fn sdk_tool(name: &str) -> Result<PathBuf> {
    let sdk = PathBuf::from(
        std::env::var_os("ProgramFiles(x86)").ok_or("Windows SDK directory unavailable")?,
    )
    .join("Windows Kits/10/bin");
    let mut versions: Vec<_> = fs::read_dir(&sdk)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().join("x64").join(name).is_file())
        .collect();
    versions.sort_by_key(|entry| {
        entry
            .file_name()
            .to_string_lossy()
            .split('.')
            .map(|p| p.parse::<u32>().unwrap_or(0))
            .collect::<Vec<_>>()
    });
    Ok(versions
        .last()
        .ok_or("Windows SDK signing tools missing")?
        .path()
        .join("x64")
        .join(name))
}

pub(crate) fn prerequisite(assets: &Assets) -> String {
    let payloads = [
        "DaatLocusVirtualDisplay.inf",
        "DaatLocusVirtualDisplay.dll",
        "DaatLocusVirtualDisplay.cat",
        "LICENSE.microsoft",
    ]
    .map(|name| {
        format!(
            "<Payload Name=\"{}\" SourceFile=\"{}\"/>",
            xml(name),
            xml(&assets.driver.join(name).to_string_lossy())
        )
    })
    .join("\n");
    // Burn 6 caches requested-present machine packages even with no elevated
    // execution planned. Skip an unchanged permanent prerequisite entirely during
    // Install; Repair (BOOTSTRAPPER_ACTION_REPAIR = 8) must still execute it.
    // Keep the source for repair without requesting cache/cleanup on the skip path.
    let present = format!(
        "VirtualDisplayPackage = \"{}\" AND VirtualDisplayRoot AND VirtualDisplayPublisher",
        assets.package_hash
    );
    let install = format!("NOT ({present}) OR WixBundleAction = 8");
    format!(
        r#"<ExePackage Id="VirtualDisplay" SourceFile="{}" InstallArguments="install" RepairArguments="install" DetectCondition="{}" InstallCondition="{}" Permanent="yes" PerMachine="yes" Vital="yes" Cache="keep">
<ExitCode Value="0" Behavior="success"/><ExitCode Value="3010" Behavior="scheduleReboot"/><ExitCode Behavior="error"/>
{}
</ExePackage>"#,
        xml(&assets.setup.to_string_lossy()),
        xml(&present),
        xml(&install),
        payloads
    )
}

pub(crate) fn detection() -> String {
    format!(
        r#"<Variable Name="VirtualDisplayPackage" Type="string" Value=""/>
<util:RegistrySearch Id="VirtualDisplayPackageSearch" Variable="VirtualDisplayPackage" Root="HKLM" Key="{}" Value="PackageSha256" Result="value" Bitness="always64"/>
<util:RegistrySearch Id="VirtualDisplayRootSearch" Variable="VirtualDisplayRoot" Root="HKLM" Key="SOFTWARE\Microsoft\SystemCertificates\ROOT\Certificates\{}" Value="Blob" Result="exists" Bitness="always64"/>
<util:RegistrySearch Id="VirtualDisplayPublisherSearch" Variable="VirtualDisplayPublisher" Root="HKLM" Key="SOFTWARE\Microsoft\SystemCertificates\TrustedPublisher\Certificates\{}" Value="Blob" Result="exists" Bitness="always64"/>"#,
        xml(TRUST_KEY),
        CERTIFICATE_THUMBPRINT,
        CERTIFICATE_THUMBPRINT
    )
}

/// Transient, unelevated transaction steps. Burn reverses them around MSI rollback:
/// stop -> driver/MSI -> resume; rollback stops restored processes before replacing
/// files and resumes the old installation afterwards. DetectCondition=0 prevents
/// these coordination steps from running during a later product uninstall.
pub(crate) fn service_lifecycle(
    companion: &std::path::Path,
    installation: &str,
) -> Result<(String, String)> {
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
            r#"<ExePackage Id="{id}" Name="{id}.exe" CacheId="DaatLocus-{id}-{digest}" SourceFile="{}" PerMachine="no" Permanent="no" Vital="yes" DetectCondition="0" InstallCondition="WixBundleAction &lt;&gt; 3" InstallArguments="{}" RepairArguments="{}" UninstallArguments="{}"><ExitCode Value="0" Behavior="success"/><ExitCode Behavior="error"/></ExePackage>"#,
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

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::{mem::size_of, ptr};
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    #[test]
    fn timestamp_signing_falls_back_only_for_the_default_and_propagates_failure() {
        for (server, allow_backup, expected, success) in [
            (
                "http://timestamp.digicert.com",
                true,
                vec![
                    "http://timestamp.digicert.com",
                    "http://timestamp.sectigo.com",
                ],
                true,
            ),
            (
                "http://timestamp.digicert.com",
                false,
                vec![
                    "http://timestamp.digicert.com",
                    "http://timestamp.sectigo.com",
                ],
                false,
            ),
            (
                "https://timestamp.example.invalid",
                true,
                vec!["https://timestamp.example.invalid"],
                false,
            ),
        ] {
            let temporary = tempfile::tempdir().unwrap();
            let signer = temporary.path().join("signer.cmd");
            fs::write(
                &signer,
                format!(
                    "@echo off\r\necho %~7>>\"%~dp0attempts.txt\"\r\n{}exit /b 1\r\n",
                    if allow_backup {
                        "if \"%~7\"==\"http://timestamp.sectigo.com\" exit /b 0\r\n"
                    } else {
                        ""
                    }
                ),
            )
            .unwrap();
            let result = sign_file(
                &signer,
                &temporary.path().join("artifact.dll"),
                "fixture",
                server,
            );
            assert_eq!(result.is_ok(), success);
            let attempts = fs::read_to_string(temporary.path().join("attempts.txt")).unwrap();
            assert_eq!(attempts.lines().collect::<Vec<_>>(), expected);
        }
    }

    struct InstalledFixture {
        product: String,
        log: PathBuf,
    }

    #[test]
    #[ignore = "requires WiX 6/Bal and DAAT_LOCUS_TEST_COMPANION pointing to the newly built CLI; starts isolated desktop services, run serially"]
    fn service_lifecycle_surrounds_msi_and_restores_the_service_after_rollback() {
        fn bounded(command: &mut Command) -> std::process::ExitStatus {
            let mut child = command.spawn().unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    return status;
                }
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("installer fixture exceeded its deadline");
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
        let companion = PathBuf::from(
            std::env::var_os("DAAT_LOCUS_TEST_COMPANION")
                .expect("set DAAT_LOCUS_TEST_COMPANION to the newly built daat-locus.exe"),
        );
        assert!(companion.is_file());
        for failure_stage in [0, 1, 2] {
            let fail = failure_stage != 0;
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path();
            let installed = root.join("installed");
            fs::create_dir(&installed).unwrap();
            let binary = installed.join("daat-locus.exe");
            fs::copy(&companion, &binary).unwrap();
            let namespace = root.join("account");
            let cli = || {
                let mut command = Command::new(&binary);
                command
                    .env("DAAT_LOCUS_HOME", &namespace)
                    .env_remove("DAAT_LOCUS_PROFILE_NAME")
                    .env_remove("DAAT_LOCUS_ACCOUNT_STORAGE_ROOT")
                    .stdin(std::process::Stdio::null());
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x08000000);
                command
            };
            struct RunningService(Command);
            impl Drop for RunningService {
                fn drop(&mut self) {
                    let _ = self.0.arg("stop").output();
                }
            }
            let _service = RunningService(cli());
            let startup = cli()
                .args(["start", "--listen", "127.0.0.1:0", "--workspace"])
                .arg(root.join("workspace"))
                .output()
                .unwrap();
            assert!(
                startup.status.success(),
                "{}",
                String::from_utf8_lossy(&startup.stderr)
            );
            let status = || -> serde_json::Value {
                let output = cli().args(["status", "--json"]).output().unwrap();
                assert!(output.status.success());
                serde_json::from_slice(&output.stdout).unwrap()
            };
            let before = status();
            let guid = |salt: &str| {
                let hash = Sha256::digest(format!("{}:{salt}", root.display()).as_bytes());
                let hex: String = hash
                    .iter()
                    .take(16)
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
                format!(
                    "{{{}-{}-{}-{}-{}}}",
                    &hex[..8],
                    &hex[8..12],
                    &hex[12..16],
                    &hex[16..20],
                    &hex[20..]
                )
            };
            let product = guid("product");
            let payload = root.join("fixture.txt");
            fs::write(&payload, b"committed fixture content").unwrap();
            let source = root.join("fixture.wxs");
            let msi = root.join("fixture.msi");
            fs::write(&source, format!(r#"<Wix xmlns="http://wixtoolset.org/schemas/v4/wxs">
<Package Name="DL temporary lifecycle regression" Manufacturer="Daat Locus" Version="1.0.0" ProductCode="{}" UpgradeCode="{}" Scope="perUser">
<MediaTemplate EmbedCab="yes"/><StandardDirectory Id="LocalAppDataFolder"><Directory Id="FIXTUREFOLDER" Name="DaatLocusLifecycleTest"><Component Id="Fixture"><File Source="{}" KeyPath="yes"/></Component></Directory></StandardDirectory>
<Feature Id="FixtureFeature"><ComponentRef Id="Fixture"/></Feature></Package></Wix>"#,
                product, guid("upgrade"), xml(&payload.to_string_lossy()))).unwrap();
            command(
                Command::new("wix")
                    .arg("build")
                    .arg(&source)
                    .args(["-arch", "x64", "-o"])
                    .arg(&msi),
            )
            .unwrap();
            let _fixture = InstalledFixture {
                product,
                log: root.join("cleanup.log"),
            };
            let (pause, resume) =
                service_lifecycle(&companion, &installed.to_string_lossy()).unwrap();
            let license = root.join("license.rtf");
            fs::write(&license, b"{\\rtf1 Fixture}").unwrap();
            let failure = if fail {
                format!(
                    r#"<ExePackage Id="FailAfterMsi" SourceFile="{}" PerMachine="no" DetectCondition="0" Permanent="yes" InstallArguments="/d /c exit 23"><ExitCode Behavior="error"/></ExePackage>"#,
                    xml(&PathBuf::from(std::env::var_os("SystemRoot").unwrap())
                        .join("System32/cmd.exe")
                        .to_string_lossy())
                )
            } else {
                String::new()
            };
            let bundle = root.join("dl-lifecycle-test.exe");
            fs::write(&source, format!(r#"<Wix xmlns="http://wixtoolset.org/schemas/v4/wxs" xmlns:bal="http://wixtoolset.org/schemas/v4/wxs/bal">
<Bundle Name="DL temporary lifecycle regression" Manufacturer="Daat Locus" Version="1.0.0" UpgradeCode="{}">
<BootstrapperApplication><bal:WixStandardBootstrapperApplication Theme="rtfLicense" LicenseFile="{}"/></BootstrapperApplication>
<Chain>{}<MsiPackage SourceFile="{}"><MsiProperty Name="FIXTUREFOLDER" Value="{}"/></MsiPackage>{}{}{}</Chain></Bundle></Wix>"#,
                guid("bundle"), xml(&license.to_string_lossy()), pause, xml(&msi.to_string_lossy()), xml(&installed.to_string_lossy()),
                if failure_stage == 1 { &failure } else { "" }, resume,
                if failure_stage == 2 { &failure } else { "" })).unwrap();
            command(
                Command::new("wix")
                    .args([
                        "build",
                        "-ext",
                        "WixToolset.BootstrapperApplications.wixext",
                    ])
                    .arg(&source)
                    .arg("-o")
                    .arg(&bundle),
            )
            .unwrap();
            struct BundleFixture {
                path: PathBuf,
                namespace: PathBuf,
            }
            impl Drop for BundleFixture {
                fn drop(&mut self) {
                    let _ = Command::new(&self.path)
                        .env("DAAT_LOCUS_HOME", &self.namespace)
                        .args(["/uninstall", "/quiet", "/norestart"])
                        .output();
                }
            }
            let _bundle = BundleFixture {
                path: bundle.clone(),
                namespace: namespace.clone(),
            };
            let result = bounded(
                Command::new(&bundle)
                    .env("DAAT_LOCUS_HOME", &namespace)
                    .args(["/quiet", "/norestart", "/log"])
                    .arg(root.join("install.log")),
            );
            if result.success() == fail {
                let bytes = fs::read(root.join("install.log")).unwrap();
                let detail = if bytes.starts_with(&[0xff, 0xfe]) {
                    String::from_utf16_lossy(
                        &bytes[2..]
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .map(|b| u16::from_le_bytes(*b))
                            .collect::<Vec<_>>(),
                    )
                } else {
                    String::from_utf8_lossy(&bytes).into_owned()
                };
                eprintln!(
                    "{}",
                    detail
                        .lines()
                        .filter(|line| line.contains("e000:")
                            || line.contains("package:")
                            || line.contains("Apply complete"))
                        .collect::<Vec<_>>()
                        .join("\n")
                );
            }
            assert_eq!(
                result.success(),
                !fail,
                "unexpected bundle result: {result}; logs at {}",
                root.display()
            );
            let after = status();
            assert!(
                after.is_object(),
                "the previously running service must be restored even after rollback"
            );
            assert_ne!(after["instance"], before["instance"]);
            assert_eq!(after["options"], before["options"]);
            assert!(!namespace.join("runtime/installation.json").exists());
            if fail {
                assert!(!installed.join("fixture.txt").exists());
            } else {
                assert_eq!(
                    fs::read(installed.join("fixture.txt")).unwrap(),
                    b"committed fixture content"
                );
            }
            let uninstall = bounded(
                Command::new(&bundle)
                    .env("DAAT_LOCUS_HOME", &namespace)
                    .args(["/uninstall", "/quiet", "/norestart", "/log"])
                    .arg(root.join("uninstall.log")),
            );
            assert!(uninstall.success());
            assert!(!installed.join("fixture.txt").exists());
            assert_eq!(
                status()["instance"],
                after["instance"],
                "removing a bundle must not replay transient lifecycle steps"
            );
        }
    }
    impl Drop for InstalledFixture {
        fn drop(&mut self) {
            let _ = Command::new("msiexec.exe")
                .args(["/x", &self.product, "/qn", "/norestart", "/l*v"])
                .arg(&self.log)
                .output();
        }
    }

    #[test]
    #[ignore = "requires Windows WiX 6 with the Bal extension and a non-elevated token; installs/removes only a disposable per-user fixture"]
    fn present_machine_driver_does_not_require_machine_cache_for_user_install() {
        let mut token = ptr::null_mut();
        assert_ne!(
            unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) },
            0
        );
        let mut elevation = TOKEN_ELEVATION::default();
        let mut size = 0;
        let success = unsafe {
            GetTokenInformation(
                token,
                TokenElevation,
                (&mut elevation as *mut TOKEN_ELEVATION).cast(),
                size_of::<TOKEN_ELEVATION>() as u32,
                &mut size,
            )
        };
        unsafe {
            CloseHandle(token);
        }
        assert_ne!(success, 0);
        assert_eq!(
            elevation.TokenIsElevated, 0,
            "run this regression without elevation to exercise the reported failure"
        );

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        // Use Windows Installer's own GUID generator without any account/private data.
        let guid = || {
            let value = crate::output(Command::new("powershell.exe").args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[guid]::NewGuid().ToString('B')",
            ]))
            .unwrap();
            value.trim().to_owned()
        };
        let product = guid();
        let upgrade = guid();
        let bundle_upgrade = guid();
        let payload = root.join("fixture.txt");
        fs::write(
            &payload,
            b"Daat Locus per-user installer regression fixture\r\n",
        )
        .unwrap();
        let msi_source = root.join("fixture.wxs");
        let msi = root.join("fixture.msi");
        fs::write(&msi_source, format!(r#"<Wix xmlns="http://wixtoolset.org/schemas/v4/wxs">
<Package Name="Daat Locus temporary installer test" Manufacturer="Daat Locus" Version="1.0.0" ProductCode="{}" UpgradeCode="{}" Scope="perUser">
<MediaTemplate EmbedCab="yes"/>
<StandardDirectory Id="LocalAppDataFolder"><Directory Id="FIXTUREFOLDER" Name="DaatLocusInstallerTest"><Component Id="Fixture"><File Source="{}" KeyPath="yes"/><RemoveFolder Id="RemoveFixture" On="uninstall"/></Component></Directory></StandardDirectory>
<Feature Id="FixtureFeature"><ComponentRef Id="Fixture"/></Feature>
</Package></Wix>"#, product, upgrade, xml(&payload.to_string_lossy()))).unwrap();
        command(
            Command::new("wix")
                .arg("build")
                .arg(&msi_source)
                .args(["-arch", "x64", "-o"])
                .arg(&msi),
        )
        .expect("WiX 6 is required");

        let driver = root.join("driver");
        fs::create_dir(&driver).unwrap();
        for name in [
            "DaatLocusVirtualDisplay.inf",
            "DaatLocusVirtualDisplay.dll",
            "DaatLocusVirtualDisplay.cat",
            "LICENSE.microsoft",
        ] {
            fs::write(
                driver.join(name),
                b"Fixture package must never be cached or executed.",
            )
            .unwrap();
        }
        let assets = Assets {
            hook: root.join("unused.dll"),
            setup: std::env::current_exe().unwrap(),
            driver,
            package_hash: "fixture-present".into(),
        };
        let license = root.join("license.rtf");
        fs::write(&license, b"{\\rtf1 Fixture}").unwrap();
        let installed = root.join("installed");
        let source = root.join("bundle.wxs");
        let bundle = root.join("bundle.exe");
        // Model the actual prerequisite state from the failure: driver present,
        // no machine execution required, only a per-user MSI needs installation.
        fs::write(&source, format!(r#"<Wix xmlns="http://wixtoolset.org/schemas/v4/wxs" xmlns:bal="http://wixtoolset.org/schemas/v4/wxs/bal">
<Bundle Name="Daat Locus temporary cache regression" Manufacturer="Daat Locus" Version="1.0.0" UpgradeCode="{}">
<BootstrapperApplication><bal:WixStandardBootstrapperApplication Theme="rtfLicense" LicenseFile="{}"/></BootstrapperApplication>
<Variable Name="VirtualDisplayPackage" Type="string" Value="fixture-present"/>
<Variable Name="VirtualDisplayRoot" Type="numeric" Value="1"/>
<Variable Name="VirtualDisplayPublisher" Type="numeric" Value="1"/>
<Chain>{}<MsiPackage SourceFile="{}"><MsiProperty Name="FIXTUREFOLDER" Value="{}"/></MsiPackage></Chain>
</Bundle></Wix>"#, bundle_upgrade, xml(&license.to_string_lossy()), prerequisite(&assets), xml(&msi.to_string_lossy()), xml(&installed.to_string_lossy()))).unwrap();
        command(
            Command::new("wix")
                .args([
                    "build",
                    "-ext",
                    "WixToolset.BootstrapperApplications.wixext",
                ])
                .arg(&source)
                .arg("-o")
                .arg(&bundle),
        )
        .unwrap();
        let log = root.join("install.log");
        // Always remove the unique test product if an assertion later fails.
        let fixture = InstalledFixture {
            product,
            log: root.join("cleanup.log"),
        };
        let status = Command::new(&bundle)
            .args(["/quiet", "/norestart", "/log"])
            .arg(&log)
            .status()
            .unwrap();
        let bytes = fs::read(&log).unwrap();
        let detail = if bytes.starts_with(&[0xff, 0xfe]) {
            String::from_utf16_lossy(
                &bytes[2..]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .collect::<Vec<_>>(),
            )
        } else {
            String::from_utf8_lossy(&bytes).into_owned()
        };
        let tail = detail
            .lines()
            .filter(|line| line.contains("e000:") || line.contains("Apply complete"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            status.success(),
            "per-user fixture installation failed: {status}\n{tail}"
        );
        assert_eq!(
            fs::read(installed.join("fixture.txt")).unwrap(),
            b"Daat Locus per-user installer regression fixture\r\n"
        );
        let uninstall = Command::new(&bundle)
            .args(["/uninstall", "/quiet", "/norestart", "/log"])
            .arg(root.join("uninstall.log"))
            .status()
            .unwrap();
        assert!(
            uninstall.success(),
            "fixture bundle uninstall failed: {uninstall}"
        );
        assert!(!installed.join("fixture.txt").exists());
        drop(fixture);
    }
}
