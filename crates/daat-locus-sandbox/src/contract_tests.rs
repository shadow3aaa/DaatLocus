//! Shared observable filesystem contract, run against each real native backend.
//! Only shell syntax and Windows host-DACL observations are platform-specific.

use std::{io, path::Path, process::ExitStatus, time::Duration};

use tokio::io::AsyncReadExt;

use super::test_support::native_sandbox_available;
use super::{
    FileSystemSandboxPolicy, SandboxAsyncChild, SandboxPolicy, SandboxProcessOptions, SandboxStdio,
    StrongFilesystemSandboxMode, WritableRoot,
};

fn policy(root: &Path) -> SandboxPolicy {
    SandboxPolicy {
        filesystem: FileSystemSandboxPolicy {
            full_disk_read: false,
            full_disk_write: false,
            readable_roots: vec![root.to_owned()],
            writable_roots: vec![WritableRoot {
                root: root.to_owned(),
                read_only_subpaths: vec![],
            }],
            deny_read_paths: vec![],
            deny_write_paths: vec![],

            bound_roots: vec![],
        },
        strong_filesystem: StrongFilesystemSandboxMode::Required,
    }
}

fn launch(policy: &SandboxPolicy, root: &Path, script: String) -> SandboxAsyncChild {
    let (program, args) = command(script);
    SandboxAsyncChild::spawn_shell(
        policy,
        &program,
        args,
        SandboxProcessOptions {
            current_dir: Some(root.to_owned()),
            stdin: SandboxStdio::Null,
            stdout: SandboxStdio::Piped,
            stderr: SandboxStdio::Piped,
            ..Default::default()
        },
    )
    .expect("required native sandbox must be available for the filesystem contract")
}

async fn collect(mut child: SandboxAsyncChild) -> (ExitStatus, String, String) {
    let mut out = child.take_stdout().unwrap();
    let mut err = child.take_stderr().unwrap();
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::try_join!(
            async {
                let mut value = String::new();
                out.read_to_string(&mut value).await?;
                Ok::<_, io::Error>(value)
            },
            async {
                let mut value = String::new();
                err.read_to_string(&mut value).await?;
                Ok::<_, io::Error>(value)
            },
            async {
                loop {
                    if let Some(status) = child.try_wait()? {
                        return Ok::<_, io::Error>(status);
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        )
    })
    .await;
    if result.is_err() {
        let _ = child.start_kill();
    }
    let (out, err, status) = result
        .expect("native filesystem contract timed out")
        .unwrap();
    (status, out, err)
}

#[tokio::test]
async fn native_writable_scope_allows_file_and_directory_rename_and_delete() {
    if !native_sandbox_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("work");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("host.txt"), "initial").unwrap();
    std::fs::create_dir(root.join("host-directory")).unwrap();
    std::fs::create_dir(root.join("host-empty-directory")).unwrap();
    std::fs::write(root.join("host-directory/child.txt"), "child").unwrap();
    #[cfg(target_os = "windows")]
    let before = dacl(&root);
    let script = writable_script(&root);
    let (status, out, err) = collect(launch(&policy(&root), &root, script)).await;
    assert!(status.success(), "{status}; stdout={out}; stderr={err}");
    assert!(out.contains("rename-delete-complete"), "stdout={out}");
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    #[cfg(target_os = "windows")]
    assert_eq!(dacl(&root), before, "sandbox changed the workspace DACL");
}

#[tokio::test]
async fn native_rename_and_delete_preserve_readonly_and_denied_targets() {
    if !native_sandbox_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("work");
    std::fs::create_dir(&root).unwrap();
    let readonly = root.join("readonly.txt");
    let denied = root.join("denied.txt");
    let readonly_dir = root.join("readonly-directory");
    let denied_dir = root.join("denied-directory");
    for directory in [&readonly_dir, &denied_dir] {
        std::fs::create_dir(directory).unwrap();
    }
    let files = [
        readonly.clone(),
        denied.clone(),
        readonly_dir.join("child.txt"),
        denied_dir.join("child.txt"),
    ];
    for path in &files {
        std::fs::write(path, "protected").unwrap();
    }
    let targets = [&readonly, &denied, &readonly_dir, &denied_dir];
    #[cfg(target_os = "windows")]
    let before = targets.map(|path| dacl(path));
    let mut policy = policy(&root);
    policy.filesystem.writable_roots[0].read_only_subpaths =
        vec![readonly.clone(), readonly_dir.clone()];
    policy.filesystem.deny_read_paths = vec![denied.clone(), denied_dir.clone()];
    policy.filesystem.deny_write_paths = vec![denied.clone(), denied_dir.clone()];
    let script = protected_script(&root);
    let (status, out, err) = collect(launch(&policy, &root, script)).await;
    assert!(status.success(), "{status}; stdout={out}; stderr={err}");
    assert!(out.contains("protected-targets-complete"), "stdout={out}");
    for path in &files {
        assert_eq!(std::fs::read_to_string(path).unwrap(), "protected");
    }
    for path in targets {
        let renamed = path.with_file_name(format!(
            "{}.renamed",
            path.file_name().unwrap().to_str().unwrap()
        ));
        assert!(
            path.exists(),
            "protected target removed: {}",
            path.display()
        );
        assert!(
            !renamed.exists(),
            "protected target renamed: {}",
            path.display()
        );
    }
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), targets.len());
    #[cfg(target_os = "windows")]
    assert_eq!(targets.map(|path| dacl(path)), before);
}

#[cfg(target_os = "windows")]
fn command(script: String) -> (String, Vec<String>) {
    use std::os::windows::ffi::OsStringExt;

    let mut buffer = vec![0u16; 32768];
    let length = unsafe {
        windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW(
            buffer.as_mut_ptr(),
            buffer.len() as u32,
        )
    } as usize;
    assert!(
        length > 0 && length < buffer.len(),
        "cannot locate Windows directory"
    );
    let shell = std::path::PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length]))
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    (
        shell.to_str().unwrap().to_owned(),
        vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-Command".into(),
            script,
        ],
    )
}

#[cfg(target_os = "windows")]
fn script_with_root(root: &Path, body: &str) -> String {
    format!(
        "$root = '{}';\n{body}",
        root.to_str().unwrap().replace('\'', "''")
    )
}

#[cfg(unix)]
fn command(script: String) -> (String, Vec<String>) {
    ("/bin/sh".into(), vec!["-c".into(), script])
}

#[cfg(unix)]
fn script_with_root(root: &Path, body: &str) -> String {
    format!(
        "root='{}'\n{body}",
        root.to_str().unwrap().replace('\'', "'\\''")
    )
}

fn writable_script(root: &Path) -> String {
    script_with_root(root, WRITABLE_SCRIPT)
}

fn protected_script(root: &Path) -> String {
    script_with_root(root, PROTECTED_SCRIPT)
}

#[cfg(target_os = "windows")]
const WRITABLE_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
# WriteAllText/AppendAllText close their handles before rename/delete.
[IO.File]::WriteAllText((Join-Path $root 'sandbox.txt'), 'initial')
foreach ($name in @('host.txt', 'sandbox.txt')) {
    $path = Join-Path $root $name
    $renamed = "$path.renamed"
    [Console]::WriteLine("append file: $name")
    [IO.File]::AppendAllText($path, '+append')
    if ([IO.File]::ReadAllText($path) -ne 'initial+append') { throw 'append mismatch' }
    [Console]::WriteLine("rename file: $name")
    Rename-Item -LiteralPath $path -NewName "$name.renamed"
    if (Test-Path -LiteralPath $path) { throw 'original file survived rename' }
    if ([IO.File]::ReadAllText($renamed) -ne 'initial+append') { throw 'rename lost content' }
    [Console]::WriteLine("delete file: $name")
    Remove-Item -LiteralPath $renamed -Force
    if (Test-Path -LiteralPath $renamed) { throw 'file survived delete' }
}
$null = [IO.Directory]::CreateDirectory((Join-Path $root 'empty-directory'))
$null = [IO.Directory]::CreateDirectory((Join-Path $root 'sandbox-directory'))
[IO.File]::WriteAllText((Join-Path $root 'sandbox-directory/child.txt'), 'child')
foreach ($name in @('host-directory', 'host-empty-directory', 'empty-directory', 'sandbox-directory')) {
    $path = Join-Path $root $name
    $renamed = "$path.renamed"
    [Console]::WriteLine("rename directory: $name")
    Rename-Item -LiteralPath $path -NewName "$name.renamed"
    if (Test-Path -LiteralPath $path) { throw 'original directory survived rename' }
    if (-not [IO.Directory]::Exists($renamed)) { throw 'renamed directory missing' }
    if ($name -notin @('empty-directory', 'host-empty-directory')) {
        if ([IO.File]::ReadAllText((Join-Path $renamed 'child.txt')) -ne 'child') {
            throw 'directory rename lost content'
        }
    }
    [Console]::WriteLine("delete directory: $name")
    Remove-Item -LiteralPath $renamed -Recurse -Force
    if (Test-Path -LiteralPath $renamed) { throw 'directory survived delete' }
}
[Console]::WriteLine('rename-delete-complete')
"#;

#[cfg(target_os = "windows")]
const PROTECTED_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
# Prove the same operations work on unprotected siblings in this process.
[IO.File]::WriteAllText((Join-Path $root 'control.txt'), 'control')
Rename-Item -LiteralPath (Join-Path $root 'control.txt') -NewName 'control.txt.renamed'
if ([IO.File]::ReadAllText((Join-Path $root 'control.txt.renamed')) -ne 'control') {
    throw 'control rename lost content'
}
Remove-Item -LiteralPath (Join-Path $root 'control.txt.renamed') -Force
$null = [IO.Directory]::CreateDirectory((Join-Path $root 'control-directory'))
Rename-Item -LiteralPath (Join-Path $root 'control-directory') -NewName 'control-directory.renamed'
if (-not [IO.Directory]::Exists((Join-Path $root 'control-directory.renamed'))) {
    throw 'control directory rename failed'
}
Remove-Item -LiteralPath (Join-Path $root 'control-directory.renamed') -Recurse -Force
foreach ($name in @('readonly.txt', 'denied.txt', 'readonly-directory', 'denied-directory')) {
    $path = Join-Path $root $name
    foreach ($operation in @('rename', 'delete')) {
        [Console]::WriteLine("deny ${operation}: $name")
        $blocked = $false
        try {
            if ($operation -eq 'rename') {
                Rename-Item -LiteralPath $path -NewName "$name.renamed"
            } else {
                Remove-Item -LiteralPath $path -Recurse -Force
            }
        } catch {
            $failure = $_
            $exception = $_.Exception
            while ($null -ne $exception) {
                if (($exception -is [UnauthorizedAccessException]) -or
                    (($exception.HResult -band 0xffff) -eq 5)) { $blocked = $true }
                $exception = $exception.InnerException
            }
            if (-not $blocked) { throw $failure }
        }
        if (-not $blocked) { throw "protected target allowed ${operation}: $name" }
    }
}
[Console]::WriteLine('protected-targets-complete')
"#;

#[cfg(unix)]
const WRITABLE_SCRIPT: &str = r#"
set -eu
printf initial > "$root/sandbox.txt"
for name in host.txt sandbox.txt; do
    path="$root/$name"
    renamed="$path.renamed"
    printf 'append file: %s\n' "$name"
    printf +append >> "$path"
    [ "$(cat "$path")" = initial+append ]
    printf 'rename file: %s\n' "$name"
    mv "$path" "$renamed"
    [ ! -e "$path" ]
    [ "$(cat "$renamed")" = initial+append ]
    printf 'delete file: %s\n' "$name"
    rm -f "$renamed"
    [ ! -e "$renamed" ]
done
mkdir "$root/empty-directory" "$root/sandbox-directory"
printf child > "$root/sandbox-directory/child.txt"
for name in host-directory host-empty-directory empty-directory sandbox-directory; do
    path="$root/$name"
    renamed="$path.renamed"
    printf 'rename directory: %s\n' "$name"
    mv "$path" "$renamed"
    [ ! -e "$path" ]
    [ -d "$renamed" ]
    if [ "$name" != empty-directory ] && [ "$name" != host-empty-directory ]; then
        [ "$(cat "$renamed/child.txt")" = child ]
    fi
    printf 'delete directory: %s\n' "$name"
    rm -rf "$renamed"
    [ ! -e "$renamed" ]
done
printf 'rename-delete-complete\n'
"#;

#[cfg(unix)]
const PROTECTED_SCRIPT: &str = r#"
set -eu
# Match native filesystem-denial diagnostics, not a missing/broken command.
export LC_ALL=C
# Prove the same operations work on unprotected siblings in this process.
printf control > "$root/control.txt"
mv "$root/control.txt" "$root/control.txt.renamed"
[ "$(cat "$root/control.txt.renamed")" = control ]
rm -f "$root/control.txt.renamed"
mkdir "$root/control-directory"
mv "$root/control-directory" "$root/control-directory.renamed"
[ -d "$root/control-directory.renamed" ]
rm -rf "$root/control-directory.renamed"
for name in readonly.txt denied.txt readonly-directory denied-directory; do
    path="$root/$name"
    for operation in rename delete; do
        printf 'deny %s: %s\n' "$operation" "$name"
        if [ "$operation" = rename ]; then
            if failure=$(mv "$path" "$path.renamed" 2>&1); then
                printf 'protected target allowed rename: %s\n' "$name" >&2
                exit 20
            fi
        else
            if failure=$(rm -rf "$path" 2>&1); then
                printf 'protected target allowed delete: %s\n' "$name" >&2
                exit 21
            fi
        fi
        case "$failure" in
            *'Permission denied'*|*'Operation not permitted'*|*'Read-only file system'*|*'Device or resource busy'*) ;;
            *) printf 'unexpected %s failure for %s: %s\n' "$operation" "$name" "$failure" >&2; exit 22 ;;
        esac
    done
done
printf 'protected-targets-complete\n'
"#;

#[cfg(target_os = "windows")]
fn dacl(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT},
            DACL_SECURITY_INFORMATION,
        },
    };
    let name = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut descriptor = std::ptr::null_mut();
    let mut acl = std::ptr::null_mut();
    let result = unsafe {
        GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut acl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    assert_eq!(result, 0);
    let bytes = if acl.is_null() {
        vec![]
    } else {
        unsafe { std::slice::from_raw_parts(acl.cast::<u8>(), (*acl).AclSize as usize).to_vec() }
    };
    unsafe {
        LocalFree(descriptor);
    }
    bytes
}
