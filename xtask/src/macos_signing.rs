//! A per-owner signing identity survives builds and checkouts. Never replace it
//! implicitly: TCC grants follow the certificate-based designated requirement.
use crate::{Result, command, reporting::status};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::{
        ffi::OsStringExt,
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub(crate) struct SigningIdentity {
    identity: String,
    keychain: Option<PathBuf>,
    // Serialize creation, unlocking and signing across concurrent package jobs.
    _lock: Option<File>,
}

impl SigningIdentity {
    pub(crate) fn resolve(identity: Option<&str>) -> Result<Self> {
        if let Some(identity) = identity {
            if identity.trim().is_empty() {
                return Err("macOS signing identity cannot be empty".into());
            }
            return Ok(Self {
                identity: identity.into(),
                keychain: None,
                _lock: None,
            });
        }
        let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?);
        let private = home.join(".daat-locus-private");
        private_directory(&private)?;
        Self::local(&private.join("macos-signing"))
    }

    fn local(root: &Path) -> Result<Self> {
        private_directory(root)?;
        let lock = private_file(&root.join("lock"), false)?;
        lock.lock()?;
        let password_path = root.join("password");
        let certificate = root.join("certificate.pem");
        let keychain = root.join("signing.keychain-db");
        let paths = [&password_path, &certificate, &keychain];
        let exists = paths
            .iter()
            .map(|path| path.try_exists())
            .collect::<std::io::Result<Vec<_>>>()?;
        if exists.iter().all(|exists| !exists) {
            // A failed, partially generated identity also needs explicit repair.
            if fs::read_dir(root)?
                .any(|entry| !entry.is_ok_and(|entry| entry.file_name() == "lock"))
            {
                return Err(format!("Incomplete macOS signing identity at {}; restore it rather than replacing its certificate", root.display()).into());
            }
            status("Creating", "Persistent local macOS code-signing identity");
            create_identity(root, &password_path, &certificate, &keychain)?;
        } else if !exists.iter().all(|exists| *exists) {
            return Err(format!("Incomplete macOS signing identity at {}; restore the original certificate, password and keychain", root.display()).into());
        }
        for path in paths {
            check_private_file(path)?;
        }
        let password = fs::read_to_string(&password_path)?;
        if password.len() != 64 || !password.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(
                "Invalid local signing keychain password file; restore the original identity"
                    .into(),
            );
        }
        let signing = Self {
            identity: fingerprint(&certificate)?,
            keychain: Some(keychain),
            _lock: Some(lock),
        };
        let chain = signing.keychain.as_ref().unwrap();
        security(&["unlock-keychain", "-p", &password, path_text(chain)?])?;
        openssl(&[
            "x509",
            "-in",
            path_text(&certificate)?,
            "-noout",
            "-checkend",
            "0",
        ])?;
        let matching = Command::new("/usr/bin/security")
            .args(["find-identity", "-p", "codesigning"])
            .arg(chain)
            .output()?;
        if !matching.status.success()
            || !String::from_utf8_lossy(&matching.stdout).contains(&signing.identity)
        {
            return Err("The local signing certificate and keychain do not contain the same identity; restore the original identity".into());
        }
        // Trust only code signing in this user's trust domain, never TLS or the
        // system trust domain. The OS may ask the owner to authenticate once.
        if !Command::new("/usr/bin/security")
            .args([
                "verify-cert",
                "-c",
                path_text(&certificate)?,
                "-p",
                "codeSign",
            ])
            .output()?
            .status
            .success()
        {
            status(
                "Trusting",
                "Local code-signing certificate (macOS may request authentication)",
            );
            security(&[
                "add-trusted-cert",
                "-r",
                "trustRoot",
                "-p",
                "codeSign",
                "-k",
                path_text(chain)?,
                path_text(&certificate)?,
            ])?;
        }
        let identities = Command::new("/usr/bin/security")
            .args(["find-identity", "-v", "-p", "codesigning"])
            .arg(chain)
            .output()?;
        if !identities.status.success()
            || !String::from_utf8_lossy(&identities.stdout).contains(&signing.identity)
        {
            return Err("The persistent macOS signing identity is unavailable or expired; restore its original keychain/certificate, or explicitly select another signing identity".into());
        }
        register_keychain(chain)?;
        status("Signing", &format!("Local identity {}", signing.identity));
        // Retain the guard through both nested and bundle signatures.
        Ok(signing)
    }

    pub(crate) fn sign_app(&self, app: &Path) -> Result<()> {
        // Seal the nested companion before the complete bundle and its resources.
        let mut nested = self.codesign();
        command(
            nested
                .args(["--identifier", "io.shadow3aaa.daatlocus.service"])
                .arg(app.join("Contents/MacOS/daat-locus")),
        )?;
        command(self.codesign().arg(app))?;
        command(
            Command::new("/usr/bin/codesign")
                .args(["--verify", "--deep", "--strict"])
                .arg(app),
        )
    }

    fn codesign(&self) -> Command {
        let mut cmd = Command::new("/usr/bin/codesign");
        cmd.args(["--force", "--sign", &self.identity, "--timestamp=none"]);
        if let Some(keychain) = &self.keychain {
            cmd.arg("--keychain").arg(keychain);
        }
        cmd
    }
}

// codesign's identity lookup still uses the search list with --keychain. Read
// paths through Security.framework to preserve even quoted/non-UTF-8 entries.
fn register_keychain(keychain: &Path) -> Result<()> {
    use std::ffi::{OsString, c_void};
    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        fn SecKeychainCopyDomainSearchList(domain: u32, list: *mut *const c_void) -> i32;
        fn SecKeychainGetPath(keychain: *const c_void, length: *mut u32, path: *mut u8) -> i32;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFArrayGetCount(array: *const c_void) -> isize;
        fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
        fn CFRelease(value: *const c_void);
    }
    struct SearchList(*const c_void);
    impl Drop for SearchList {
        fn drop(&mut self) {
            unsafe { CFRelease(self.0) }
        }
    }
    let mut list = std::ptr::null();
    // kSecPreferencesDomainUser = 0; do not copy system/dynamic entries into
    // the user's persisted list.
    let result = unsafe { SecKeychainCopyDomainSearchList(0, &mut list) };
    if result != 0 || list.is_null() {
        return Err(format!("Cannot read Keychain search list ({result})").into());
    }
    let list = SearchList(list);
    let mut paths = Vec::new();
    for index in 0..unsafe { CFArrayGetCount(list.0) } {
        let item = unsafe { CFArrayGetValueAtIndex(list.0, index) };
        let mut buffer = vec![0u8; 4096];
        let mut length = buffer.len() as u32;
        let result = unsafe { SecKeychainGetPath(item, &mut length, buffer.as_mut_ptr()) };
        if result != 0 || length as usize > buffer.len() {
            return Err(format!("Cannot preserve a Keychain search path ({result})").into());
        }
        buffer.truncate(length as usize);
        paths.push(PathBuf::from(OsString::from_vec(buffer)));
    }
    if !paths.iter().any(|path| path == keychain) {
        command(
            Command::new("/usr/bin/security")
                .args(["list-keychains", "-d", "user", "-s"])
                .args(paths)
                .arg(keychain),
        )?;
    }
    Ok(())
}

impl Drop for SigningIdentity {
    fn drop(&mut self) {
        if let Some(keychain) = &self.keychain {
            let _ = Command::new("/usr/bin/security")
                .arg("lock-keychain")
                .arg(keychain)
                .output();
        }
    }
}

fn private_directory(path: &Path) -> Result<()> {
    if !path.try_exists()? {
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(path) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(format!(
            "Signing directory must be an owner-only directory: {}",
            path.display()
        )
        .into());
    }
    Ok(())
}

fn private_file(path: &Path, exclusive: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(!exclusive)
        .create_new(exclusive)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    check_metadata(path, &file.metadata()?)?;
    Ok(file)
}

fn check_metadata(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(format!(
            "Signing file must be an owner-only regular file: {}",
            path.display()
        )
        .into());
    }
    Ok(())
}

fn check_private_file(path: &Path) -> Result<()> {
    check_metadata(path, &fs::symlink_metadata(path)?)
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| "Signing paths must be UTF-8".into())
}

fn security_quote(value: &str) -> Result<String> {
    if value.contains(['\n', '\r', '\0']) {
        return Err("Invalid character in security command argument".into());
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

fn security(args: &[&str]) -> Result<()> {
    // Interactive mode reads secrets from stdin instead of exposing passwords
    // in process arguments or the generic command failure diagnostic.
    let line = args
        .iter()
        .map(|arg| security_quote(arg))
        .collect::<Result<Vec<_>>>()?
        .join(" ")
        + "\n";
    let mut child = Command::new("/usr/bin/security")
        .arg("-i")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("Missing security input pipe")?
        .write_all(line.as_bytes())?;
    if !child.wait()?.success() {
        return Err(format!(
            "macOS security {} failed; the original local signing identity is retained",
            args[0]
        )
        .into());
    }
    Ok(())
}

fn openssl(args: &[&str]) -> Result<String> {
    openssl_input(args, None)
}

fn openssl_input(args: &[&str], input: Option<&str>) -> Result<String> {
    let mut child = Command::new("/usr/bin/openssl")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(input) = input {
        child
            .stdin
            .take()
            .ok_or("Missing OpenSSL input pipe")?
            .write_all(input.as_bytes())?;
    } else {
        drop(child.stdin.take());
    }
    let result = child.wait_with_output()?;
    if !result.status.success() {
        return Err(format!("Local signing certificate operation ({}) failed", args[0]).into());
    }
    Ok(String::from_utf8(result.stdout)?)
}

fn fingerprint(certificate: &Path) -> Result<String> {
    openssl(&[
        "x509",
        "-in",
        path_text(certificate)?,
        "-noout",
        "-fingerprint",
        "-sha1",
    ])?
    .trim()
    .split_once('=')
    .map(|(_, hash)| hash.replace(':', ""))
    .filter(|hash| hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
    .ok_or_else(|| "Invalid local signing certificate fingerprint".into())
}

fn create_identity(
    root: &Path,
    password_path: &Path,
    certificate: &Path,
    keychain: &Path,
) -> Result<()> {
    let password = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    private_file(password_path, true)?.write_all(password.as_bytes())?;
    let key = root.join("private-key.pem");
    let archive = root.join("identity.p12");
    let config = root.join("openssl.cnf");
    for path in [&key, &archive, certificate, &config] {
        private_file(path, true)?;
    }
    fs::write(
        &config,
        "[req]\nprompt=no\ndistinguished_name=name\nx509_extensions=ext\n[name]\nCN=Daat Locus Local Code Signing\nO=Daat Locus Local Builds\n[ext]\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=critical,codeSigning\nsubjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid\n",
    )?;
    let pass_source = format!("file:{}", path_text(password_path)?);
    openssl(&[
        "req",
        "-x509",
        "-newkey",
        "rsa:3072",
        "-days",
        "3650",
        "-sha256",
        "-config",
        path_text(&config)?,
        "-passout",
        &pass_source,
        "-keyout",
        path_text(&key)?,
        "-out",
        path_text(certificate)?,
    ])?;
    openssl_input(
        &[
            "pkcs12",
            "-export",
            "-in",
            path_text(certificate)?,
            "-inkey",
            path_text(&key)?,
            "-out",
            path_text(&archive)?,
            "-passin",
            "stdin",
            "-passout",
            &pass_source,
        ],
        Some(&format!("{password}\n")),
    )?;
    security(&["create-keychain", "-p", &password, path_text(keychain)?])?;
    fs::set_permissions(keychain, fs::Permissions::from_mode(0o600))?;
    security(&["unlock-keychain", "-p", &password, path_text(keychain)?])?;
    security(&[
        "import",
        path_text(&archive)?,
        "-f",
        "pkcs12",
        "-k",
        path_text(keychain)?,
        "-P",
        &password,
        "-T",
        "/usr/bin/codesign",
        "-x",
    ])?;
    security(&[
        "set-key-partition-list",
        "-S",
        "apple-tool:,apple:,codesign:",
        "-s",
        "-k",
        &password,
        path_text(keychain)?,
    ])?;
    // The encrypted PKCS#12 and dedicated keychain retain the original key;
    // no unencrypted private key is ever written, including during generation.
    fs::remove_file(key)?;
    fs::remove_file(config)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_identity_imports_with_its_encrypted_backup() {
        let directory = tempfile::Builder::new()
            .prefix("dl signing '")
            .tempdir()
            .unwrap();
        let root = directory.path();
        let password = root.join("password");
        let certificate = root.join("certificate.pem");
        let keychain = root.join("fixture.keychain-db");
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = Command::new("/usr/bin/security")
                    .arg("delete-keychain")
                    .arg(&self.0)
                    .output();
            }
        }
        let _cleanup = Cleanup(keychain.clone());
        create_identity(root, &password, &certificate, &keychain).unwrap();
        for path in [
            &password,
            &certificate,
            &keychain,
            &root.join("identity.p12"),
        ] {
            check_private_file(path).unwrap();
        }
        let identities = Command::new("/usr/bin/security")
            .args(["find-identity", "-p", "codesigning"])
            .arg(&keychain)
            .output()
            .unwrap();
        assert!(identities.status.success());
        assert!(
            String::from_utf8(identities.stdout)
                .unwrap()
                .contains(&fingerprint(&certificate).unwrap())
        );
        let backup = openssl(&[
            "pkcs12",
            "-in",
            path_text(&root.join("identity.p12")).unwrap(),
            "-passin",
            &format!("file:{}", password.display()),
            "-nokeys",
        ])
        .unwrap();
        assert!(backup.contains(fs::read_to_string(&certificate).unwrap().trim()));
        assert!(!root.join("private-key.pem").exists());
    }

    #[test]
    fn incomplete_identity_is_not_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("signing");
        private_directory(&root).unwrap();
        private_file(&root.join("password"), true)
            .unwrap()
            .write_all(b"original")
            .unwrap();
        assert!(SigningIdentity::local(&root).is_err());
        assert_eq!(fs::read(root.join("password")).unwrap(), b"original");
        assert!(!root.join("certificate.pem").exists());
    }

    #[test]
    fn signing_storage_rejects_symlinks_and_shared_files() {
        let directory = tempfile::tempdir().unwrap();
        let secret = directory.path().join("secret");
        private_file(&secret, true)
            .unwrap()
            .write_all(b"original")
            .unwrap();
        let link = directory.path().join("link");
        std::os::unix::fs::symlink(&secret, &link).unwrap();
        assert!(private_file(&link, false).is_err());
        assert!(private_directory(&link).is_err());
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(private_file(&secret, false).is_err());
        assert_eq!(fs::read(&secret).unwrap(), b"original");
    }
}
