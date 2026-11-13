use super::*;
use std::path::PathBuf;

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
        let (pause, resume) = service_lifecycle(&companion, &installed.to_string_lossy()).unwrap();
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
