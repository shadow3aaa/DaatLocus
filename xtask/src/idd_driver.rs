use clap::Args;
use std::path::{Path, PathBuf};

pub(crate) fn distribution(
    root: &Path,
    certificate_thumbprint: Option<String>,
    timestamp_server: &str,
) -> crate::Result<PathBuf> {
    build(
        root,
        Options {
            wdk_root: None,
            sdk_root: None,
            check: false,
            test: false,
            certificate_thumbprint,
            timestamp_server: timestamp_server.into(),
        },
    )?;
    Ok(root.join("target/virtual-workspace-driver"))
}

#[derive(Args)]
pub(crate) struct Options {
    /// Use an existing WDK instead of restoring the pinned, hash-verified NuGet package.
    #[arg(long)]
    wdk_root: Option<PathBuf>,
    #[arg(long)]
    sdk_root: Option<PathBuf>,
    /// Run strict Clippy for the driver instead of producing a package.
    #[arg(long)]
    check: bool,
    /// Run driver unit tests with the same WDK and static-CRT configuration.
    #[arg(long, conflicts_with = "check")]
    test: bool,
    /// Existing CurrentUser/My code-signing certificate; no certificates are created/imported.
    #[arg(long)]
    certificate_thumbprint: Option<String>,
    #[arg(long, default_value = crate::computer_use::TIMESTAMP_SERVER)]
    timestamp_server: String,
}

#[cfg(not(windows))]
pub(crate) fn build(_: &Path, _: Options) -> crate::Result<()> {
    Err("IDD builds require a Windows x64 host, the Windows SDK and MSVC tools".into())
}

#[cfg(windows)]
pub(crate) fn build(root: &Path, options: Options) -> crate::Result<()> {
    use crate::{command, output, reporting::status};
    use sha2::{Digest, Sha256};
    use std::{
        fs,
        io::{Read, Write},
        process::Command,
    };

    let deps = root.join("target/virtual-workspace-deps");
    let package = root.join("target/virtual-workspace-driver");
    fs::create_dir_all(&deps)?;
    fs::create_dir_all(&package)?;
    let wdk = match options.wdk_root {
        Some(path) => path,
        None => {
            let archive = deps.join("wdk-10.0.26100.6584.zip");
            if !archive.exists() {
                status("Downloading", "pinned Windows Driver Kit");
                let mut response = reqwest::blocking::Client::builder()
                    .timeout(std::time::Duration::from_secs(300)).build()?
                    .get("https://api.nuget.org/v3-flatcontainer/microsoft.windows.wdk.x64/10.0.26100.6584/microsoft.windows.wdk.x64.10.0.26100.6584.nupkg")
                    .send()?.error_for_status()?;
                let partial = archive.with_extension("partial");
                let mut destination = fs::File::create(&partial)?;
                std::io::copy(&mut response, &mut destination)?;
                destination.flush()?;
                drop(destination);
                fs::rename(partial, &archive)?;
            }
            let mut file = fs::File::open(&archive)?;
            let mut hash = Sha256::new();
            let mut buffer = [0; 65536];
            loop {
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                hash.update(&buffer[..count]);
            }
            let actual: String = hash
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            if actual != "c393d03dfb640b5c92f546b32f6770ef68cd3aaf691956e7d66d8e2c28a1b55e" {
                return Err("WDK package SHA256 mismatch; archive preserved for inspection".into());
            }
            let extracted = deps.join("wdk");
            let marker = extracted.join(".daat-locus-extracted");
            if !marker.exists() {
                status("Extracting", "Windows Driver Kit");
                zip::ZipArchive::new(fs::File::open(&archive)?)?.extract(&extracted)?;
                fs::write(marker, b"10.0.26100.6584")?;
            }
            extracted.join("c")
        }
    };
    let wdk = crate::absolute(root, wdk);
    let program_files = PathBuf::from(
        std::env::var_os("ProgramFiles(x86)").ok_or("ProgramFiles(x86) is unavailable")?,
    );
    let sdk = options
        .sdk_root
        .unwrap_or_else(|| program_files.join("Windows Kits/10"));
    let sdk = crate::absolute(root, sdk);
    let vs = PathBuf::from(
        output(
            Command::new(program_files.join("Microsoft Visual Studio/Installer/vswhere.exe")).args(
                [
                    "-latest",
                    "-products",
                    "*",
                    "-requires",
                    "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
                    "-property",
                    "installationPath",
                ],
            ),
        )?
        .trim(),
    );
    if vs.as_os_str().is_empty() {
        return Err("Visual Studio MSVC x64 tools are required".into());
    }
    let msvc = latest_version(&vs.join("VC/Tools/MSVC"))?;
    let sdk_version = latest_version(&sdk.join("Include"))?;
    let wdk_version = latest_version(&wdk.join("Include"))?;
    let includes = [
        vs.join("VC/Tools/MSVC").join(&msvc).join("include"),
        sdk.join("Include").join(&sdk_version).join("ucrt"),
        sdk.join("Include").join(&sdk_version).join("shared"),
        sdk.join("Include").join(&sdk_version).join("um"),
        wdk.join("Include/wdf/umdf/2.25"),
        wdk.join("Include").join(&wdk_version).join("um/iddcx/1.4"),
    ];
    let libraries = [
        vs.join("VC/Tools/MSVC").join(&msvc).join("lib/x64"),
        sdk.join("Lib").join(&sdk_version).join("ucrt/x64"),
        sdk.join("Lib").join(&sdk_version).join("um/x64"),
        wdk.join("Lib/wdf/umdf/x64/2.25"),
        wdk.join("Lib").join(&wdk_version).join("um/x64/iddcx/1.4"),
    ];
    for path in includes.iter().chain(libraries.iter()) {
        if !path.is_dir() {
            return Err(format!("Required WDK/SDK directory missing: {}", path.display()).into());
        }
    }
    let source = root.join("crates/daat-locus-virtual-workspace/driver");
    let target = root.join("target/virtual-workspace-rust-driver");
    let mut cargo = Command::new("cargo");
    cargo
        .current_dir(&source)
        .args([
            if options.test {
                "test"
            } else if options.check {
                "clippy"
            } else {
                "build"
            },
            "--locked",
            "--release",
            "--target",
            "x86_64-pc-windows-msvc",
        ])
        .env("DL_IDD_INCLUDE", std::env::join_paths(&includes)?)
        .env("LIB", std::env::join_paths(&libraries)?)
        .env("CARGO_TARGET_DIR", &target);
    if options.test {
        cargo.args(["--lib", "--", "--test-threads=1"]);
    } else if options.check {
        cargo.args(["--", "-D", "warnings"]);
    }
    command(&mut cargo)?;
    if options.check || options.test {
        return Ok(());
    }
    let dll = package.join("DaatLocusVirtualDisplay.dll");
    fs::copy(
        target.join("x86_64-pc-windows-msvc/release/daat_locus_idd_driver.dll"),
        &dll,
    )?;
    for name in ["DaatLocusVirtualDisplay.inf", "LICENSE.microsoft"] {
        fs::copy(source.join(name), package.join(name))?;
    }
    command(
        Command::new(
            wdk.join("tools")
                .join(&wdk_version)
                .join("x64/infverif.exe"),
        )
        .current_dir(wdk.join("build").join(&wdk_version).join("bin/x64"))
        .args(["/u", "/v"])
        .arg(package.join("DaatLocusVirtualDisplay.inf")),
    )?;
    let rules = wdk
        .join("build")
        .join(&wdk_version)
        .join("universalDDIs/x64");
    command(
        Command::new(
            wdk.join("bin")
                .join(&wdk_version)
                .join("x64/apivalidator.exe"),
        )
        .arg(format!(
            "-SupportedApiXmlFiles:{}",
            rules.join("UniversalDDIs.xml").display()
        ))
        .arg(format!(
            "-ModuleWhiteListXmlFiles:{}",
            rules.join("ModuleWhiteList.xml").display()
        ))
        .arg(format!("-BinaryPath:{}", dll.display())),
    )?;
    let signtool = sdk.join("bin").join(&sdk_version).join("x64/signtool.exe");
    let sign = |path: &Path| -> crate::Result<()> {
        if let Some(thumbprint) = &options.certificate_thumbprint {
            crate::computer_use::sign_file(&signtool, path, thumbprint, &options.timestamp_server)?;
        }
        Ok(())
    };
    // Signing changes the DLL hash: finish it before calculating the catalog.
    sign(&dll)?;
    command(
        Command::new(wdk.join("bin").join(&wdk_version).join("x86/Inf2Cat.exe"))
            .arg(format!("/driver:{}", package.display()))
            .args(["/os:10_X64", "/uselocaltime"]),
    )?;
    let cat = package.join("DaatLocusVirtualDisplay.cat");
    sign(&cat)?;
    if options.certificate_thumbprint.is_some() {
        command(
            Command::new(&signtool)
                .args(["verify", "/pa", "/v", "/c"])
                .arg(cat)
                .arg(dll)
                .arg(package.join("DaatLocusVirtualDisplay.inf")),
        )?;
    } else {
        status(
            "Unsigned",
            "sign the package before installation; Windows trust policy is unchanged",
        );
    }
    status("Packaged", &package.display().to_string());
    Ok(())
}

#[cfg(windows)]
fn latest_version(directory: &Path) -> crate::Result<std::ffi::OsString> {
    std::fs::read_dir(directory)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| {
            let name = entry.file_name();
            let numbers: Option<Vec<u32>> = name
                .to_str()?
                .split('.')
                .map(|part| part.parse().ok())
                .collect();
            Some((numbers?, name))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, name)| name)
        .ok_or_else(|| format!("No SDK/toolset version in {}", directory.display()).into())
}
