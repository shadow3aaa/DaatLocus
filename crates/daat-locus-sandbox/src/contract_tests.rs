//! Shared observable filesystem contract, run against each real native backend.
//! Only shell syntax and Windows host-DACL observations are platform-specific.

use std::{io, path::Path, process::ExitStatus, time::Duration};

use tokio::io::AsyncReadExt;

use super::test_support::{native_sandbox_available, scoped_directory};
use super::{
    FileSystemSandboxPolicy, SandboxAsyncChild, SandboxPolicy, SandboxProcessOptions, SandboxStdio,
    StrongFilesystemSandboxMode, WritableRoot,
};

fn policy(root: &Path) -> SandboxPolicy {
    SandboxPolicy {
        filesystem: FileSystemSandboxPolicy {
            full_disk_read: false,
            deny_all_writes: false,
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
                let mut value = Vec::new();
                out.read_to_end(&mut value).await?;
                Ok::<_, io::Error>(String::from_utf8_lossy(&value).into_owned())
            },
            async {
                let mut value = Vec::new();
                err.read_to_end(&mut value).await?;
                Ok::<_, io::Error>(String::from_utf8_lossy(&value).into_owned())
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
async fn native_temporary_paths_share_host_bytes_under_the_common_policy() {
    if !native_sandbox_available() {
        return;
    }
    let root = scoped_directory();
    let scratch = tempfile::tempdir().unwrap();
    let input = scratch.path().join("from-host.txt");
    let output = scratch.path().join("from-child.txt");
    std::fs::write(&input, "shared-scratch").unwrap();
    let policy = policy(root.path());
    assert!(policy.is_path_readable(&input));
    assert!(policy.is_path_writable(&output));
    #[cfg(windows)]
    let script = format!(
        "$ErrorActionPreference='Stop'; if ([IO.File]::ReadAllText('{}') -cne 'shared-scratch') {{exit 73}}; [IO.File]::WriteAllText('{}', 'child-scratch')",
        input.display().to_string().replace('\'', "''"),
        output.display().to_string().replace('\'', "''")
    );
    #[cfg(unix)]
    let script = format!(
        "test \"$(cat '{}')\" = shared-scratch && printf child-scratch > '{}'",
        input.display().to_string().replace('\'', "'\\''"),
        output.display().to_string().replace('\'', "'\\''")
    );
    let (status, out, err) = collect(launch(&policy, root.path(), script)).await;
    assert!(status.success(), "{status}; stdout={out}; stderr={err}");
    assert_eq!(std::fs::read_to_string(output).unwrap(), "child-scratch");
}

#[tokio::test]
async fn native_runtime_reads_match_the_common_policy_and_explicit_denials() {
    if !native_sandbox_available() {
        return;
    }
    let root = scoped_directory();
    #[cfg(windows)]
    let file = super::runtime_read_scopes().unwrap()[0]
        .path
        .join("win.ini");
    #[cfg(target_os = "linux")]
    let file = std::path::PathBuf::from("/etc/hosts");
    #[cfg(target_os = "macos")]
    let file = std::path::PathBuf::from("/private/etc/hosts");
    let expected = std::fs::metadata(&file).unwrap().len();
    assert!(expected > 0, "runtime fixture must contain observable data");
    let mut policy = policy(root.path());
    assert!(policy.is_path_readable(&file));
    assert!(!policy.is_path_writable(&file));
    #[cfg(windows)]
    let script = format!(
        "try {{ [Console]::WriteLine([IO.File]::ReadAllBytes('{}').Length) }} catch {{ [Console]::WriteLine(0) }}",
        file.display().to_string().replace('\'', "''")
    );
    #[cfg(unix)]
    let script = format!(
        "cat '{}' 2>/dev/null | wc -c",
        file.display().to_string().replace('\'', "'\\''")
    );
    let (status, out, err) = collect(launch(&policy, root.path(), script.clone())).await;
    assert!(status.success(), "runtime read failed: {err}");
    assert_eq!(out.trim().parse::<u64>().unwrap(), expected);
    policy.filesystem.deny_read_paths.push(file.clone());
    assert!(!policy.is_path_readable(&file));
    let (_, out, _) = collect(launch(&policy, root.path(), script)).await;
    assert_eq!(out.trim().parse::<u64>().unwrap(), 0);
}

#[tokio::test]
async fn native_writable_scope_allows_file_and_directory_rename_and_delete() {
    if !native_sandbox_available() {
        return;
    }
    let temp = scoped_directory();
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
    let temp = scoped_directory();
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
    // Observe host state even if a command's exit status is misleading.
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
    assert!(status.success(), "{status}; stdout={out}; stderr={err}");
    assert!(out.contains("protected-targets-complete"), "stdout={out}");
}

#[cfg(windows)]
#[tokio::test]
async fn native_powershell_provider_path_normalization_obeys_the_read_scope() {
    if !native_sandbox_available() {
        return;
    }
    for full_read in [false, true] {
        let directory = scoped_directory();
        let root = directory.path();
        std::fs::write(root.join("source.txt"), "original").unwrap();
        let mut policy = policy(root);
        policy.filesystem.full_disk_read = full_read;
        let script = script_with_root(
            root,
            r#"
$ErrorActionPreference = 'Stop'
$file = [IO.FileInfo]::new((Join-Path $root 'source.txt'))
$file.MoveTo((Join-Path $root 'moved.txt'))
try {
    Rename-Item -LiteralPath (Join-Path $root 'moved.txt') -NewName 'renamed.txt'
    Remove-Item -LiteralPath (Join-Path $root 'renamed.txt') -Force
} catch [UnauthorizedAccessException] { exit 71 }
"#,
        );
        let (status, out, err) = collect(launch(&policy, root, script)).await;
        if full_read {
            assert!(status.success(), "{status}; stdout={out}; stderr={err}");
            assert_eq!(std::fs::read_dir(root).unwrap().count(), 0);
        } else {
            // FileInfo moved the authorized file. PowerShell's provider also
            // normalizes absolute paths through ancestors outside that root;
            // allowing writes must not implicitly grant those reads.
            assert_eq!(status.code(), Some(71), "stdout={out}; stderr={err}");
            assert_eq!(
                std::fs::read_to_string(root.join("moved.txt")).unwrap(),
                "original"
            );
        }
    }
}

#[tokio::test]
async fn native_children_and_grandchildren_inherit_granted_and_denied_scopes() {
    if !native_sandbox_available() {
        return;
    }
    let temp = scoped_directory();
    let root = temp.path().join("work");
    let outside = temp.path().join("outside");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&outside).unwrap();
    #[cfg(windows)]
    let (name, body) = (
        "descendant.ps1",
        r#"
param([string]$root, [string]$outside, [int]$depth)
$ErrorActionPreference = 'Stop'
[IO.File]::WriteAllText((Join-Path $root "$depth.txt"), 'allowed')
$blocked = $false
try { [IO.File]::WriteAllText((Join-Path $outside "$depth.txt"), 'forbidden') }
catch [UnauthorizedAccessException] { $blocked = $true }
catch { if (($_.Exception.HResult -band 0xffff) -eq 5 -or ($_.Exception.InnerException.HResult -band 0xffff) -eq 5) { $blocked = $true } else { throw } }
if (-not $blocked) { throw 'descendant escaped write isolation' }
if ($depth -lt 2) {
    & (Join-Path $PSHOME 'powershell.exe') -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $PSCommandPath $root $outside ($depth + 1)
    if ($LASTEXITCODE -ne 0) { throw 'descendant failed' }
}
"#,
    );
    #[cfg(unix)]
    let (name, body) = (
        "descendant.sh",
        r#"
set -eu
root="$1"; outside="$2"; depth="$3"
printf allowed > "$root/$depth.txt"
if (printf forbidden > "$outside/$depth.txt") 2>/dev/null; then exit 41; fi
if [ "$depth" -lt 2 ]; then /bin/sh "$0" "$root" "$outside" "$((depth + 1))"; fi
"#,
    );
    std::fs::write(root.join(name), body).unwrap();
    #[cfg(windows)]
    let script = script_with_root(
        &root,
        &format!(
            "& (Join-Path $root '{name}') $root '{}' 0; if (-not $?) {{ exit 1 }}",
            outside.to_str().unwrap().replace('\'', "''")
        ),
    );
    #[cfg(unix)]
    let script = script_with_root(
        &root,
        &format!(
            "/bin/sh \"$root/{name}\" \"$root\" '{}' 0",
            outside.to_str().unwrap().replace('\'', "'\\''")
        ),
    );
    let (status, out, err) = collect(launch(&policy(&root), &root, script)).await;
    assert!(status.success(), "{status}; stdout={out}; stderr={err}");
    for depth in 0..=2 {
        assert_eq!(
            std::fs::read_to_string(root.join(format!("{depth}.txt"))).unwrap(),
            "allowed"
        );
        assert!(
            !outside.join(format!("{depth}.txt")).exists(),
            "descendant {depth} escaped sandbox"
        );
    }
}

#[tokio::test]
async fn live_process_scope_change_requires_explicit_restart_without_replaying_effects() {
    if !native_sandbox_available() {
        return;
    }
    let temp = scoped_directory();
    let root = temp.path();
    let original = policy(root);
    #[cfg(windows)]
    let body =
        "[IO.File]::WriteAllText((Join-Path $root 'once.txt'), 'once'); Start-Sleep -Seconds 20";
    #[cfg(unix)]
    let body = "printf once > \"$root/once.txt\"; sleep 20";
    let mut child = launch(&original, root, script_with_root(root, body));
    child.ensure_policy(&original).unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !root.join("once.txt").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("original effect never started");
    let mut changed = original.clone();
    changed
        .filesystem
        .readable_roots
        .push(temp.path().join("extra"));
    let error = child.ensure_policy(&changed).unwrap_err();
    assert!(error.to_string().contains("Explicitly restart"));
    assert_eq!(
        std::fs::read_to_string(root.join("once.txt")).unwrap(),
        "once"
    );
    assert!(!root.join("extra").exists());
    child.start_kill().unwrap();
    let _ = collect(child).await;
}

#[tokio::test]
async fn native_readonly_authority_overrides_full_access_and_temp_baseline() {
    if !native_sandbox_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let target = root.join("existing.txt");
    std::fs::write(&target, "original").unwrap();
    let mut policy = policy(root);
    policy.filesystem.full_disk_read = true;
    policy.filesystem.full_disk_write = true;
    policy.filesystem.deny_all_writes = true;
    #[cfg(windows)]
    let script = script_with_root(
        root,
        r#"
$ErrorActionPreference = 'Stop'
if ([IO.File]::ReadAllText((Join-Path $root 'existing.txt')) -ne 'original') { throw 'read access lost' }
foreach ($name in @('existing.txt', 'new.txt')) {
    $blocked = $false
    try { [IO.File]::WriteAllText((Join-Path $root $name), 'forbidden') }
    catch { if (($_.Exception.HResult -band 0xffff) -eq 5 -or ($_.Exception.InnerException.HResult -band 0xffff) -eq 5) { $blocked = $true } else { throw } }
    if (-not $blocked) { throw 'readonly write allowed' }
}
"#,
    );
    #[cfg(unix)]
    let script = script_with_root(
        root,
        r#"
set -eu
[ "$(cat "$root/existing.txt")" = original ]
for name in existing.txt new.txt; do
    if (printf forbidden > "$root/$name") 2>/dev/null; then exit 42; fi
done
"#,
    );
    let (status, out, err) = collect(launch(&policy, root, script)).await;
    assert_eq!(std::fs::read_to_string(target).unwrap(), "original");
    assert!(!root.join("new.txt").exists());
    assert!(status.success(), "{status}; stdout={out}; stderr={err}");
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
            "-ExecutionPolicy".into(),
            "Bypass".into(),
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
    [IO.File]::Move($path, $renamed)
    if (Test-Path -LiteralPath $path) { throw 'original file survived rename' }
    if ([IO.File]::ReadAllText($renamed) -ne 'initial+append') { throw 'rename lost content' }
    [Console]::WriteLine("delete file: $name")
    [IO.File]::Delete($renamed)
    if (Test-Path -LiteralPath $renamed) { throw 'file survived delete' }
}
$null = [IO.Directory]::CreateDirectory((Join-Path $root 'empty-directory'))
$null = [IO.Directory]::CreateDirectory((Join-Path $root 'sandbox-directory'))
[IO.File]::WriteAllText((Join-Path $root 'sandbox-directory/child.txt'), 'child')
foreach ($name in @('host-directory', 'host-empty-directory', 'empty-directory', 'sandbox-directory')) {
    $path = Join-Path $root $name
    $renamed = "$path.renamed"
    [Console]::WriteLine("rename directory: $name")
    [IO.Directory]::Move($path, $renamed)
    if (Test-Path -LiteralPath $path) { throw 'original directory survived rename' }
    if (-not [IO.Directory]::Exists($renamed)) { throw 'renamed directory missing' }
    if ($name -notin @('empty-directory', 'host-empty-directory')) {
        if ([IO.File]::ReadAllText((Join-Path $renamed 'child.txt')) -ne 'child') {
            throw 'directory rename lost content'
        }
    }
    [Console]::WriteLine("delete directory: $name")
    [IO.Directory]::Delete($renamed, $true)
    if (Test-Path -LiteralPath $renamed) { throw 'directory survived delete' }
}
[Console]::WriteLine('rename-delete-complete')
"#;

#[cfg(target_os = "windows")]
const PROTECTED_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
# Prove the same operations work on unprotected siblings in this process.
[IO.File]::WriteAllText((Join-Path $root 'control.txt'), 'control')
[IO.File]::Move((Join-Path $root 'control.txt'), (Join-Path $root 'control.txt.renamed'))
if ([IO.File]::ReadAllText((Join-Path $root 'control.txt.renamed')) -ne 'control') {
    throw 'control rename lost content'
}
[IO.File]::Delete((Join-Path $root 'control.txt.renamed'))
$null = [IO.Directory]::CreateDirectory((Join-Path $root 'control-directory'))
[IO.Directory]::Move((Join-Path $root 'control-directory'), (Join-Path $root 'control-directory.renamed'))
if (-not [IO.Directory]::Exists((Join-Path $root 'control-directory.renamed'))) {
    throw 'control directory rename failed'
}
[IO.Directory]::Delete((Join-Path $root 'control-directory.renamed'), $true)
foreach ($name in @('readonly.txt', 'denied.txt', 'readonly-directory', 'denied-directory')) {
    $path = Join-Path $root $name
    $renamed = "$path.renamed"
    foreach ($operation in @('rename', 'delete')) {
        [Console]::WriteLine("deny ${operation}: $name")
        $blocked = $false
        try {
            if ($operation -eq 'rename') {
                if ([IO.Directory]::Exists($path)) { [IO.Directory]::Move($path, $renamed) } else { [IO.File]::Move($path, $renamed) }
            } else {
                if ([IO.Directory]::Exists($path)) { [IO.Directory]::Delete($path, $true) } else { [IO.File]::Delete($path) }
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
rm "$root/control.txt.renamed"
mkdir "$root/control-directory"
mv "$root/control-directory" "$root/control-directory.renamed"
[ -d "$root/control-directory.renamed" ]
rm -r "$root/control-directory.renamed"
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
            # BSD rm -rf silently skips FTS_NS entries when stat is denied.
            # Without -f it attempts unlink or reports the traversal error;
            # only an actual permission diagnostic is accepted below.
            if failure=$(rm -r "$path" 2>&1); then
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
