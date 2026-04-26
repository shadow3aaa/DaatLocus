use std::{
    collections::BTreeMap,
    ffi::{OsStr, c_void},
    io,
    os::windows::{
        ffi::OsStrExt,
        io::{FromRawHandle, RawHandle},
        process::ExitStatusExt,
    },
    path::{Path, PathBuf},
    process::ExitStatus,
    ptr,
};

use uuid::Uuid;
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_SUCCESS, GENERIC_ALL, GetLastError, HANDLE,
        HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, LUID, LocalFree, SetHandleInformation,
        WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Security::Authorization::{
        ConvertStringSidToSidW, DENY_ACCESS, EXPLICIT_ACCESS_W, GetNamedSecurityInfoW,
        REVOKE_ACCESS, SET_ACCESS, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID,
        TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
    },
    Security::{
        AdjustTokenPrivileges, CONTAINER_INHERIT_ACE, CopySid, CreateRestrictedToken,
        CreateWellKnownSid, DACL_SECURITY_INFORMATION, DISABLE_MAX_PRIVILEGE, GetLengthSid,
        GetTokenInformation, LUA_TOKEN, LookupPrivilegeValueW, OBJECT_INHERIT_ACE, PSID,
        SID_AND_ATTRIBUTES, SetTokenInformation, TOKEN_ADJUST_DEFAULT, TOKEN_ADJUST_PRIVILEGES,
        TOKEN_ADJUST_SESSIONID, TOKEN_ASSIGN_PRIMARY, TOKEN_DEFAULT_DACL, TOKEN_DUPLICATE,
        TOKEN_PRIVILEGES, TOKEN_QUERY, TokenDefaultDacl, TokenGroups, WRITE_RESTRICTED,
    },
    Storage::FileSystem::{
        CreateFileW, DELETE, FILE_APPEND_DATA, FILE_ATTRIBUTE_NORMAL, FILE_DELETE_CHILD,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES,
        FILE_WRITE_DATA, FILE_WRITE_EA, OPEN_EXISTING,
    },
    System::{
        Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE},
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject,
        },
        Pipes::CreatePipe,
        SystemServices::SE_GROUP_LOGON_ID,
        Threading::{
            CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, GetCurrentProcess,
            GetExitCodeProcess, GetProcessId, INFINITE, OpenProcessToken, PROCESS_INFORMATION,
            STARTF_USESTDHANDLES, STARTUPINFOW, TerminateProcess, WaitForSingleObject,
        },
    },
};

use super::{
    RuntimeSandboxPolicy, SandboxProcessOptions, SandboxStdio, apply_std_command_options,
    policy_paths_with_resolved,
};

const WIN_WORLD_SID: i32 = 1;
const WORKER_READ_ALLOW_MASK: u32 = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE;
const WORKER_WRITE_ALLOW_MASK: u32 =
    FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE | FILE_DELETE_CHILD;
const WORKER_DENY_WRITE_MASK: u32 = FILE_GENERIC_WRITE
    | FILE_WRITE_DATA
    | FILE_APPEND_DATA
    | FILE_WRITE_EA
    | FILE_WRITE_ATTRIBUTES
    | DELETE
    | FILE_DELETE_CHILD;
const WORKER_DENY_READ_MASK: u32 = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE;

pub enum WindowsSandboxChild {
    Plain(std::process::Child),
    Restricted(RestrictedWindowsChild),
}

unsafe impl Send for WindowsSandboxChild {}
unsafe impl Sync for WindowsSandboxChild {}

pub struct WindowsSandboxAsyncChild {
    child: RestrictedWindowsChild,
    stdin: Option<tokio::fs::File>,
    stdout: Option<tokio::fs::File>,
    stderr: Option<tokio::fs::File>,
}

unsafe impl Send for WindowsSandboxAsyncChild {}
unsafe impl Sync for WindowsSandboxAsyncChild {}

pub struct RestrictedWindowsChild {
    process: HANDLE,
    thread: HANDLE,
    job: HANDLE,
    process_id: u32,
    exit_status: Option<ExitStatus>,
    acl_guards: Vec<PathBuf>,
    cap_sid: String,
    acl_cleaned: bool,
}

unsafe impl Send for RestrictedWindowsChild {}
unsafe impl Sync for RestrictedWindowsChild {}

struct LocalSid {
    ptr: PSID,
}

struct LocalMem<T>(*mut T);

struct OwnedHandle(HANDLE);

struct StartupHandles {
    stdin: HANDLE,
    stdout: HANDLE,
    stderr: HANDLE,
    _owned: Vec<OwnedHandle>,
}

#[derive(Default)]
struct ParentPipeHandles {
    stdin: Option<OwnedHandle>,
    stdout: Option<OwnedHandle>,
    stderr: Option<OwnedHandle>,
}

impl WindowsSandboxChild {
    pub fn id(&self) -> u32 {
        match self {
            Self::Plain(child) => child.id(),
            Self::Restricted(child) => child.id(),
        }
    }

    pub fn kill(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(child) => child.kill(),
            Self::Restricted(child) => child.kill(),
        }
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        match self {
            Self::Plain(child) => child.try_wait(),
            Self::Restricted(child) => child.try_wait(),
        }
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        match self {
            Self::Plain(child) => child.wait(),
            Self::Restricted(child) => child.wait(),
        }
    }
}

impl WindowsSandboxAsyncChild {
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn take_stdin(&mut self) -> Option<tokio::fs::File> {
        self.stdin.take()
    }

    pub fn take_stdout(&mut self) -> Option<tokio::fs::File> {
        self.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<tokio::fs::File> {
        self.stderr.take()
    }

    pub fn start_kill(&mut self) -> io::Result<()> {
        self.child.kill()
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }
}

pub fn spawn_plain(
    policy: &RuntimeSandboxPolicy,
    program: PathBuf,
    args: Vec<String>,
    options: SandboxProcessOptions,
) -> io::Result<WindowsSandboxChild> {
    let mut command = std::process::Command::new(program);
    command.args(args);
    apply_std_command_options(policy, &mut command, options);
    command.spawn().map(WindowsSandboxChild::Plain)
}

pub fn spawn_restricted(
    policy: &RuntimeSandboxPolicy,
    program: PathBuf,
    args: Vec<String>,
    options: SandboxProcessOptions,
) -> io::Result<WindowsSandboxChild> {
    let cap_sid = random_capability_sid();
    let cap_sid_ptr = LocalSid::from_string(&cap_sid)?;
    let token = create_restricted_token(&cap_sid_ptr)?;
    let mut acl_guards = Vec::new();
    let process = match spawn_restricted_inner(
        policy,
        &program,
        args,
        options,
        &cap_sid,
        cap_sid_ptr.as_ptr(),
        token.raw(),
        &mut acl_guards,
    ) {
        Ok(process) => process,
        Err(err) => {
            revoke_acl_guards(&acl_guards, &cap_sid);
            return Err(err);
        }
    };
    drop(token);
    Ok(WindowsSandboxChild::Restricted(process))
}

pub fn spawn_restricted_async(
    policy: &RuntimeSandboxPolicy,
    program: PathBuf,
    args: Vec<String>,
    options: SandboxProcessOptions,
) -> io::Result<WindowsSandboxAsyncChild> {
    let cap_sid = random_capability_sid();
    let cap_sid_ptr = LocalSid::from_string(&cap_sid)?;
    let token = create_restricted_token(&cap_sid_ptr)?;
    let mut acl_guards = Vec::new();
    let process = match spawn_restricted_async_inner(
        policy,
        &program,
        args,
        options,
        &cap_sid,
        cap_sid_ptr.as_ptr(),
        token.raw(),
        &mut acl_guards,
    ) {
        Ok(process) => process,
        Err(err) => {
            revoke_acl_guards(&acl_guards, &cap_sid);
            return Err(err);
        }
    };
    drop(token);
    Ok(process)
}

#[allow(clippy::too_many_arguments)]
fn spawn_restricted_inner(
    policy: &RuntimeSandboxPolicy,
    program: &Path,
    args: Vec<String>,
    options: SandboxProcessOptions,
    cap_sid: &str,
    psid_capability: PSID,
    token: HANDLE,
    acl_guards: &mut Vec<PathBuf>,
) -> io::Result<RestrictedWindowsChild> {
    apply_policy_acl_rules(policy, program, psid_capability, acl_guards)?;
    let current_dir = options
        .current_dir
        .clone()
        .unwrap_or(std::env::current_dir()?);
    let argv = command_argv(program, args);
    let mut command_line = to_wide(argv_to_command_line(&argv));
    let program_wide = to_wide(program.as_os_str());
    let current_dir_wide = to_wide(current_dir.as_os_str());
    let env_block = environment_block(policy);
    let stdio = StartupHandles::new(options)?;
    let startup_info = startup_info(&stdio);
    let mut process_info = PROCESS_INFORMATION::default();

    let created = unsafe {
        CreateProcessAsUserW(
            token,
            program_wide.as_ptr(),
            command_line.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            1,
            CREATE_UNICODE_ENVIRONMENT,
            env_block.as_ptr().cast::<c_void>(),
            current_dir_wide.as_ptr(),
            &startup_info,
            &mut process_info,
        )
    };
    if created == 0 {
        return Err(last_os_error("CreateProcessAsUserW failed"));
    }

    let job = match create_kill_on_close_job(process_info.hProcess) {
        Ok(job) => job,
        Err(err) => {
            unsafe {
                TerminateProcess(process_info.hProcess, 1);
                CloseHandle(process_info.hThread);
                CloseHandle(process_info.hProcess);
            }
            return Err(err);
        }
    };

    Ok(RestrictedWindowsChild {
        process: process_info.hProcess,
        thread: process_info.hThread,
        job,
        process_id: unsafe { GetProcessId(process_info.hProcess) },
        exit_status: None,
        acl_guards: std::mem::take(acl_guards),
        cap_sid: cap_sid.to_string(),
        acl_cleaned: false,
    })
}

#[allow(clippy::too_many_arguments)]
fn spawn_restricted_async_inner(
    policy: &RuntimeSandboxPolicy,
    program: &Path,
    args: Vec<String>,
    options: SandboxProcessOptions,
    cap_sid: &str,
    psid_capability: PSID,
    token: HANDLE,
    acl_guards: &mut Vec<PathBuf>,
) -> io::Result<WindowsSandboxAsyncChild> {
    apply_policy_acl_rules(policy, program, psid_capability, acl_guards)?;
    let current_dir = options
        .current_dir
        .clone()
        .unwrap_or(std::env::current_dir()?);
    let argv = command_argv(program, args);
    let mut command_line = to_wide(argv_to_command_line(&argv));
    let program_wide = to_wide(program.as_os_str());
    let current_dir_wide = to_wide(current_dir.as_os_str());
    let env_block = environment_block(policy);
    let (stdio, parent_pipes) = StartupHandles::new_with_parent_pipes(options)?;
    let startup_info = startup_info(&stdio);
    let mut process_info = PROCESS_INFORMATION::default();

    let created = unsafe {
        CreateProcessAsUserW(
            token,
            program_wide.as_ptr(),
            command_line.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            1,
            CREATE_UNICODE_ENVIRONMENT,
            env_block.as_ptr().cast::<c_void>(),
            current_dir_wide.as_ptr(),
            &startup_info,
            &mut process_info,
        )
    };
    if created == 0 {
        return Err(last_os_error("CreateProcessAsUserW failed"));
    }

    let job = match create_kill_on_close_job(process_info.hProcess) {
        Ok(job) => job,
        Err(err) => {
            unsafe {
                TerminateProcess(process_info.hProcess, 1);
                CloseHandle(process_info.hThread);
                CloseHandle(process_info.hProcess);
            }
            return Err(err);
        }
    };

    let child = RestrictedWindowsChild {
        process: process_info.hProcess,
        thread: process_info.hThread,
        job,
        process_id: unsafe { GetProcessId(process_info.hProcess) },
        exit_status: None,
        acl_guards: std::mem::take(acl_guards),
        cap_sid: cap_sid.to_string(),
        acl_cleaned: false,
    };
    let (stdin, stdout, stderr) = parent_pipes.into_async_files()?;

    Ok(WindowsSandboxAsyncChild {
        child,
        stdin,
        stdout,
        stderr,
    })
}

fn apply_policy_acl_rules(
    policy: &RuntimeSandboxPolicy,
    program: &Path,
    psid: PSID,
    acl_guards: &mut Vec<PathBuf>,
) -> io::Result<()> {
    if let Some(parent) = program.parent() {
        add_program_read_ace(parent, psid, acl_guards)?;
    }
    add_program_read_ace(program, psid, acl_guards)?;

    for root in policy_paths_with_resolved(&policy.filesystem.readable_roots) {
        add_guarded_ace(&root, psid, WORKER_READ_ALLOW_MASK, SET_ACCESS, acl_guards)?;
    }
    for writable_root in &policy.filesystem.writable_roots {
        for root in policy_paths_with_resolved(std::slice::from_ref(&writable_root.root)) {
            add_guarded_ace(&root, psid, WORKER_WRITE_ALLOW_MASK, SET_ACCESS, acl_guards)?;
        }
        for subpath in policy_paths_with_resolved(&writable_root.read_only_subpaths) {
            add_guarded_ace_recursive(
                &subpath,
                psid,
                WORKER_DENY_WRITE_MASK,
                DENY_ACCESS,
                acl_guards,
            )?;
        }
    }
    for path in policy_paths_with_resolved(&policy.filesystem.deny_write_paths) {
        add_guarded_ace_recursive(&path, psid, WORKER_DENY_WRITE_MASK, DENY_ACCESS, acl_guards)?;
    }
    for path in policy_paths_with_resolved(&policy.filesystem.deny_read_paths) {
        add_guarded_ace_recursive(&path, psid, WORKER_DENY_READ_MASK, DENY_ACCESS, acl_guards)?;
    }
    Ok(())
}

fn add_program_read_ace(path: &Path, psid: PSID, acl_guards: &mut Vec<PathBuf>) -> io::Result<()> {
    match add_guarded_ace(path, psid, WORKER_READ_ALLOW_MASK, SET_ACCESS, acl_guards) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
            tracing::warn!(
                "Windows sandbox could not add read ACE for executable path {}; relying on existing ACLs: {err}",
                path.display()
            );
            Ok(())
        }
        Err(err) => Err(err),
    }
}

fn add_guarded_ace(
    path: &Path,
    psid: PSID,
    mask: u32,
    mode: i32,
    acl_guards: &mut Vec<PathBuf>,
) -> io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    add_explicit_ace(path, psid, mask, mode)?;
    acl_guards.push(path.to_path_buf());
    Ok(())
}

fn add_guarded_ace_recursive(
    path: &Path,
    psid: PSID,
    mask: u32,
    mode: i32,
    acl_guards: &mut Vec<PathBuf>,
) -> io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    add_guarded_ace(path, psid, mask, mode, acl_guards)?;
    if !path.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let child = entry.path();
        if entry.file_type()?.is_dir() {
            add_guarded_ace_recursive(&child, psid, mask, mode, acl_guards)?;
        } else {
            add_guarded_ace(&child, psid, mask, mode, acl_guards)?;
        }
    }
    Ok(())
}

fn add_explicit_ace(path: &Path, psid: PSID, mask: u32, mode: i32) -> io::Result<()> {
    let path_wide = to_wide(path.as_os_str());
    let mut security_descriptor: PSID = ptr::null_mut();
    let mut dacl = ptr::null_mut();
    let code = unsafe {
        GetNamedSecurityInfoW(
            path_wide.as_ptr(),
            1,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut security_descriptor,
        )
    };
    if code != ERROR_SUCCESS {
        return Err(win32_error(
            code,
            format!("GetNamedSecurityInfoW failed for {}", path.display()),
        ));
    }
    let _security_descriptor = LocalMem(security_descriptor);
    let explicit = explicit_access(psid, mask, mode, inheritance_for_path(path));
    let mut new_dacl = ptr::null_mut();
    let code = unsafe { SetEntriesInAclW(1, &explicit, dacl, &mut new_dacl) };
    if code != ERROR_SUCCESS {
        return Err(win32_error(
            code,
            format!("SetEntriesInAclW failed for {}", path.display()),
        ));
    }
    let _new_dacl = LocalMem(new_dacl);
    let code = unsafe {
        SetNamedSecurityInfoW(
            path_wide.as_ptr() as *mut u16,
            1,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            new_dacl,
            ptr::null_mut(),
        )
    };
    if code != ERROR_SUCCESS {
        return Err(win32_error(
            code,
            format!("SetNamedSecurityInfoW failed for {}", path.display()),
        ));
    }
    Ok(())
}

fn revoke_acl_guards(paths: &[PathBuf], cap_sid: &str) {
    let Ok(sid) = LocalSid::from_string(cap_sid) else {
        return;
    };
    for path in paths {
        let _ = revoke_ace(path, sid.as_ptr());
    }
}

fn revoke_ace(path: &Path, psid: PSID) -> io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let path_wide = to_wide(path.as_os_str());
    let mut security_descriptor: PSID = ptr::null_mut();
    let mut dacl = ptr::null_mut();
    let code = unsafe {
        GetNamedSecurityInfoW(
            path_wide.as_ptr(),
            1,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut security_descriptor,
        )
    };
    if code != ERROR_SUCCESS {
        return Err(win32_error(
            code,
            format!("GetNamedSecurityInfoW failed for {}", path.display()),
        ));
    }
    let _security_descriptor = LocalMem(security_descriptor);
    let explicit = explicit_access(psid, 0, REVOKE_ACCESS, inheritance_for_path(path));
    let mut new_dacl = ptr::null_mut();
    let code = unsafe { SetEntriesInAclW(1, &explicit, dacl, &mut new_dacl) };
    if code != ERROR_SUCCESS {
        return Err(win32_error(
            code,
            format!("SetEntriesInAclW failed for {}", path.display()),
        ));
    }
    let _new_dacl = LocalMem(new_dacl);
    let code = unsafe {
        SetNamedSecurityInfoW(
            path_wide.as_ptr() as *mut u16,
            1,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            new_dacl,
            ptr::null_mut(),
        )
    };
    if code != ERROR_SUCCESS {
        return Err(win32_error(
            code,
            format!("SetNamedSecurityInfoW failed for {}", path.display()),
        ));
    }
    Ok(())
}

fn explicit_access(psid: PSID, mask: u32, mode: i32, inheritance: u32) -> EXPLICIT_ACCESS_W {
    EXPLICIT_ACCESS_W {
        grfAccessPermissions: mask,
        grfAccessMode: mode,
        grfInheritance: inheritance,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: ptr::null_mut(),
            MultipleTrusteeOperation: 0,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: psid.cast::<u16>(),
        },
    }
}

fn inheritance_for_path(path: &Path) -> u32 {
    if path.is_dir() {
        CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE
    } else {
        0
    }
}

fn create_restricted_token(psid_capability: &LocalSid) -> io::Result<OwnedHandle> {
    let base = current_token_for_restriction()?;
    let mut logon_sid = logon_sid_bytes(base.raw())?;
    let psid_logon = logon_sid.as_mut_ptr().cast::<c_void>();
    let mut everyone_sid = world_sid_bytes()?;
    let psid_everyone = everyone_sid.as_mut_ptr().cast::<c_void>();
    let mut restricted_sids = vec![
        SID_AND_ATTRIBUTES {
            Sid: psid_capability.as_ptr(),
            Attributes: 0,
        },
        SID_AND_ATTRIBUTES {
            Sid: psid_logon,
            Attributes: 0,
        },
        SID_AND_ATTRIBUTES {
            Sid: psid_everyone,
            Attributes: 0,
        },
    ];
    let mut token = ptr::null_mut();
    let ok = unsafe {
        CreateRestrictedToken(
            base.raw(),
            DISABLE_MAX_PRIVILEGE | LUA_TOKEN | WRITE_RESTRICTED,
            0,
            ptr::null(),
            0,
            ptr::null(),
            restricted_sids.len() as u32,
            restricted_sids.as_mut_ptr(),
            &mut token,
        )
    };
    if ok == 0 {
        return Err(last_os_error("CreateRestrictedToken failed"));
    }
    let token = OwnedHandle(token);
    set_default_dacl(
        token.raw(),
        &[psid_capability.as_ptr(), psid_logon, psid_everyone],
    )?;
    enable_single_privilege(token.raw(), "SeChangeNotifyPrivilege")?;
    Ok(token)
}

fn current_token_for_restriction() -> io::Result<OwnedHandle> {
    let desired = TOKEN_DUPLICATE
        | TOKEN_QUERY
        | TOKEN_ASSIGN_PRIMARY
        | TOKEN_ADJUST_DEFAULT
        | TOKEN_ADJUST_SESSIONID
        | TOKEN_ADJUST_PRIVILEGES;
    let mut token = ptr::null_mut();
    let ok = unsafe { OpenProcessToken(GetCurrentProcess(), desired, &mut token) };
    if ok == 0 {
        Err(last_os_error("OpenProcessToken failed"))
    } else {
        Ok(OwnedHandle(token))
    }
}

fn logon_sid_bytes(token: HANDLE) -> io::Result<Vec<u8>> {
    if let Some(sid) = scan_token_groups_for_logon(token) {
        return Ok(sid);
    }
    Err(io::Error::other("logon SID not present on token"))
}

fn scan_token_groups_for_logon(token: HANDLE) -> Option<Vec<u8>> {
    let mut needed = 0;
    unsafe {
        GetTokenInformation(token, TokenGroups, ptr::null_mut(), 0, &mut needed);
    }
    if needed == 0 {
        return None;
    }
    let mut buffer = vec![0u8; needed as usize];
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenGroups,
            buffer.as_mut_ptr().cast::<c_void>(),
            needed,
            &mut needed,
        )
    };
    if ok == 0 || needed as usize <= std::mem::size_of::<u32>() {
        return None;
    }
    let group_count = unsafe { ptr::read_unaligned(buffer.as_ptr().cast::<u32>()) as usize };
    let after_count = unsafe { buffer.as_ptr().add(std::mem::size_of::<u32>()) } as usize;
    let align = std::mem::align_of::<SID_AND_ATTRIBUTES>();
    let aligned = (after_count + (align - 1)) & !(align - 1);
    let groups = aligned as *const SID_AND_ATTRIBUTES;
    for index in 0..group_count {
        let entry = unsafe { ptr::read_unaligned(groups.add(index)) };
        if (entry.Attributes & SE_GROUP_LOGON_ID as u32) == SE_GROUP_LOGON_ID as u32 {
            let sid_len = unsafe { GetLengthSid(entry.Sid) };
            if sid_len == 0 {
                return None;
            }
            let mut out = vec![0u8; sid_len as usize];
            let ok = unsafe { CopySid(sid_len, out.as_mut_ptr().cast::<c_void>(), entry.Sid) };
            if ok == 0 {
                return None;
            }
            return Some(out);
        }
    }
    None
}

fn world_sid_bytes() -> io::Result<Vec<u8>> {
    let mut size = 0;
    unsafe {
        CreateWellKnownSid(WIN_WORLD_SID, ptr::null_mut(), ptr::null_mut(), &mut size);
    }
    if size == 0 {
        return Err(last_os_error("CreateWellKnownSid size query failed"));
    }
    let mut buffer = vec![0u8; size as usize];
    let ok = unsafe {
        CreateWellKnownSid(
            WIN_WORLD_SID,
            ptr::null_mut(),
            buffer.as_mut_ptr().cast::<c_void>(),
            &mut size,
        )
    };
    if ok == 0 {
        Err(last_os_error("CreateWellKnownSid failed"))
    } else {
        Ok(buffer)
    }
}

fn set_default_dacl(token: HANDLE, sids: &[PSID]) -> io::Result<()> {
    let entries = sids
        .iter()
        .map(|sid| explicit_access(*sid, GENERIC_ALL, SET_ACCESS, 0))
        .collect::<Vec<_>>();
    let mut new_dacl = ptr::null_mut();
    let code = unsafe {
        SetEntriesInAclW(
            entries.len() as u32,
            entries.as_ptr(),
            ptr::null(),
            &mut new_dacl,
        )
    };
    if code != ERROR_SUCCESS {
        return Err(win32_error(
            code,
            "SetEntriesInAclW failed for token default DACL",
        ));
    }
    let _new_dacl = LocalMem(new_dacl);
    let info = TOKEN_DEFAULT_DACL {
        DefaultDacl: new_dacl,
    };
    let ok = unsafe {
        SetTokenInformation(
            token,
            TokenDefaultDacl,
            (&info as *const TOKEN_DEFAULT_DACL).cast::<c_void>(),
            std::mem::size_of::<TOKEN_DEFAULT_DACL>() as u32,
        )
    };
    if ok == 0 {
        Err(last_os_error(
            "SetTokenInformation(TokenDefaultDacl) failed",
        ))
    } else {
        Ok(())
    }
}

fn enable_single_privilege(token: HANDLE, name: &str) -> io::Result<()> {
    let mut luid = LUID {
        LowPart: 0,
        HighPart: 0,
    };
    let name_wide = to_wide(name);
    let ok = unsafe { LookupPrivilegeValueW(ptr::null(), name_wide.as_ptr(), &mut luid) };
    if ok == 0 {
        return Err(last_os_error("LookupPrivilegeValueW failed"));
    }
    let privileges = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [windows_sys::Win32::Security::LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: 0x0000_0002,
        }],
    };
    let ok = unsafe {
        AdjustTokenPrivileges(token, 0, &privileges, 0, ptr::null_mut(), ptr::null_mut())
    };
    if ok == 0 {
        Err(last_os_error("AdjustTokenPrivileges failed"))
    } else {
        Ok(())
    }
}

fn create_kill_on_close_job(process: HANDLE) -> io::Result<HANDLE> {
    let job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
    if is_invalid_handle(job) {
        return Err(last_os_error("CreateJobObjectW failed"));
    }
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let ok = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast::<c_void>(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if ok == 0 {
        unsafe {
            CloseHandle(job);
        }
        return Err(last_os_error("SetInformationJobObject failed"));
    }
    let ok = unsafe { AssignProcessToJobObject(job, process) };
    if ok == 0 {
        unsafe {
            CloseHandle(job);
        }
        return Err(last_os_error("AssignProcessToJobObject failed"));
    }
    Ok(job)
}

impl RestrictedWindowsChild {
    fn id(&self) -> u32 {
        self.process_id
    }

    fn kill(&mut self) -> io::Result<()> {
        if !is_invalid_handle(self.job) {
            let ok = unsafe { TerminateJobObject(self.job, 1) };
            if ok != 0 {
                return Ok(());
            }
        }
        let ok = unsafe { TerminateProcess(self.process, 1) };
        if ok == 0 {
            Err(last_os_error("TerminateProcess failed"))
        } else {
            Ok(())
        }
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.exit_status {
            return Ok(Some(status));
        }
        match unsafe { WaitForSingleObject(self.process, 0) } {
            WAIT_TIMEOUT => Ok(None),
            WAIT_OBJECT_0 => {
                let status = self.finish_after_exit()?;
                Ok(Some(status))
            }
            WAIT_FAILED => Err(last_os_error("WaitForSingleObject failed")),
            other => Err(io::Error::other(format!("unexpected wait result {other}"))),
        }
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.exit_status {
            return Ok(status);
        }
        match unsafe { WaitForSingleObject(self.process, INFINITE) } {
            WAIT_OBJECT_0 => self.finish_after_exit(),
            WAIT_FAILED => Err(last_os_error("WaitForSingleObject failed")),
            other => Err(io::Error::other(format!("unexpected wait result {other}"))),
        }
    }

    fn finish_after_exit(&mut self) -> io::Result<ExitStatus> {
        let mut code = 1;
        let ok = unsafe { GetExitCodeProcess(self.process, &mut code) };
        if ok == 0 {
            return Err(last_os_error("GetExitCodeProcess failed"));
        }
        self.close_thread_handle();
        self.cleanup_acl_guards();
        let status = ExitStatus::from_raw(code);
        self.exit_status = Some(status);
        Ok(status)
    }

    fn close_thread_handle(&mut self) {
        if !is_invalid_handle(self.thread) {
            unsafe {
                CloseHandle(self.thread);
            }
            self.thread = ptr::null_mut();
        }
    }

    fn cleanup_acl_guards(&mut self) {
        if self.acl_cleaned {
            return;
        }
        revoke_acl_guards(&self.acl_guards, &self.cap_sid);
        self.acl_cleaned = true;
    }
}

impl Drop for RestrictedWindowsChild {
    fn drop(&mut self) {
        self.close_thread_handle();
        if self.exit_status.is_none() && !is_invalid_handle(self.job) {
            unsafe {
                CloseHandle(self.job);
            }
            self.job = ptr::null_mut();
            let _ = unsafe { WaitForSingleObject(self.process, 5000) };
        }
        self.cleanup_acl_guards();
        if !is_invalid_handle(self.process) {
            unsafe {
                CloseHandle(self.process);
            }
            self.process = ptr::null_mut();
        }
        if !is_invalid_handle(self.job) {
            unsafe {
                CloseHandle(self.job);
            }
            self.job = ptr::null_mut();
        }
    }
}

impl LocalSid {
    fn from_string(sid: &str) -> io::Result<Self> {
        let wide = to_wide(sid);
        let mut ptr = ptr::null_mut();
        let ok = unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut ptr) };
        if ok == 0 {
            Err(last_os_error("ConvertStringSidToSidW failed"))
        } else {
            Ok(Self { ptr })
        }
    }

    fn as_ptr(&self) -> PSID {
        self.ptr
    }
}

impl Drop for LocalSid {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                LocalFree(self.ptr.cast::<c_void>());
            }
            self.ptr = ptr::null_mut();
        }
    }
}

impl<T> Drop for LocalMem<T> {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                LocalFree(self.0.cast::<c_void>());
            }
            self.0 = ptr::null_mut();
        }
    }
}

impl OwnedHandle {
    fn raw(&self) -> HANDLE {
        self.0
    }

    fn into_raw(mut self) -> HANDLE {
        let handle = self.0;
        self.0 = ptr::null_mut();
        handle
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !is_invalid_handle(self.0) {
            unsafe {
                CloseHandle(self.0);
            }
            self.0 = ptr::null_mut();
        }
    }
}

impl StartupHandles {
    fn new(options: SandboxProcessOptions) -> io::Result<Self> {
        let mut owned = Vec::new();
        let stdin = stdio_handle(options.stdin, STD_INPUT_HANDLE, true, &mut owned)?;
        let stdout = stdio_handle(options.stdout, STD_OUTPUT_HANDLE, false, &mut owned)?;
        let stderr = stdio_handle(options.stderr, STD_ERROR_HANDLE, false, &mut owned)?;
        Ok(Self {
            stdin,
            stdout,
            stderr,
            _owned: owned,
        })
    }

    fn new_with_parent_pipes(
        options: SandboxProcessOptions,
    ) -> io::Result<(Self, ParentPipeHandles)> {
        let mut owned = Vec::new();
        let mut parent = ParentPipeHandles::default();
        let stdin = match options.stdin {
            SandboxStdio::Piped => {
                let (child_read, parent_write) = create_pipe_pair("stdin")?;
                set_handle_inherit(child_read.raw(), true)?;
                set_handle_inherit(parent_write.raw(), false)?;
                let child_handle = child_read.raw();
                owned.push(child_read);
                parent.stdin = Some(parent_write);
                child_handle
            }
            mode => stdio_handle(mode, STD_INPUT_HANDLE, true, &mut owned)?,
        };
        let stdout = match options.stdout {
            SandboxStdio::Piped => {
                let (parent_read, child_write) = create_pipe_pair("stdout")?;
                set_handle_inherit(child_write.raw(), true)?;
                set_handle_inherit(parent_read.raw(), false)?;
                let child_handle = child_write.raw();
                owned.push(child_write);
                parent.stdout = Some(parent_read);
                child_handle
            }
            mode => stdio_handle(mode, STD_OUTPUT_HANDLE, false, &mut owned)?,
        };
        let stderr = match options.stderr {
            SandboxStdio::Piped => {
                let (parent_read, child_write) = create_pipe_pair("stderr")?;
                set_handle_inherit(child_write.raw(), true)?;
                set_handle_inherit(parent_read.raw(), false)?;
                let child_handle = child_write.raw();
                owned.push(child_write);
                parent.stderr = Some(parent_read);
                child_handle
            }
            mode => stdio_handle(mode, STD_ERROR_HANDLE, false, &mut owned)?,
        };
        Ok((
            Self {
                stdin,
                stdout,
                stderr,
                _owned: owned,
            },
            parent,
        ))
    }
}

impl ParentPipeHandles {
    fn into_async_files(
        self,
    ) -> io::Result<(
        Option<tokio::fs::File>,
        Option<tokio::fs::File>,
        Option<tokio::fs::File>,
    )> {
        Ok((
            owned_handle_to_async_file(self.stdin),
            owned_handle_to_async_file(self.stdout),
            owned_handle_to_async_file(self.stderr),
        ))
    }
}

fn owned_handle_to_async_file(handle: Option<OwnedHandle>) -> Option<tokio::fs::File> {
    let handle = handle?;
    let raw = handle.into_raw();
    Some(unsafe { tokio::fs::File::from_raw_handle(raw as RawHandle) })
}

fn startup_info(handles: &StartupHandles) -> STARTUPINFOW {
    STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        dwFlags: STARTF_USESTDHANDLES,
        hStdInput: handles.stdin,
        hStdOutput: handles.stdout,
        hStdError: handles.stderr,
        ..Default::default()
    }
}

fn stdio_handle(
    mode: SandboxStdio,
    standard_handle: u32,
    read_access: bool,
    owned: &mut Vec<OwnedHandle>,
) -> io::Result<HANDLE> {
    let handle = match mode {
        SandboxStdio::Inherit => {
            let inherited = unsafe { GetStdHandle(standard_handle) };
            if is_invalid_handle(inherited) {
                open_nul(read_access, owned)?
            } else {
                inherited
            }
        }
        SandboxStdio::Null => open_nul(read_access, owned)?,
        SandboxStdio::Piped => {
            return Err(io::Error::other(
                "Windows restricted sandbox does not support piped stdio yet",
            ));
        }
    };
    set_handle_inherit(handle, true)?;
    Ok(handle)
}

fn create_pipe_pair(label: &str) -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut read: HANDLE = ptr::null_mut();
    let mut write: HANDLE = ptr::null_mut();
    let ok = unsafe { CreatePipe(&mut read, &mut write, ptr::null_mut(), 0) };
    if ok == 0 {
        return Err(last_os_error(&format!("CreatePipe({label}) failed")));
    }
    Ok((OwnedHandle(read), OwnedHandle(write)))
}

fn set_handle_inherit(handle: HANDLE, inherit: bool) -> io::Result<()> {
    let flags = if inherit { HANDLE_FLAG_INHERIT } else { 0 };
    let ok = unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, flags) };
    if ok == 0 {
        Err(last_os_error("SetHandleInformation failed"))
    } else {
        Ok(())
    }
}

fn open_nul(read_access: bool, owned: &mut Vec<OwnedHandle>) -> io::Result<HANDLE> {
    let name = to_wide("NUL");
    let access = if read_access {
        FILE_GENERIC_READ
    } else {
        FILE_GENERIC_WRITE
    };
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_BACKUP_SEMANTICS,
            ptr::null_mut(),
        )
    };
    if is_invalid_handle(handle) {
        Err(last_os_error("CreateFileW(NUL) failed"))
    } else {
        owned.push(OwnedHandle(handle));
        Ok(handle)
    }
}

fn command_argv(program: &Path, args: Vec<String>) -> Vec<String> {
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(program.to_string_lossy().into_owned());
    argv.extend(args);
    argv
}

fn environment_block(policy: &RuntimeSandboxPolicy) -> Vec<u16> {
    let mut env = BTreeMap::<String, String>::new();
    for (name, value) in std::env::vars_os() {
        let name = name.to_string_lossy().into_owned();
        if policy.is_env_var_protected(&name) {
            continue;
        }
        env.insert(name, value.to_string_lossy().into_owned());
    }

    let mut block = Vec::new();
    for (name, value) in env {
        let mut item = to_wide(format!("{name}={value}"));
        item.pop();
        block.extend(item);
        block.push(0);
    }
    block.push(0);
    block
}

fn random_capability_sid() -> String {
    let uuid = Uuid::new_v4();
    let bytes = uuid.as_bytes();
    let a = u32::from_le_bytes(bytes[0..4].try_into().expect("uuid field"));
    let b = u32::from_le_bytes(bytes[4..8].try_into().expect("uuid field"));
    let c = u32::from_le_bytes(bytes[8..12].try_into().expect("uuid field"));
    let d = u32::from_le_bytes(bytes[12..16].try_into().expect("uuid field"));
    format!("S-1-5-21-{a}-{b}-{c}-{d}")
}

fn argv_to_command_line(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| quote_windows_arg(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_windows_arg(arg: &str) -> String {
    let needs_quotes = arg.is_empty()
        || arg
            .chars()
            .any(|ch| matches!(ch, ' ' | '\t' | '\n' | '\r' | '"'));
    if !needs_quotes {
        return arg.to_string();
    }

    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('"');
    let mut backslashes = 0;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                if backslashes > 0 {
                    quoted.push_str(&"\\".repeat(backslashes));
                    backslashes = 0;
                }
                quoted.push(ch);
            }
        }
    }
    if backslashes > 0 {
        quoted.push_str(&"\\".repeat(backslashes * 2));
    }
    quoted.push('"');
    quoted
}

fn to_wide<S: AsRef<OsStr>>(value: S) -> Vec<u16> {
    let mut wide = value.as_ref().encode_wide().collect::<Vec<_>>();
    wide.push(0);
    wide
}

fn is_invalid_handle(handle: HANDLE) -> bool {
    handle.is_null() || handle == INVALID_HANDLE_VALUE
}

fn last_os_error(context: &str) -> io::Error {
    let err = unsafe { GetLastError() };
    io::Error::other(format!(
        "{context}: {}",
        io::Error::from_raw_os_error(err as i32)
    ))
}

fn win32_error(code: u32, context: impl Into<String>) -> io::Error {
    let message = format!(
        "{}: {}",
        context.into(),
        io::Error::from_raw_os_error(code as i32)
    );
    if code == ERROR_ACCESS_DENIED {
        io::Error::new(io::ErrorKind::PermissionDenied, message)
    } else {
        io::Error::other(message)
    }
}
