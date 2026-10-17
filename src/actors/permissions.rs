//! Capability checks shared by Actor creation and every tool execution.
use super::{ActorConfig, ActorError};
use crate::sandbox::{
    FileSystemSandboxPolicy, RuntimeSandboxPolicy, StrongFilesystemSandboxMode, WritableRoot,
};
use miette::{IntoDiagnostic, Result};
use std::path::{Path, PathBuf};

pub fn canonical_path(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(ActorError::Invalid(
            "Permission roots and workspaces must be absolute paths".into(),
        )
        .into());
    }
    let normalized = RuntimeSandboxPolicy::resolve_path(path, None);
    let mut existing = normalized.as_path();
    let mut suffix = Vec::new();
    while !existing.exists() {
        suffix.push(
            existing
                .file_name()
                .ok_or_else(|| ActorError::Invalid("Invalid filesystem root".into()))?
                .to_owned(),
        );
        existing = existing
            .parent()
            .ok_or_else(|| ActorError::Invalid("Invalid filesystem root".into()))?;
    }
    let mut result = std::fs::canonicalize(existing).into_diagnostic()?;
    for component in suffix.into_iter().rev() {
        result.push(component);
    }
    Ok(result)
}
pub fn normalize_config(mut config: ActorConfig) -> Result<ActorConfig> {
    if config.name.trim().is_empty() {
        return Err(ActorError::Invalid("Invalid Actor name".into()).into());
    }
    crate::builtin_apps::validate_app_names(&config.apps, config.permissions.read_only)
        .map_err(|_| ActorError::Invalid("Invalid or unavailable App configuration".into()))?;
    if let Some(apps) = &config.permissions.app_access {
        crate::builtin_apps::validate_app_grants(apps, config.permissions.read_only)?;
    }
    if let Some(apps) = &config.permissions.delegation_apps {
        crate::builtin_apps::validate_app_grants(apps, config.permissions.read_only)
            .map_err(|_| ActorError::Invalid("Invalid delegation App ceiling".into()))?;
    }
    config.workspace = canonical_path(&config.workspace)?;
    config.permissions.readable_roots = config
        .permissions
        .readable_roots
        .iter()
        .map(|p| canonical_path(p))
        .collect::<Result<_>>()?;
    config.permissions.writable_roots = config
        .permissions
        .writable_roots
        .iter()
        .map(|p| canonical_path(p))
        .collect::<Result<_>>()?;
    config.permissions.readable_roots.sort();
    config.permissions.readable_roots.dedup();
    config.permissions.writable_roots.sort();
    config.permissions.writable_roots.dedup();
    if config.permissions.read_only && !config.permissions.writable_roots.is_empty() {
        return Err(ActorError::Invalid("Read-only Actors cannot have write grants".into()).into());
    }
    if !covered(&config.workspace, &config.permissions.readable_roots) {
        return Err(ActorError::Invalid("Workspace requires a read grant".into()).into());
    }
    for root in &config.permissions.writable_roots {
        if !covered(root, &config.permissions.readable_roots) {
            return Err(
                ActorError::Invalid("Write grants require a matching read grant".into()).into(),
            );
        }
    }
    Ok(config)
}
fn covered(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}
pub fn delegation_apps(config: &ActorConfig) -> &[String] {
    config
        .permissions
        .delegation_apps
        .as_deref()
        .unwrap_or_else(|| direct_apps(config))
}
pub fn direct_apps(config: &ActorConfig) -> &[String] {
    config
        .permissions
        .app_access
        .as_deref()
        .unwrap_or(&config.apps)
}
pub fn app_granted(grants: &[String], app: &str) -> bool {
    grants.iter().any(|grant| grant == "*" || grant == app)
}
pub fn intersect_app_grants(a: &[String], b: &[String]) -> Vec<String> {
    if a.iter().any(|value| value == "*") {
        return b.to_vec();
    }
    if b.iter().any(|value| value == "*") {
        return a.to_vec();
    }
    a.iter().filter(|app| b.contains(app)).cloned().collect()
}
pub fn check_child(parent: &ActorConfig, child: &ActorConfig) -> Result<()> {
    let p = &parent.permissions;
    let c = &child.permissions;
    if !p.allow_children
        || (p.read_only && !c.read_only)
        || (c.allow_children && !p.allow_children)
        || approval_strength(c.approval_mode) < approval_strength(p.approval_mode)
        || direct_apps(child)
            .iter()
            .any(|app| !app_granted(delegation_apps(parent), app))
        || delegation_apps(child)
            .iter()
            .any(|app| !app_granted(delegation_apps(parent), app))
        || c.readable_roots
            .iter()
            .any(|root| !covered(root, &p.readable_roots))
        || c.writable_roots
            .iter()
            .any(|root| !covered(root, &p.writable_roots))
        || c.message_targets
            .iter()
            .any(|target| !p.message_targets.contains(target))
        || p.tools.as_ref().is_some_and(|allowed| {
            c.tools
                .as_ref()
                .is_none_or(|requested| requested.iter().any(|tool| !allowed.contains(tool)))
        })
    {
        return Err(
            ActorError::Forbidden("Delegation cannot increase Actor capabilities".into()).into(),
        );
    }
    Ok(())
}

pub fn check_recipient(sender: &ActorConfig, recipient: &ActorConfig) -> Result<()> {
    let mut ceiling = sender.clone();
    ceiling.permissions.allow_children = true;
    let mut requested = recipient.clone();
    requested.permissions.allow_children = false;
    check_child(&ceiling, &requested)
}
pub(crate) fn approval_strength(mode: super::ApprovalMode) -> u8 {
    match mode {
        super::ApprovalMode::None => 0,
        super::ApprovalMode::Auto => 1,
        super::ApprovalMode::Ask => 2,
    }
}
pub fn policy(
    config: &ActorConfig,
    own_delivery: &Path,
    denied: Vec<PathBuf>,
    protected_env_vars: Vec<String>,
) -> RuntimeSandboxPolicy {
    let mut readable_roots = config.permissions.readable_roots.clone();
    readable_roots.push(own_delivery.to_path_buf());
    RuntimeSandboxPolicy {
        filesystem: FileSystemSandboxPolicy {
            full_disk_read: false,
            full_disk_write: false,
            private_delivery: Some(crate::sandbox::PrivateDeliveryScope {
                root: own_delivery.parent().unwrap_or(own_delivery).to_path_buf(),
                own_readable: own_delivery.to_path_buf(),
            }),
            readable_roots,
            bound_roots: config
                .permissions
                .readable_roots
                .iter()
                .chain(config.permissions.writable_roots.iter())
                .cloned()
                .chain(std::iter::once(own_delivery.to_path_buf()))
                .collect(),
            writable_roots: config
                .permissions
                .writable_roots
                .iter()
                .map(|root| WritableRoot {
                    root: root.clone(),
                    read_only_subpaths: vec![],
                })
                .collect(),
            deny_read_paths: denied.clone(),
            deny_write_paths: denied
                .into_iter()
                .chain(std::iter::once(own_delivery.to_path_buf()))
                .collect(),
        },
        protected_env_vars,
        strong_filesystem: StrongFilesystemSandboxMode::Required,
    }
}

/// Private service state is accessible to the OS owner, never an ambient group.
pub fn protect_private_path(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if path.is_dir() { 0o700 } else { 0o600 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).into_diagnostic()?;
    }
    #[cfg(windows)]
    windows_private_acl(path)?;
    #[cfg(not(any(unix, windows)))]
    return Err(ActorError::Unavailable(
        "Private storage permissions are unsupported on this platform".into(),
    )
    .into());
    Ok(())
}

#[cfg(windows)]
fn windows_private_acl(path: &Path) -> Result<()> {
    use std::{os::windows::ffi::OsStrExt, sync::OnceLock};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, LocalFree},
        Security::Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SDDL_REVISION_1, SE_FILE_OBJECT, SetNamedSecurityInfoW,
        },
        Security::{
            DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetTokenInformation,
            PROTECTED_DACL_SECURITY_INFORMATION, TOKEN_QUERY, TOKEN_USER, TokenUser,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };
    static SID: OnceLock<std::result::Result<String, String>> = OnceLock::new();
    let sid = SID
        .get_or_init(|| unsafe {
            let mut token = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err("Unable to determine the OS owner".into());
            }
            let mut length = 0;
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut length);
            if length == 0 {
                CloseHandle(token);
                return Err("Unable to determine the OS owner".into());
            }
            let mut buffer = vec![0usize; (length as usize).div_ceil(std::mem::size_of::<usize>())];
            if GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                length,
                &mut length,
            ) == 0
            {
                CloseHandle(token);
                return Err("Unable to determine the OS owner".into());
            }
            let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
            let mut sid = std::ptr::null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut sid) == 0 {
                CloseHandle(token);
                return Err("Unable to determine the OS owner".into());
            }
            let mut chars = 0;
            while *sid.add(chars) != 0 && chars < 1024 {
                chars += 1;
            }
            let value = String::from_utf16(std::slice::from_raw_parts(sid, chars))
                .map_err(|_| "Invalid OS owner SID".to_string());
            LocalFree(sid.cast());
            CloseHandle(token);
            value
        })
        .as_ref()
        .map_err(|_| ActorError::Unavailable("Cannot establish private service storage".into()))?;
    let sddl = format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;{sid})");
    let wide_sddl: Vec<u16> = sddl.encode_utf16().chain([0]).collect();
    let mut descriptor = std::ptr::null_mut();
    let mut wide_path: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide_sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(
                ActorError::Unavailable("Cannot establish private service storage".into()).into(),
            );
        }
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl = std::ptr::null_mut();
        let fetched =
            GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted);
        let result = if fetched != 0 && present != 0 {
            SetNamedSecurityInfoW(
                wide_path.as_mut_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null_mut(),
            )
        } else {
            1
        };
        LocalFree(descriptor);
        if result != 0 {
            return Err(
                ActorError::Unavailable("Cannot establish private service storage".into()).into(),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod delegation_tests {
    use super::*;
    #[test]
    fn wildcard_delegation_intersection_never_expands_a_specific_ceiling() {
        for (a, b, expected) in [
            (vec!["*"], vec!["files"], vec!["files"]),
            (vec!["files"], vec!["*"], vec!["files"]),
            (vec!["*"], vec![], vec![]),
            (
                vec!["files", "browser"],
                vec!["browser", "terminal"],
                vec!["browser"],
            ),
            (vec!["*"], vec!["*"], vec!["*"]),
        ] {
            let values =
                |items: Vec<&str>| items.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert_eq!(
                intersect_app_grants(&values(a), &values(b)),
                values(expected)
            );
        }
    }
    #[test]
    fn portable_core_delegates_only_inside_its_separate_ceiling() {
        let directory = tempfile::tempdir().unwrap();
        let mut parent = ActorConfig {
            name: "Core".into(),
            workspace: directory.path().to_path_buf(),
            apps: vec!["actors".into()],
            model: None,
            instructions: String::new(),
            permissions: super::super::ActorPermissions::workspace(
                directory.path().to_path_buf(),
                false,
            ),
        };
        parent.permissions.delegation_apps = Some(vec!["actors".into(), "files".into()]);
        let mut child = parent.clone();
        child.apps = vec!["files".into()];
        child.permissions.delegation_apps = None;
        assert!(check_child(&parent, &child).is_ok());
        child.apps = vec!["terminal".into()];
        assert!(check_child(&parent, &child).is_err());
        child.apps = vec!["files".into()];
        child.permissions.delegation_apps = Some(vec!["terminal".into()]);
        assert!(check_child(&parent, &child).is_err());
        child.permissions.delegation_apps = None;
        parent.permissions.read_only = true;
        assert!(check_child(&parent, &child).is_err());
        child.permissions.read_only = true;
        child.permissions.writable_roots.clear();
        parent.permissions.writable_roots.clear();
        assert!(check_child(&parent, &child).is_ok());
        parent.permissions.tools = Some(vec!["actors__create".into()]);
        assert!(check_child(&parent, &child).is_err());
    }
}
