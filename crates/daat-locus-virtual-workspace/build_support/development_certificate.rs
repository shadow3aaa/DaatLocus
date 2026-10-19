//! Shared by the debug setup helper and xtask. Never used for release identities.
use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub struct Identity {
    pub certificate: PathBuf,
    pub thumbprint: String,
}

pub fn ensure(root: &Path) -> Result<Identity, Box<dyn std::error::Error>> {
    let directory = root.join(".dev/windows-signing");
    // PowerShell supplies the Windows certificate/DPAPI APIs, not a separate build script.
    // Paths travel in environment variables; passwords and key bytes never leave this process.
    let output = Command::new("powershell.exe")
        .env_remove("PSModulePath")
        .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
        .env("DL_DEV_SIGNING_DIRECTORY", &directory)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "Development signing identity failed in {}: {}",
            directory.display(),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let thumbprint = String::from_utf8(output.stdout)?.trim().to_owned();
    if thumbprint.len() != 40 || !thumbprint.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("Development certificate returned an invalid thumbprint".into());
    }
    Ok(Identity {
        certificate: directory.join("certificate.cer"),
        thumbprint,
    })
}

const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Add-Type -AssemblyName System.Security
$directory = [IO.Path]::GetFullPath($env:DL_DEV_SIGNING_DIRECTORY)
$sid = [Security.Principal.WindowsIdentity]::GetCurrent().User
$hash = [Security.Cryptography.SHA256]::Create()
$name = [BitConverter]::ToString($hash.ComputeHash([Text.Encoding]::Unicode.GetBytes($directory.ToUpperInvariant()))).Replace('-', '')
$mutex = [Threading.Mutex]::new($false, "Local\DaatLocusDevSigning-$name")
$locked = $false
try {
    try { $locked = $mutex.WaitOne(120000) } catch [Threading.AbandonedMutexException] { $locked = $true }
    if (!$locked) { throw 'Another build is busy creating the development identity' }
    $acl = [Security.AccessControl.DirectorySecurity]::new()
    $acl.SetOwner($sid)
    $acl.SetAccessRuleProtection($true, $false)
    $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($sid, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow'))
    # Apply the owner ACL at creation, before any secret bytes are written.
    $null = [IO.Directory]::CreateDirectory($directory, $acl)
    $item = Get-Item -LiteralPath $directory -Force
    if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Signing directory cannot be a reparse point' }
    [IO.Directory]::SetAccessControl($directory, $acl)
    foreach ($file in Get-ChildItem -LiteralPath $directory -Force) {
        if ($file.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Signing files cannot be reparse points' }
        $fileAcl = [Security.AccessControl.FileSecurity]::new()
        $fileAcl.SetOwner($sid)
        $fileAcl.SetAccessRuleProtection($true, $false)
        $fileAcl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($sid, 'FullControl', 'Allow'))
        [IO.File]::SetAccessControl($file.FullName, $fileAcl)
    }
    $pfx = Join-Path $directory 'identity.pfx'
    $secret = Join-Path $directory 'password.dpapi'
    $public = Join-Path $directory 'certificate.cer'
    $existing = @($pfx, $secret, $public | Where-Object { Test-Path -LiteralPath $_ })
    if ($existing.Count -eq 0) {
        $bytes = [byte[]]::new(32)
        $rng = [Security.Cryptography.RandomNumberGenerator]::Create()
        $rng.GetBytes($bytes)
        $passwordText = [Convert]::ToBase64String($bytes)
        $password = ConvertTo-SecureString -String $passwordText -AsPlainText -Force
        $protected = [Security.Cryptography.ProtectedData]::Protect([Text.Encoding]::UTF8.GetBytes($passwordText), $null, [Security.Cryptography.DataProtectionScope]::CurrentUser)
        $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject 'CN=Daat Locus Local Development' -CertStoreLocation 'Cert:\CurrentUser\My' -KeyAlgorithm RSA -KeyLength 3072 -HashAlgorithm SHA256 -KeyExportPolicy Exportable -NotAfter (Get-Date).AddYears(5)
        [IO.File]::WriteAllBytes($secret, $protected)
        $null = Export-PfxCertificate -Cert $cert -FilePath $pfx -Password $password
        $null = Export-Certificate -Cert $cert -FilePath $public -Type CERT
    } elseif ($existing.Count -ne 3) {
        throw 'Incomplete development identity; restore its files or deliberately remove this directory to create a new identity'
    }
    $passwordText = [Text.Encoding]::UTF8.GetString([Security.Cryptography.ProtectedData]::Unprotect([IO.File]::ReadAllBytes($secret), $null, [Security.Cryptography.DataProtectionScope]::CurrentUser))
    $password = ConvertTo-SecureString -String $passwordText -AsPlainText -Force
    $data = Get-PfxData -FilePath $pfx -Password $password
    $cert = [Security.Cryptography.X509Certificates.X509Certificate2]::new($public)
    if ($data.EndEntityCertificates.Count -ne 1 -or $data.EndEntityCertificates[0].Thumbprint -ne $cert.Thumbprint) { throw 'Development PFX/public certificate mismatch' }
    if ($cert.NotAfter -le (Get-Date) -or $cert.NotBefore -gt (Get-Date)) { throw 'Development certificate is outside its validity period; renew explicitly' }
    $stored = Get-Item -LiteralPath "Cert:\CurrentUser\My\$($cert.Thumbprint)" -ErrorAction SilentlyContinue
    if (!$stored -or !$stored.HasPrivateKey) {
        $stored = Import-PfxCertificate -FilePath $pfx -Password $password -CertStoreLocation 'Cert:\CurrentUser\My' -Exportable
    }
    if (!$stored.HasPrivateKey) { throw 'Development signing key is unavailable' }
    [Console]::Out.Write($cert.Thumbprint)
} catch {
    # Do not serialize PowerShell's invocation/arguments, which may contain secrets.
    [Console]::Error.Write($_.Exception.Message)
    exit 1
} finally {
    if ($locked) { $mutex.ReleaseMutex() }
    $mutex.Dispose()
}
"#;

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    struct Fixture {
        root: tempfile::TempDir,
        thumbprint: String,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = powershell(
                "$path = 'Cert:\\CurrentUser\\My\\' + $env:DL_TEST_THUMBPRINT; if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -DeleteKey }",
                &self.thumbprint,
                self.root.path(),
            );
        }
    }
    fn powershell(script: &str, thumbprint: &str, root: &Path) -> std::process::Output {
        Command::new("powershell.exe")
            .env_remove("PSModulePath")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .env("DL_TEST_THUMBPRINT", thumbprint)
            .env("DL_TEST_ROOT", root)
            .output()
            .unwrap()
    }

    #[test]
    fn local_identity_reuses_keys_restores_store_and_rejects_damaged_backup() {
        let root = tempfile::tempdir().unwrap();
        let (first, concurrent) = std::thread::scope(|scope| {
            let a = scope.spawn(|| ensure(root.path()).map_err(|error| error.to_string()));
            let b = scope.spawn(|| ensure(root.path()).map_err(|error| error.to_string()));
            (a.join().unwrap().unwrap(), b.join().unwrap().unwrap())
        });
        let fixture = Fixture {
            root,
            thumbprint: first.thumbprint.clone(),
        };
        assert_eq!(first.thumbprint, concurrent.thumbprint);
        let directory = first.certificate.parent().unwrap();
        let public = std::fs::read(&first.certificate).unwrap();
        let pfx = std::fs::read(directory.join("identity.pfx")).unwrap();
        let password = std::fs::read(directory.join("password.dpapi")).unwrap();
        let check = powershell(
            r#"
$ErrorActionPreference = 'Stop'
$directory = Join-Path $env:DL_TEST_ROOT '.dev/windows-signing'
$sid = [Security.Principal.WindowsIdentity]::GetCurrent().User
foreach ($path in @($directory) + @(Get-ChildItem -LiteralPath $directory -Force | ForEach-Object FullName)) {
    $acl = Get-Acl -LiteralPath $path
    if ($acl.GetOwner([Security.Principal.SecurityIdentifier]) -ne $sid) { throw 'Wrong owner' }
    foreach ($rule in $acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
        if ($rule.IdentityReference -ne $sid) { throw 'Non-owner access to signing material' }
    }
}
$cert = Get-Item -LiteralPath ('Cert:\CurrentUser\My\' + $env:DL_TEST_THUMBPRINT)
if (!$cert.HasPrivateKey -or $cert.EnhancedKeyUsageList.ObjectId -notcontains '1.3.6.1.5.5.7.3.3') { throw 'Not a signing identity' }
foreach ($store in @('Root', 'TrustedPublisher')) {
    if (Test-Path -LiteralPath "Cert:\CurrentUser\$store\$env:DL_TEST_THUMBPRINT") { throw 'Build must not provision trust' }
    if (Test-Path -LiteralPath "Cert:\LocalMachine\$store\$env:DL_TEST_THUMBPRINT") { throw 'Build must not provision machine trust' }
}
Remove-Item -LiteralPath ('Cert:\CurrentUser\My\' + $env:DL_TEST_THUMBPRINT) -DeleteKey
"#,
            &first.thumbprint,
            fixture.root.path(),
        );
        assert!(
            check.status.success(),
            "{}",
            String::from_utf8_lossy(&check.stderr)
        );
        // Reimport from the protected backup, without generating a new key/certificate.
        let second = ensure(fixture.root.path()).unwrap();
        assert_eq!(second.thumbprint, first.thumbprint);
        assert_eq!(std::fs::read(&second.certificate).unwrap(), public);
        assert_eq!(std::fs::read(directory.join("identity.pfx")).unwrap(), pfx);
        assert_eq!(
            std::fs::read(directory.join("password.dpapi")).unwrap(),
            password
        );
        assert_eq!(
            ensure(fixture.root.path()).unwrap().thumbprint,
            first.thumbprint
        );
        std::fs::write(directory.join("identity.pfx"), b"damaged backup").unwrap();
        assert!(ensure(fixture.root.path()).is_err());
        assert_eq!(std::fs::read(&first.certificate).unwrap(), public);
        assert_eq!(
            std::fs::read(directory.join("identity.pfx")).unwrap(),
            b"damaged backup"
        );
        std::fs::write(directory.join("identity.pfx"), pfx).unwrap();
        std::fs::remove_file(directory.join("password.dpapi")).unwrap();
        assert!(ensure(fixture.root.path()).is_err());
        assert_eq!(std::fs::read(&first.certificate).unwrap(), public);
    }
}
