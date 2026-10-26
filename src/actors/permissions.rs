//! Canonical filesystem targets, Actor configuration and OS storage protection.
use super::{ActorConfig, ActorError};
use crate::sandbox::RuntimeSandboxPolicy;
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
    crate::builtin_apps::validate_app_names(&config.apps)
        .map_err(|_| ActorError::Invalid("Invalid or unavailable App configuration".into()))?;
    config.workspace = canonical_path(&config.workspace)?;
    Ok(config)
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
