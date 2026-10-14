use std::{
    collections::{BTreeMap, HashSet},
    ffi::{OsStr, OsString, c_void},
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
static ACL_UPDATES: std::sync::Mutex<()> = std::sync::Mutex::new(());
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_SUCCESS, GENERIC_ALL, GetLastError, HANDLE,
        HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, LUID, LocalFree, SetHandleInformation,
        WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSidToSidW, DENY_ACCESS, EXPLICIT_ACCESS_W,
        GetNamedSecurityInfoW, SET_ACCESS, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID,
        TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
    },
    Security::Isolation::{CreateAppContainerProfile, DeleteAppContainerProfile},
    Security::{
        ACL, ACL_SIZE_INFORMATION, AclSizeInformation, AddAce, AdjustTokenPrivileges,
        CONTAINER_INHERIT_ACE, CopySid, CreateRestrictedToken, DACL_SECURITY_INFORMATION,
        DISABLE_MAX_PRIVILEGE, FreeSid, GetAce, GetAclInformation, GetLengthSid,
        GetTokenInformation, InitializeAcl, LUA_TOKEN, LookupPrivilegeValueW, OBJECT_INHERIT_ACE,
        PROTECTED_DACL_SECURITY_INFORMATION, PSID, SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES,
        SetTokenInformation, TOKEN_ADJUST_DEFAULT, TOKEN_ADJUST_PRIVILEGES, TOKEN_ADJUST_SESSIONID,
        TOKEN_ASSIGN_PRIMARY, TOKEN_DEFAULT_DACL, TOKEN_DUPLICATE, TOKEN_PRIVILEGES, TOKEN_QUERY,
        TOKEN_USER, TokenDefaultDacl, TokenUser,
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
        SystemServices::SE_GROUP_ENABLED,
        Threading::{
            CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW,
            DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
            GetExitCodeProcess, GetProcessId, InitializeProcThreadAttributeList, OpenProcessToken,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
            PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW,
            TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
        },
    },
};

use super::{
    RuntimeSandboxPolicy, SandboxProcessOptions, SandboxStdio, policy_paths_with_resolved,
};

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
    acl_guards: Vec<AclGuard>,
    acl_cleaned: bool,
    profile: Option<AppContainerProfile>,
}

unsafe impl Send for RestrictedWindowsChild {}
unsafe impl Sync for RestrictedWindowsChild {}

struct LocalSid {
    ptr: PSID,
}

struct LocalMem<T>(*mut T);

struct OwnedHandle(HANDLE);

struct RestrictedToken {
    handle: OwnedHandle,
}

struct AppContainerProfile {
    name: Vec<u16>,
    sid: PSID,
}
unsafe impl Send for AppContainerProfile {}
unsafe impl Sync for AppContainerProfile {}
impl AppContainerProfile {
    fn create() -> io::Result<Self> {
        let name = to_wide(format!("DaatLocus.Native.{}", Uuid::new_v4().simple()));
        let mut sid = ptr::null_mut();
        let status = unsafe {
            CreateAppContainerProfile(
                name.as_ptr(),
                name.as_ptr(),
                name.as_ptr(),
                ptr::null(),
                0,
                &mut sid,
            )
        };
        if status < 0 || sid.is_null() {
            return Err(io::Error::other(format!(
                "AppContainer profile creation failed: HRESULT 0x{:08x}",
                status as u32
            )));
        }
        Ok(Self { name, sid })
    }
}
impl Drop for AppContainerProfile {
    fn drop(&mut self) {
        unsafe {
            DeleteAppContainerProfile(self.name.as_ptr());
            FreeSid(self.sid);
        }
    }
}

struct StartupAttributes {
    storage: Vec<usize>,
}
impl StartupAttributes {
    fn new() -> io::Result<Self> {
        let mut size = 0;
        unsafe {
            InitializeProcThreadAttributeList(ptr::null_mut(), 2, 0, &mut size);
        }
        let mut storage = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
        if unsafe {
            InitializeProcThreadAttributeList(storage.as_mut_ptr().cast(), 2, 0, &mut size)
        } == 0
        {
            return Err(last_os_error("InitializeProcThreadAttributeList failed"));
        }
        Ok(Self { storage })
    }
    fn pointer(&mut self) -> *mut c_void {
        self.storage.as_mut_ptr().cast()
    }
    fn update(&mut self, attribute: u32, value: *mut c_void, size: usize) -> io::Result<()> {
        if unsafe {
            UpdateProcThreadAttribute(
                self.pointer(),
                0,
                attribute as usize,
                value,
                size,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(last_os_error("UpdateProcThreadAttribute failed"));
        }
        Ok(())
    }
}
impl Drop for StartupAttributes {
    fn drop(&mut self) {
        unsafe {
            DeleteProcThreadAttributeList(self.pointer());
        }
    }
}

struct AclGuard {
    path: PathBuf,
    capability_sid: Vec<u8>,
}

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

impl WindowsSandboxAsyncChild {
    pub const fn id(&self) -> u32 {
        self.child.id()
    }

    pub const fn take_stdin(&mut self) -> Option<tokio::fs::File> {
        self.stdin.take()
    }

    pub const fn take_stdout(&mut self) -> Option<tokio::fs::File> {
        self.stdout.take()
    }

    pub const fn take_stderr(&mut self) -> Option<tokio::fs::File> {
        self.stderr.take()
    }

    pub fn start_kill(&mut self) -> io::Result<()> {
        self.child.kill()
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }
}
pub fn spawn_restricted_async(
    policy: &RuntimeSandboxPolicy,
    program: &PathBuf,
    args: Vec<String>,
    options: SandboxProcessOptions,
) -> io::Result<WindowsSandboxAsyncChild> {
    let profile = AppContainerProfile::create()?;
    let token = create_restricted_token(profile.sid)?;
    let mut acl_guards = Vec::new();
    let mut guarded_paths = HashSet::new();
    let process = match spawn_restricted_async_inner(RestrictedSpawnInput {
        policy,
        program,
        args,
        options,
        psid_capability: profile.sid,
        token: token.handle.raw(),
        acl_guards: &mut acl_guards,
        guarded_paths: &mut guarded_paths,
    }) {
        Ok(process) => process,
        Err(err) => {
            restore_acl_guards(&acl_guards);
            return Err(err);
        }
    };
    drop(token);
    let mut process = process;
    process.child.profile = Some(profile);
    Ok(process)
}

struct RestrictedSpawnInput<'a> {
    policy: &'a RuntimeSandboxPolicy,
    program: &'a Path,
    args: Vec<String>,
    options: SandboxProcessOptions,
    psid_capability: PSID,
    token: HANDLE,
    acl_guards: &'a mut Vec<AclGuard>,
    guarded_paths: &'a mut HashSet<PathBuf>,
}

fn spawn_restricted_async_inner(
    input: RestrictedSpawnInput<'_>,
) -> io::Result<WindowsSandboxAsyncChild> {
    let acl_lock = ACL_UPDATES
        .lock()
        .map_err(|_| io::Error::other("ACL update lock is unavailable"))?;
    // Protect exact credential files as well as framework directories so an
    // ancestor grant cannot be inherited, while keeping broker access.
    for root in policy_paths_with_resolved(&input.policy.filesystem.deny_read_paths) {
        if root.try_exists()? {
            protect_private_namespace(&root)?;
        }
    }
    apply_policy_acl_rules(
        input.policy,
        input.program,
        input.psid_capability,
        input.acl_guards,
        input.guarded_paths,
    )?;
    drop(acl_lock);
    let current_dir = input
        .options
        .current_dir
        .clone()
        .unwrap_or(std::env::current_dir()?);
    let argv = command_argv(input.program, input.args);
    let mut command_line = to_wide(argv_to_command_line(&argv));
    let program_wide = to_wide(input.program.as_os_str());
    let current_dir_wide = to_wide(current_dir.as_os_str());
    let env_block = environment_block(input.policy);
    let creation = crate::process_spawn::creation_lock()?;
    let (stdio, parent_pipes) = StartupHandles::new_with_parent_pipes(&input.options)?;
    let mut attributes = StartupAttributes::new()?;
    // Networking capabilities authorize network transports, not filesystem
    // access. Loopback retains Windows AppContainer's OS restrictions.
    let internet = LocalSid::from_string("S-1-15-3-1")?;
    let private_network = LocalSid::from_string("S-1-15-3-3")?;
    let mut network_capabilities = [
        SID_AND_ATTRIBUTES {
            Sid: internet.as_ptr(),
            Attributes: SE_GROUP_ENABLED as u32,
        },
        SID_AND_ATTRIBUTES {
            Sid: private_network.as_ptr(),
            Attributes: SE_GROUP_ENABLED as u32,
        },
    ];
    let mut capabilities = SECURITY_CAPABILITIES {
        AppContainerSid: input.psid_capability,
        Capabilities: network_capabilities.as_mut_ptr(),
        CapabilityCount: network_capabilities.len() as u32,
        Reserved: 0,
    };
    attributes.update(
        PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
        (&mut capabilities as *mut SECURITY_CAPABILITIES).cast(),
        std::mem::size_of::<SECURITY_CAPABILITIES>(),
    )?;
    let mut handles = vec![stdio.stdin, stdio.stdout, stdio.stderr];
    handles.retain(|handle| !is_invalid_handle(*handle));
    handles.sort_by_key(|handle| *handle as usize);
    handles.dedup();
    for handle in &handles {
        if unsafe { SetHandleInformation(*handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } == 0 {
            return Err(last_os_error("stdio inheritance setup failed"));
        }
    }
    attributes.update(
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
        handles.as_mut_ptr().cast(),
        handles.len() * std::mem::size_of::<HANDLE>(),
    )?;
    let mut startup_info = STARTUPINFOEXW {
        StartupInfo: startup_info(&stdio),
        lpAttributeList: attributes.pointer(),
    };
    startup_info.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    let mut process_info = PROCESS_INFORMATION::default();

    let created = unsafe {
        CreateProcessAsUserW(
            input.token,
            program_wide.as_ptr(),
            command_line.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            1,
            CREATE_UNICODE_ENVIRONMENT
                | CREATE_SUSPENDED
                | CREATE_NO_WINDOW
                | EXTENDED_STARTUPINFO_PRESENT,
            env_block.as_ptr().cast::<c_void>(),
            current_dir_wide.as_ptr(),
            &raw const startup_info.StartupInfo,
            &raw mut process_info,
        )
    };
    if created == 0 {
        return Err(last_os_error("CreateProcessAsUserW failed"));
    }
    drop(stdio);
    drop(creation);

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
    if unsafe { ResumeThread(process_info.hThread) } == u32::MAX {
        unsafe {
            TerminateProcess(process_info.hProcess, 1);
            CloseHandle(job);
            CloseHandle(process_info.hThread);
            CloseHandle(process_info.hProcess);
        }
        return Err(last_os_error("ResumeThread failed"));
    }

    let child = RestrictedWindowsChild {
        process: process_info.hProcess,
        thread: process_info.hThread,
        job,
        process_id: unsafe { GetProcessId(process_info.hProcess) },
        exit_status: None,
        acl_guards: std::mem::take(input.acl_guards),
        acl_cleaned: false,
        profile: None,
    };
    let (child_stdin, child_stdout, child_stderr) = parent_pipes.into_async_files();

    Ok(WindowsSandboxAsyncChild {
        child,
        stdin: child_stdin,
        stdout: child_stdout,
        stderr: child_stderr,
    })
}

fn apply_policy_acl_rules(
    policy: &RuntimeSandboxPolicy,
    program: &Path,
    psid: PSID,
    acl_guards: &mut Vec<AclGuard>,
    guarded_paths: &mut HashSet<PathBuf>,
) -> io::Result<()> {
    if let Some(parent) = program.parent() {
        add_program_read_ace(parent, psid, acl_guards, guarded_paths)?;
    }
    add_program_read_ace(program, psid, acl_guards, guarded_paths)?;

    for root in policy_paths_with_resolved(&policy.filesystem.readable_roots) {
        add_guarded_ace(
            &root,
            psid,
            WORKER_READ_ALLOW_MASK,
            SET_ACCESS,
            acl_guards,
            guarded_paths,
        )?;
    }
    for writable_root in &policy.filesystem.writable_roots {
        for root in policy_paths_with_resolved(std::slice::from_ref(&writable_root.root)) {
            add_guarded_ace(
                &root,
                psid,
                WORKER_WRITE_ALLOW_MASK,
                SET_ACCESS,
                acl_guards,
                guarded_paths,
            )?;
        }
        for subpath in policy_paths_with_resolved(&writable_root.read_only_subpaths) {
            add_guarded_ace_recursive(
                &subpath,
                psid,
                WORKER_DENY_WRITE_MASK,
                DENY_ACCESS,
                acl_guards,
                guarded_paths,
            )?;
        }
    }
    for path in policy_paths_with_resolved(&policy.filesystem.deny_write_paths) {
        add_guarded_ace_recursive(
            &path,
            psid,
            WORKER_DENY_WRITE_MASK,
            DENY_ACCESS,
            acl_guards,
            guarded_paths,
        )?;
    }
    // Restricting SID checks apply to reads and writes. Only the worker's
    // capability is denied, so the broker remains able to access private state.
    let deny_read_entries = [(psid, WORKER_DENY_READ_MASK, DENY_ACCESS)];
    let deny_read_directory_entries = [(psid, WORKER_DENY_READ_MASK, DENY_ACCESS)];
    for path in policy_paths_with_resolved(&policy.filesystem.deny_read_paths) {
        add_guarded_read_denies_recursive(
            &path,
            &deny_read_entries,
            &deny_read_directory_entries,
            acl_guards,
            guarded_paths,
        )?;
    }
    Ok(())
}

fn add_program_read_ace(
    path: &Path,
    psid: PSID,
    acl_guards: &mut Vec<AclGuard>,
    guarded_paths: &mut HashSet<PathBuf>,
) -> io::Result<()> {
    match add_guarded_ace(
        path,
        psid,
        WORKER_READ_ALLOW_MASK,
        SET_ACCESS,
        acl_guards,
        guarded_paths,
    ) {
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
    acl_guards: &mut Vec<AclGuard>,
    guarded_paths: &mut HashSet<PathBuf>,
) -> io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    ensure_acl_guard(path, psid, acl_guards, guarded_paths)?;
    add_explicit_aces(path, &[(psid, mask, mode)])?;
    Ok(())
}

fn add_guarded_aces(
    path: &Path,
    entries: &[(PSID, u32, i32)],
    acl_guards: &mut Vec<AclGuard>,
    guarded_paths: &mut HashSet<PathBuf>,
) -> io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let sid = entries
        .first()
        .ok_or_else(|| io::Error::other("missing capability ACL entry"))?
        .0;
    ensure_acl_guard(path, sid, acl_guards, guarded_paths)?;
    add_explicit_aces(path, entries)?;
    Ok(())
}

fn add_guarded_ace_recursive(
    path: &Path,
    psid: PSID,
    mask: u32,
    mode: i32,
    acl_guards: &mut Vec<AclGuard>,
    guarded_paths: &mut HashSet<PathBuf>,
) -> io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_dir() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let child = entry.path();
            if entry.file_type()?.is_dir() {
                add_guarded_ace_recursive(&child, psid, mask, mode, acl_guards, guarded_paths)?;
            } else {
                add_guarded_ace(&child, psid, mask, mode, acl_guards, guarded_paths)?;
            }
        }
    }
    add_guarded_ace(path, psid, mask, mode, acl_guards, guarded_paths)?;
    Ok(())
}

fn add_guarded_read_denies_recursive(
    path: &Path,
    file_entries: &[(PSID, u32, i32)],
    directory_entries: &[(PSID, u32, i32)],
    acl_guards: &mut Vec<AclGuard>,
    guarded_paths: &mut HashSet<PathBuf>,
) -> io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_dir() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let child = entry.path();
            if entry.file_type()?.is_dir() {
                add_guarded_read_denies_recursive(
                    &child,
                    file_entries,
                    directory_entries,
                    acl_guards,
                    guarded_paths,
                )?;
            } else {
                add_guarded_aces(&child, file_entries, acl_guards, guarded_paths)?;
            }
        }
        add_guarded_aces(path, directory_entries, acl_guards, guarded_paths)?;
    } else {
        add_guarded_aces(path, file_entries, acl_guards, guarded_paths)?;
    }
    Ok(())
}

fn ensure_acl_guard(
    path: &Path,
    sid: PSID,
    acl_guards: &mut Vec<AclGuard>,
    guarded_paths: &mut HashSet<PathBuf>,
) -> io::Result<()> {
    if !guarded_paths.insert(path.to_path_buf()) {
        return Ok(());
    }
    let length = unsafe { GetLengthSid(sid) };
    let mut capability_sid = vec![0u8; length as usize];
    if unsafe { CopySid(length, capability_sid.as_mut_ptr().cast(), sid) } == 0 {
        return Err(last_os_error("CopySid failed"));
    }
    acl_guards.push(AclGuard {
        path: path.to_path_buf(),
        capability_sid,
    });
    Ok(())
}

fn capture_dacl(path: &Path) -> io::Result<Option<Vec<u32>>> {
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
            &raw mut dacl,
            ptr::null_mut(),
            &raw mut security_descriptor,
        )
    };
    if code != ERROR_SUCCESS {
        return Err(win32_error(
            code,
            format!("GetNamedSecurityInfoW failed for {}", path.display()),
        ));
    }
    let _security_descriptor = LocalMem(security_descriptor);
    if dacl.is_null() {
        return Ok(None);
    }
    let mut info = ACL_SIZE_INFORMATION::default();
    let ok = unsafe {
        GetAclInformation(
            dacl.cast::<ACL>(),
            (&raw mut info).cast::<c_void>(),
            u32::try_from(std::mem::size_of::<ACL_SIZE_INFORMATION>())
                .expect("ACL size information size fits in u32"),
            AclSizeInformation,
        )
    };
    if ok == 0 {
        return Err(last_os_error("GetAclInformation failed"));
    }
    let acl_bytes = usize::try_from(info.AclBytesInUse).expect("ACL byte count fits in usize");
    let mut words = vec![0u32; acl_bytes.div_ceil(std::mem::size_of::<u32>())];
    unsafe {
        ptr::copy_nonoverlapping(
            dacl.cast::<u8>(),
            words.as_mut_ptr().cast::<u8>(),
            acl_bytes,
        );
    }
    Ok(Some(words))
}

fn add_explicit_aces(path: &Path, entries: &[(PSID, u32, i32)]) -> io::Result<()> {
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
            &raw mut dacl,
            ptr::null_mut(),
            &raw mut security_descriptor,
        )
    };
    if code != ERROR_SUCCESS {
        return Err(win32_error(
            code,
            format!("GetNamedSecurityInfoW failed for {}", path.display()),
        ));
    }
    let _security_descriptor = LocalMem(security_descriptor);
    let inheritance = inheritance_for_path(path);
    let explicits = entries
        .iter()
        .map(|(psid, mask, mode)| explicit_access(*psid, *mask, *mode, inheritance))
        .collect::<Vec<_>>();
    let mut new_dacl = ptr::null_mut();
    let code = unsafe {
        SetEntriesInAclW(
            u32::try_from(explicits.len()).expect("explicit ACL entry count fits in u32"),
            explicits.as_ptr(),
            dacl,
            &raw mut new_dacl,
        )
    };
    if code != ERROR_SUCCESS {
        return Err(win32_error(
            code,
            format!("SetEntriesInAclW failed for {}", path.display()),
        ));
    }
    let _new_dacl = LocalMem(new_dacl);
    let code = unsafe {
        SetNamedSecurityInfoW(
            path_wide.as_ptr().cast_mut(),
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

fn restore_acl_guards(guards: &[AclGuard]) {
    let Ok(_lock) = ACL_UPDATES.lock() else {
        return;
    };
    for guard in guards.iter().rev() {
        let _ = restore_acl_guard(guard);
    }
}

fn sid_text(sid: PSID) -> io::Result<String> {
    let mut text = ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(last_os_error("SID conversion failed"));
    }
    let allocation = LocalMem(text);
    let mut length = 0;
    unsafe {
        while *text.add(length) != 0 && length < 1024 {
            length += 1;
        }
    }
    let value = unsafe { String::from_utf16(std::slice::from_raw_parts(text, length)) }
        .map_err(|_| io::Error::other("invalid SID encoding"));
    drop(allocation);
    value
}

fn protect_private_namespace(path: &Path) -> io::Result<()> {
    let Some(current) = capture_dacl(path)? else {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private namespace has no access boundary",
        ));
    };
    let original = current.as_ptr().cast::<ACL>();
    let size = current.len() * std::mem::size_of::<u32>();
    let mut filtered = vec![0u32; current.len()];
    let acl = filtered.as_mut_ptr().cast::<ACL>();
    if unsafe { InitializeAcl(acl, size as u32, (*original).AclRevision as u32) } == 0 {
        return Err(last_os_error("InitializeAcl failed"));
    }
    for index in 0..unsafe { (*original).AceCount } {
        let mut ace = ptr::null_mut();
        if unsafe { GetAce(original, index as u32, &mut ace) } == 0 {
            return Err(last_os_error("GetAce failed"));
        }
        let bytes = ace.cast::<u8>();
        let kind = unsafe { *bytes };
        let length = unsafe { std::ptr::read_unaligned(bytes.add(2).cast::<u16>()) } as u32;
        if kind == 0 {
            let sid = unsafe { bytes.add(8) }.cast::<c_void>();
            if sid_text(sid)?.starts_with("S-1-15-2-") {
                continue;
            }
        } else if kind != 1 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "private namespace has unsupported conditional/object ACL",
            ));
        }
        if unsafe { AddAce(acl, (*original).AclRevision as u32, u32::MAX, ace, length) } == 0 {
            return Err(last_os_error("AddAce failed"));
        }
    }
    let mut name = to_wide(path.as_os_str());
    let status = unsafe {
        SetNamedSecurityInfoW(
            name.as_mut_ptr(),
            1,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            acl,
            ptr::null_mut(),
        )
    };
    if status != 0 {
        return Err(win32_error(
            status,
            "cannot establish protected private namespace",
        ));
    }
    Ok(())
}

fn restore_acl_guard(guard: &AclGuard) -> io::Result<()> {
    if !guard.path.exists() {
        return Ok(());
    }
    let path_wide = to_wide(guard.path.as_os_str());
    let Some(current) = capture_dacl(&guard.path)? else {
        return Ok(());
    };
    let sid = guard.capability_sid.as_ptr().cast::<c_void>().cast_mut();
    // Rebuild the current ACL, removing this unique package's allow and deny
    // entries while preserving other workers and OS-owner changes.
    let original = current.as_ptr().cast::<ACL>();
    let mut filtered = vec![0u32; current.len()];
    let dacl = filtered.as_mut_ptr().cast::<ACL>();
    if unsafe {
        InitializeAcl(
            dacl,
            (current.len() * 4) as u32,
            (*original).AclRevision as u32,
        )
    } == 0
    {
        return Err(last_os_error("InitializeAcl failed"));
    }
    for index in 0..unsafe { (*original).AceCount } {
        let mut ace = ptr::null_mut();
        if unsafe { GetAce(original, index as u32, &mut ace) } == 0 {
            return Err(last_os_error("GetAce failed"));
        }
        let bytes = ace.cast::<u8>();
        let kind = unsafe { *bytes };
        let length = unsafe { ptr::read_unaligned(bytes.add(2).cast::<u16>()) } as u32;
        if matches!(kind, 0 | 1)
            && unsafe { windows_sys::Win32::Security::EqualSid(bytes.add(8).cast(), sid) } != 0
        {
            continue;
        }
        if unsafe { AddAce(dacl, (*original).AclRevision as u32, u32::MAX, ace, length) } == 0 {
            return Err(last_os_error("AddAce failed"));
        }
    }
    let code = unsafe {
        SetNamedSecurityInfoW(
            path_wide.as_ptr().cast_mut(),
            1,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            dacl,
            ptr::null_mut(),
        )
    };
    if code != ERROR_SUCCESS {
        return Err(win32_error(
            code,
            format!("SetNamedSecurityInfoW failed for {}", guard.path.display()),
        ));
    }
    Ok(())
}

const fn explicit_access(psid: PSID, mask: u32, mode: i32, inheritance: u32) -> EXPLICIT_ACCESS_W {
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

fn create_restricted_token(package_sid: PSID) -> io::Result<RestrictedToken> {
    let base = current_token_for_restriction()?;
    let mut token = ptr::null_mut();
    let ok = unsafe {
        CreateRestrictedToken(
            base.raw(),
            DISABLE_MAX_PRIVILEGE | LUA_TOKEN,
            0,
            ptr::null(),
            0,
            ptr::null(),
            0,
            ptr::null(),
            &raw mut token,
        )
    };
    if ok == 0 {
        return Err(last_os_error("CreateRestrictedToken failed"));
    }
    let token = OwnedHandle(token);
    let mut user = token_user_sid_bytes(base.raw())?;
    set_default_dacl(token.raw(), &[package_sid, user.as_mut_ptr().cast()])?;
    enable_single_privilege(token.raw(), "SeChangeNotifyPrivilege")?;
    Ok(RestrictedToken { handle: token })
}

fn token_user_sid_bytes(token: HANDLE) -> io::Result<Vec<u8>> {
    let mut size = 0;
    unsafe {
        GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut size);
    }
    let mut buffer = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            size,
            &mut size,
        )
    } == 0
    {
        return Err(last_os_error("TokenUser failed"));
    }
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let length = unsafe { GetLengthSid(user.User.Sid) };
    let mut sid = vec![0u8; length as usize];
    if unsafe { CopySid(length, sid.as_mut_ptr().cast(), user.User.Sid) } == 0 {
        return Err(last_os_error("CopySid failed"));
    }
    Ok(sid)
}

fn current_token_for_restriction() -> io::Result<OwnedHandle> {
    let desired = TOKEN_DUPLICATE
        | TOKEN_QUERY
        | TOKEN_ASSIGN_PRIMARY
        | TOKEN_ADJUST_DEFAULT
        | TOKEN_ADJUST_SESSIONID
        | TOKEN_ADJUST_PRIVILEGES;
    let mut token = ptr::null_mut();
    let ok = unsafe { OpenProcessToken(GetCurrentProcess(), desired, &raw mut token) };
    if ok == 0 {
        Err(last_os_error("OpenProcessToken failed"))
    } else {
        Ok(OwnedHandle(token))
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
            u32::try_from(entries.len()).expect("default DACL entry count fits in u32"),
            entries.as_ptr(),
            ptr::null(),
            &raw mut new_dacl,
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
            (&raw const info).cast::<c_void>(),
            u32::try_from(std::mem::size_of::<TOKEN_DEFAULT_DACL>())
                .expect("token default DACL size fits in u32"),
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
    let ok = unsafe { LookupPrivilegeValueW(ptr::null(), name_wide.as_ptr(), &raw mut luid) };
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
        AdjustTokenPrivileges(
            token,
            0,
            &raw const privileges,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
        )
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
            (&raw const limits).cast::<c_void>(),
            u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                .expect("job-object limit information size fits in u32"),
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
    const fn id(&self) -> u32 {
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

    fn finish_after_exit(&mut self) -> io::Result<ExitStatus> {
        let mut code = 1;
        let ok = unsafe { GetExitCodeProcess(self.process, &raw mut code) };
        if ok == 0 {
            return Err(last_os_error("GetExitCodeProcess failed"));
        }
        self.close_thread_handle();
        if !is_invalid_handle(self.job) {
            unsafe {
                TerminateJobObject(self.job, 0);
            }
        }
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
        restore_acl_guards(&self.acl_guards);
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
        let ok = unsafe { ConvertStringSidToSidW(wide.as_ptr(), &raw mut ptr) };
        if ok == 0 {
            Err(last_os_error("ConvertStringSidToSidW failed"))
        } else {
            Ok(Self { ptr })
        }
    }

    const fn as_ptr(&self) -> PSID {
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
    const fn raw(&self) -> HANDLE {
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
    fn new_with_parent_pipes(
        options: &SandboxProcessOptions,
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
            mode => stdio_handle(
                matches!(mode, SandboxStdio::Inherit),
                STD_INPUT_HANDLE,
                true,
                &mut owned,
            )?,
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
            mode => stdio_handle(
                matches!(mode, SandboxStdio::Inherit),
                STD_OUTPUT_HANDLE,
                false,
                &mut owned,
            )?,
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
            mode => stdio_handle(
                matches!(mode, SandboxStdio::Inherit),
                STD_ERROR_HANDLE,
                false,
                &mut owned,
            )?,
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
    ) -> (
        Option<tokio::fs::File>,
        Option<tokio::fs::File>,
        Option<tokio::fs::File>,
    ) {
        (
            owned_handle_to_async_file(self.stdin),
            owned_handle_to_async_file(self.stdout),
            owned_handle_to_async_file(self.stderr),
        )
    }
}

fn owned_handle_to_async_file(handle: Option<OwnedHandle>) -> Option<tokio::fs::File> {
    let handle = handle?;
    let raw = handle.into_raw();
    Some(unsafe { tokio::fs::File::from_raw_handle(raw as RawHandle) })
}

fn startup_info(handles: &StartupHandles) -> STARTUPINFOW {
    STARTUPINFOW {
        cb: u32::try_from(std::mem::size_of::<STARTUPINFOW>())
            .expect("startup information size fits in u32"),
        dwFlags: STARTF_USESTDHANDLES,
        hStdInput: handles.stdin,
        hStdOutput: handles.stdout,
        hStdError: handles.stderr,
        ..Default::default()
    }
}

fn stdio_handle(
    inherit: bool,
    standard_handle: u32,
    read_access: bool,
    owned: &mut Vec<OwnedHandle>,
) -> io::Result<HANDLE> {
    let handle = if inherit {
        let inherited = unsafe { GetStdHandle(standard_handle) };
        if is_invalid_handle(inherited) {
            open_nul(read_access, owned)?
        } else {
            inherited
        }
    } else {
        open_nul(read_access, owned)?
    };
    set_handle_inherit(handle, true)?;
    Ok(handle)
}

fn create_pipe_pair(label: &str) -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut read: HANDLE = ptr::null_mut();
    let mut write: HANDLE = ptr::null_mut();
    let ok = unsafe { CreatePipe(&raw mut read, &raw mut write, ptr::null_mut(), 0) };
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
    let mut command_line = Vec::with_capacity(args.len() + 1);
    command_line.push(program.to_string_lossy().into_owned());
    command_line.extend(args);
    command_line
}

fn environment_block(policy: &RuntimeSandboxPolicy) -> Vec<u16> {
    let mut env = BTreeMap::<OsString, OsString>::new();
    for (name, value) in std::env::vars_os() {
        if os_env_name_is_protected(policy, &name) {
            continue;
        }
        env.insert(name, value);
    }

    let mut block = Vec::new();
    for (name, value) in env {
        let mut item = to_wide(name);
        item.pop();
        item.push(u16::from(b'='));
        let mut value_wide = to_wide(value);
        value_wide.pop();
        block.extend(item);
        block.extend(value_wide);
        block.push(0);
    }
    block.push(0);
    block
}

fn os_env_name_is_protected(policy: &RuntimeSandboxPolicy, name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    policy.is_env_var_protected(name)
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
        io::Error::from_raw_os_error(err.cast_signed())
    ))
}

fn win32_error(code: u32, context: impl Into<String>) -> io::Error {
    let message = format!(
        "{}: {}",
        context.into(),
        io::Error::from_raw_os_error(code.cast_signed())
    );
    if code == ERROR_ACCESS_DENIED {
        io::Error::new(io::ErrorKind::PermissionDenied, message)
    } else {
        io::Error::other(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::{FileSystemSandboxPolicy, StrongFilesystemSandboxMode, WritableRoot};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn quote(path: &Path) -> String {
        format!("'{}'", path.to_string_lossy().replace('\'', "''"))
    }
    fn acl_has_sid(path: &Path, wanted: &str) -> bool {
        let Some(words) = capture_dacl(path).unwrap() else {
            return false;
        };
        let acl = words.as_ptr().cast::<ACL>();
        for index in 0..unsafe { (*acl).AceCount } {
            let mut ace = ptr::null_mut();
            assert_ne!(unsafe { GetAce(acl, index as u32, &mut ace) }, 0);
            let bytes = ace.cast::<u8>();
            if matches!(unsafe { *bytes }, 0 | 1)
                && sid_text(unsafe { bytes.add(8) }.cast()).unwrap() == wanted
            {
                return true;
            }
        }
        false
    }
    #[test]
    fn package_acl_cleanup_preserves_concurrent_package_and_owner_changes() {
        let temp = tempfile::tempdir().unwrap();
        let first = AppContainerProfile::create().unwrap();
        let second = AppContainerProfile::create().unwrap();
        let first_id = sid_text(first.sid).unwrap();
        let second_id = sid_text(second.sid).unwrap();
        let mut a = vec![];
        let mut b = vec![];
        add_guarded_ace(
            temp.path(),
            first.sid,
            WORKER_WRITE_ALLOW_MASK,
            SET_ACCESS,
            &mut a,
            &mut HashSet::new(),
        )
        .unwrap();
        add_guarded_ace(
            temp.path(),
            second.sid,
            WORKER_READ_ALLOW_MASK,
            SET_ACCESS,
            &mut b,
            &mut HashSet::new(),
        )
        .unwrap();
        let world = LocalSid::from_string("S-1-1-0").unwrap();
        add_explicit_aces(
            temp.path(),
            &[(world.as_ptr(), FILE_GENERIC_READ, SET_ACCESS)],
        )
        .unwrap();
        restore_acl_guards(&a);
        assert!(!acl_has_sid(temp.path(), &first_id));
        assert!(acl_has_sid(temp.path(), &second_id));
        assert!(acl_has_sid(temp.path(), "S-1-1-0"));
        restore_acl_guards(&b);
        assert!(!acl_has_sid(temp.path(), &first_id));
        assert!(!acl_has_sid(temp.path(), &second_id));
        assert!(acl_has_sid(temp.path(), "S-1-1-0"));
    }
    async fn collect(mut child: WindowsSandboxAsyncChild) -> (ExitStatus, String, String) {
        let mut stdout = child.take_stdout();
        let mut stderr = child.take_stderr();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if tokio::time::Instant::now() >= deadline {
                child.start_kill().unwrap();
                panic!("native sandbox probe timed out");
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };
        let mut out = String::new();
        let mut err = String::new();
        if let Some(stdout) = &mut stdout {
            stdout.read_to_string(&mut out).await.unwrap();
        }
        if let Some(stderr) = &mut stderr {
            stderr.read_to_string(&mut err).await.unwrap();
        }
        (status, out, err)
    }
    #[tokio::test]
    async fn appcontainer_enforces_read_write_scope_and_future_private_namespace() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let private = temp.path().join("private");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir(&private).unwrap();
        let inside = workspace.join("inside.txt");
        let outside = temp.path().join("outside.txt");
        let public = temp.path().join("world.txt");
        let output = workspace.join("output.txt");
        let credential = workspace.join("credential.json");
        let outside_write = temp.path().join("outside-write.txt");
        std::fs::write(&inside, "inside").unwrap();
        std::fs::write(&outside, "outside").unwrap();
        std::fs::write(&public, "world-readable").unwrap();
        std::fs::write(&credential, "synthetic credential").unwrap();
        let world = LocalSid::from_string("S-1-1-0").unwrap();
        add_explicit_aces(&public, &[(world.as_ptr(), FILE_GENERIC_READ, SET_ACCESS)]).unwrap();
        let mut policy = RuntimeSandboxPolicy {
            filesystem: FileSystemSandboxPolicy {
                full_disk_read: false,
                full_disk_write: false,
                readable_roots: vec![workspace.clone()],
                writable_roots: vec![WritableRoot {
                    root: workspace.clone(),
                    read_only_subpaths: vec![],
                }],
                deny_read_paths: vec![private.clone(), credential.clone()],
                deny_write_paths: vec![private.clone(), credential.clone()],
                private_delivery: None,
                bound_roots: vec![],
            },
            protected_env_vars: vec![],
            strong_filesystem: StrongFilesystemSandboxMode::Required,
        };
        let powershell = PathBuf::from(
            std::env::var_os("SystemRoot").unwrap_or_else(|| OsString::from("C:\\Windows")),
        )
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let script = format!(
            "$ErrorActionPreference='Stop'; if([IO.File]::ReadAllText({}) -ne 'inside'){{exit 10}}; [IO.File]::WriteAllText({},'allowed'); foreach($p in @({},{})){{ $allowed=$false;try{{$null=[IO.File]::ReadAllText($p);$allowed=$true}}catch{{}};if($allowed){{exit 20}} }}; $allowed=$false;try{{[IO.File]::WriteAllText({},'forbidden');$allowed=$true}}catch{{}};if($allowed){{exit 30}}; [Console]::WriteLine('scope-ok')",
            quote(&inside),
            quote(&output),
            quote(&outside),
            quote(&public),
            quote(&outside_write)
        );
        let spawn = policy.shell_spawn_spec(
            powershell.to_str().unwrap(),
            vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                script,
            ],
        );
        let child = spawn_restricted_async(
            &policy,
            &spawn.program,
            spawn.args,
            SandboxProcessOptions {
                current_dir: Some(workspace.clone()),
                stdin: SandboxStdio::Null,
                stdout: SandboxStdio::Piped,
                stderr: SandboxStdio::Piped,
            },
        )
        .unwrap();
        let first_sid = sid_text(child.child.profile.as_ref().unwrap().sid).unwrap();
        let (status, out, err) = collect(child).await;
        assert!(
            status.success(),
            "strict native probe {status}; stdout={out:?},stderr={err:?}"
        );
        assert!(out.contains("scope-ok"));
        assert_eq!(std::fs::read_to_string(&output).unwrap(), "allowed");
        assert!(!outside_write.exists());
        assert!(!acl_has_sid(&workspace, &first_sid));
        assert!(!acl_has_sid(&private, &first_sid));
        // A broader approved read root must still deny a new sibling created
        // after the persistent native process has already started.
        policy.filesystem.readable_roots = vec![temp.path().to_path_buf()];
        let future = private.join("future-actor/upload.txt");
        let existing_private = private.join("existing.txt");
        std::fs::write(&existing_private, "existing private").unwrap();
        let script = format!(
            "$ErrorActionPreference='Stop'; if([IO.File]::ReadAllText({}) -ne 'inside'){{exit 37}}; $allowed=$false;try{{$null=[IO.File]::ReadAllText({});$allowed=$true}}catch{{}};if($allowed){{exit 38}}; $allowed=$false;try{{$null=[IO.File]::ReadAllText({});$allowed=$true}}catch{{}};if($allowed){{exit 39}};[Console]::WriteLine('ready'); $null=[Console]::ReadLine(); $allowed=$false;try{{$null=[IO.File]::ReadAllText({});$allowed=$true}}catch{{}};if($allowed){{exit 40}};[Console]::WriteLine('future-denied')",
            quote(&inside),
            quote(&credential),
            quote(&existing_private),
            quote(&future)
        );
        let spawn = policy.shell_spawn_spec(
            powershell.to_str().unwrap(),
            vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                script,
            ],
        );
        let mut child = spawn_restricted_async(
            &policy,
            &spawn.program,
            spawn.args,
            SandboxProcessOptions {
                current_dir: Some(workspace),
                stdin: SandboxStdio::Piped,
                stdout: SandboxStdio::Piped,
                stderr: SandboxStdio::Piped,
            },
        )
        .unwrap();
        std::fs::create_dir(future.parent().unwrap()).unwrap();
        std::fs::write(&future, "future private data").unwrap();
        let second_sid = sid_text(child.child.profile.as_ref().unwrap().sid).unwrap();
        child
            .take_stdin()
            .unwrap()
            .write_all(b"go\n")
            .await
            .unwrap();
        let (status, out, err) = collect(child).await;
        assert!(
            status.success(),
            "future native probe {status}; stdout={out:?},stderr={err:?}"
        );
        assert!(out.contains("future-denied"));
        assert!(!acl_has_sid(&private, &second_sid));
        assert!(!acl_has_sid(&credential, &second_sid));
        assert!(!acl_has_sid(future.parent().unwrap(), &second_sid));
        assert_eq!(
            std::fs::read_to_string(&credential).unwrap(),
            "synthetic credential"
        );
        assert_eq!(
            std::fs::read_to_string(future).unwrap(),
            "future private data"
        );
    }
}
