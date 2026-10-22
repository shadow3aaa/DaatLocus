//! Plain file operations with execution-time scope checks and whole-file freshness.
pub(crate) mod guarded_directory;
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::Mutex,
};

use daat_locus_macros::model_schema;
use miette::{Result, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    builtin_resources::BuiltinResource, model_protocol::AgentContentPart,
    sandbox::RuntimeSandboxPolicy,
};

pub const MAX_VIEW_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_READ_PAGE_BYTES: usize = 1024 * 1024;
static FILE_WRITES: Mutex<()> = Mutex::new(());

#[model_schema]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadFileArgs {
    pub path: String,
    pub start_line: Option<usize>,
    pub line_count: Option<usize>,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteFileArgs {
    pub path: String,
    pub content: String,
    /// The hash returned by read_file; null is allowed only when creating a file.
    pub expected_sha256: Option<String>,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditFileArgs {
    pub path: String,
    pub old_text: String,
    pub new_text: String,
    pub expected_sha256: String,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CopyFileArgs {
    pub source: String,
    pub destination: String,
    pub expected_sha256: Option<String>,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewImageArgs {
    pub path: String,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListDirectoryArgs {
    pub path: String,
    /// The last entry name returned by the previous page; null starts a listing.
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchTextArgs {
    pub path: String,
    pub query: String,
    pub case_sensitive: bool,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DirectoryEntry {
    pub name: String,
    pub path: String,
    pub kind: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct DirectoryPage {
    pub entries: Vec<DirectoryEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchMatch {
    pub path: String,
    pub line_number: usize,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub matches: Vec<SearchMatch>,
    pub limited: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReadFileResult {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub line_count: usize,
    pub total_lines: usize,
    pub reached_eof: bool,
    pub sha256: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileWriteResult {
    pub path: String,
    pub sha256: String,
    pub byte_count: usize,
    pub created: bool,
    pub change: Option<FileChange>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileChange {
    pub before_sha256: Option<String>,
    pub after_sha256: String,
    pub added_lines: usize,
    pub removed_lines: usize,
    pub lines: Vec<DiffLine>,
    pub patch: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiffLine {
    pub kind: &'static str,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    pub text: String,
}

fn file_change(before: &[u8], after: &[u8], created: bool) -> Option<FileChange> {
    if before.len() + after.len() > 2 * 1024 * 1024 {
        return None;
    }
    let (old, new) = (
        std::str::from_utf8(before).ok()?,
        std::str::from_utf8(after).ok()?,
    );
    if old.contains('\0')
        || new.contains('\0')
        || old.lines().count() + new.lines().count() > 20_000
    {
        return None;
    }
    let patch = diffy::create_patch(old, new);
    let mut change = FileChange {
        before_sha256: (!created).then(|| sha256(before)),
        after_sha256: sha256(after),
        added_lines: 0,
        removed_lines: 0,
        lines: vec![],
        patch: patch.to_string(),
    };
    for hunk in patch.hunks() {
        let mut old_line = hunk.old_range().start();
        let mut new_line = hunk.new_range().start();
        for line in hunk.lines() {
            let (kind, old_number, new_number, text) = match line {
                diffy::Line::Context(text) => {
                    let values = ("context", Some(old_line), Some(new_line), *text);
                    old_line += 1;
                    new_line += 1;
                    values
                }
                diffy::Line::Delete(text) => {
                    change.removed_lines += 1;
                    let values = ("remove", Some(old_line), None, *text);
                    old_line += 1;
                    values
                }
                diffy::Line::Insert(text) => {
                    change.added_lines += 1;
                    let values = ("add", None, Some(new_line), *text);
                    new_line += 1;
                    values
                }
            };
            change.lines.push(DiffLine {
                kind,
                old_line: old_number,
                new_line: new_number,
                text: text.trim_end_matches(['\r', '\n']).into(),
            });
        }
    }
    Some(change)
}

#[derive(Debug, Clone)]
pub struct ViewedImage {
    pub media_type: String,
    pub byte_count: usize,
    pub model_image: AgentContentPart,
}

fn authorized_path(
    cwd: &Path,
    policy: &RuntimeSandboxPolicy,
    value: &str,
    write: bool,
) -> Result<PathBuf> {
    if !write {
        let logical = unresolved_file_path(cwd, value)?;
        ensure_authorized_read(&logical, policy, "file target")?;
        let actual = resolve_read_file_path(cwd, policy, value)?;
        if actual != logical {
            ensure_authorized_read(&actual, policy, "file target")?;
        }
        return Ok(actual);
    }
    let logical = RuntimeSandboxPolicy::resolve_path(Path::new(value), Some(cwd));
    guarded_directory::local_path(&logical)
        .map_err(|_| miette!("remote or device file paths are unsupported"))?;
    policy.ensure_path_writable(&logical, "file target")?;
    let actual = resolve_file_path(cwd, value)?;
    policy.ensure_path_writable(&actual, "file target")?;
    Ok(actual)
}

fn unresolved_file_path(cwd: &Path, value: &str) -> Result<PathBuf> {
    if value.is_empty() || value.len() > 32768 {
        return Err(miette!("file path is empty"));
    }
    let path = Path::new(value);
    let logical = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    guarded_directory::local_path(&logical)
        .map_err(|_| miette!("remote or device file paths are unsupported"))?;
    Ok(logical)
}

/// Preserve the requested spelling until built-in resource validation has run.
/// Canonicalizing first would erase a traversal or an untrusted symlink alias.
pub(crate) fn resolve_read_file_path(
    cwd: &Path,
    policy: &RuntimeSandboxPolicy,
    value: &str,
) -> Result<PathBuf> {
    let logical = unresolved_file_path(cwd, value)?;
    if builtin_resource_for_path(&logical, policy)?.is_some() {
        return Ok(logical);
    }
    let actual = resolve_file_path(cwd, value)?;
    if policy
        .builtin_resources
        .iter()
        .any(|resource| resource.path == actual)
    {
        return Err(miette!(
            "file target changed to a built-in resource during resolution"
        ));
    }
    Ok(actual)
}

/// This is a file-read exemption, not a new filesystem root or process grant.
/// A registered target must validate even if the ordinary policy permits it.
pub(crate) fn ensure_authorized_read(
    path: &Path,
    policy: &RuntimeSandboxPolicy,
    label: &str,
) -> Result<()> {
    if let Some(resource) = builtin_resource_for_path(path, policy)? {
        read_builtin_bytes(resource, resource.content.len() as u64)?;
        Ok(())
    } else {
        policy.ensure_path_readable(path, label)
    }
}

pub(crate) fn read_requires_permission(path: &Path, policy: &RuntimeSandboxPolicy) -> Result<bool> {
    if let Some(resource) = builtin_resource_for_path(path, policy)? {
        read_builtin_bytes(resource, resource.content.len() as u64)?;
        Ok(false)
    } else {
        Ok(!policy.is_path_readable(path))
    }
}

fn builtin_resource_for_path<'a>(
    path: &Path,
    policy: &'a RuntimeSandboxPolicy,
) -> Result<Option<&'a BuiltinResource>> {
    guarded_directory::local_path(path)
        .map_err(|_| miette!("remote or device file paths are unsupported"))?;
    if policy.builtin_resources.is_empty() {
        return Ok(None);
    }
    let normalized = RuntimeSandboxPolicy::resolve_path(path, None);
    let resource = policy
        .builtin_resources
        .iter()
        .find(|resource| resource.path == normalized);
    // An alias of a built-in must not be canonicalized into the exemption,
    // including when the caller would otherwise have unrestricted access.
    let resource = match resource {
        Some(resource) => Some(resource),
        None => path.canonicalize().ok().and_then(|actual| {
            policy
                .builtin_resources
                .iter()
                .find(|resource| resource.path == actual)
        }),
    };
    if let Some(resource) = resource {
        if !path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
            || path != resource.path.as_path()
        {
            return Err(miette!(
                "built-in resources require their exact published path"
            ));
        }
        return Ok(Some(resource));
    }
    Ok(None)
}

fn validate_builtin_handle(
    file: &fs::File,
    directory: &guarded_directory::GuardedDirectory,
    resource: &BuiltinResource,
) -> Result<()> {
    let parent = resource
        .path
        .parent()
        .ok_or_else(|| miette!("built-in resource has no parent"))?;
    let name = resource
        .path
        .file_name()
        .ok_or_else(|| miette!("built-in resource has no filename"))?;
    if opened_file_path(file, &resource.path)? != resource.path
        || opened_file_path(directory.handle(), parent)? != parent
    {
        return Err(miette!("built-in resource path changed during access"));
    }
    // Rewalk every ancestor without following links, rather than comparing
    // against the earlier pinned directory alone after it may have moved.
    let current = guarded_directory::GuardedDirectory::open(parent, false)
        .map_err(|_| miette!("built-in resource ancestors changed during access"))?;
    if !guarded_directory::same_file(directory.handle(), current.handle())
        .map_err(|_| miette!("unable to verify built-in resource directory"))?
        || !current
            .matches_file(name, file)
            .map_err(|_| miette!("unable to verify built-in resource identity"))?
    {
        return Err(miette!("built-in resource identity changed during access"));
    }
    Ok(())
}

fn read_builtin_bytes(resource: &BuiltinResource, limit: u64) -> Result<Vec<u8>> {
    let expected_len = resource.content.len() as u64;
    if expected_len > limit {
        return Err(miette!("file exceeds the size limit"));
    }
    let parent = resource
        .path
        .parent()
        .ok_or_else(|| miette!("built-in resource has no parent"))?;
    let name = resource
        .path
        .file_name()
        .ok_or_else(|| miette!("built-in resource has no filename"))?;
    let directory = guarded_directory::GuardedDirectory::open(parent, false)
        .map_err(|_| miette!("unable to safely open built-in resource directory"))?;
    let mut file = directory
        .open_read(name)
        .map_err(|_| miette!("unable to safely open built-in resource file"))?;
    validate_builtin_handle(&file, &directory, resource)?;
    let metadata = file
        .metadata()
        .map_err(|_| miette!("unable to inspect built-in resource file"))?;
    if !metadata.is_file() || metadata.len() != expected_len {
        return Err(miette!("built-in resource content changed"));
    }
    let mut bytes = vec![0; resource.content.len()];
    file.read_exact(&mut bytes)
        .map_err(|_| miette!("unable to read complete built-in resource file"))?;
    validate_builtin_handle(&file, &directory, resource)?;
    if file
        .metadata()
        .map_err(|_| miette!("unable to inspect built-in resource file"))?
        .len()
        != expected_len
        || bytes.as_slice() != resource.content
    {
        return Err(miette!("built-in resource content changed"));
    }
    Ok(bytes)
}

pub(crate) fn resolve_file_path(cwd: &Path, value: &str) -> Result<PathBuf> {
    if value.is_empty() || value.len() > 32768 {
        return Err(miette!("file path is empty"));
    }
    let logical = RuntimeSandboxPolicy::resolve_path(Path::new(value), Some(cwd));
    guarded_directory::local_path(&logical)
        .map_err(|_| miette!("remote or device file paths are unsupported"))?;
    let mut ancestor = logical.clone();
    let mut suffix = Vec::new();
    loop {
        match fs::symlink_metadata(&ancestor) {
            Ok(_) => break,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let component = ancestor
                    .file_name()
                    .ok_or_else(|| miette!("invalid file path"))?;
                suffix.push(component.to_owned());
                if !ancestor.pop() {
                    return Err(miette!("invalid file path"));
                }
            }
            Err(_) => return Err(miette!("unable to inspect file path")),
        }
    }
    let mut actual = ancestor
        .canonicalize()
        .map_err(|_| miette!("unable to resolve file path"))?;
    guarded_directory::local_path(&actual)
        .map_err(|_| miette!("remote or device file paths are unsupported"))?;
    for component in suffix.iter().rev() {
        actual.push(component);
    }
    Ok(actual)
}

pub(crate) fn read_bytes(
    path: &Path,
    limit: u64,
    policy: &RuntimeSandboxPolicy,
) -> Result<Vec<u8>> {
    read_authorized_bytes(path, limit, policy)
}

/// Ensure a local directory exists without following replaced ancestors.
/// Existing read-only directories need read access; each newly created
/// component independently requires write authority.
pub fn ensure_directory(policy: &RuntimeSandboxPolicy, path: &Path) -> Result<()> {
    guarded_directory::local_path(path)
        .map_err(|_| miette!("remote or device directory paths are unsupported"))?;
    policy.ensure_path_readable(path, "workspace directory")?;
    let guard = guarded_directory::GuardedDirectory::open_with(path, true, |created| {
        policy
            .ensure_path_writable(created, "new directory")
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "directory creation is outside writable roots",
                )
            })
    })
    .map_err(|error| miette!("unable to safely create directory: {error}"))?;
    check_opened_file(guard.handle(), path, policy, false)
}

pub(crate) fn read_authorized_bytes(
    path: &Path,
    limit: u64,
    policy: &RuntimeSandboxPolicy,
) -> Result<Vec<u8>> {
    guarded_directory::local_path(path)
        .map_err(|_| miette!("remote or device file paths are unsupported"))?;
    if let Some(resource) = builtin_resource_for_path(path, policy)? {
        return read_builtin_bytes(resource, limit);
    }
    policy.ensure_path_readable(path, "file target")?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|_| miette!("unable to read file"))?;
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{FILE_TYPE_DISK, GetFileType};
        if unsafe { GetFileType(file.as_raw_handle().cast()) } != FILE_TYPE_DISK {
            return Err(miette!("file target is not on a local filesystem"));
        }
    }
    let actual = opened_file_path(&file, path)?;
    if policy
        .builtin_resources
        .iter()
        .any(|resource| resource.path == actual)
    {
        // Classification preceded opening. A racing alias must not redirect
        // the ordinary reader into a published target and skip verification.
        return Err(miette!(
            "file target changed to a built-in resource during access"
        ));
    }
    policy.ensure_path_readable(&actual, "opened file")?;
    let metadata = file
        .metadata()
        .map_err(|_| miette!("unable to inspect file"))?;
    if !metadata.is_file() {
        return Err(miette!("file target is not a regular file"));
    }
    if metadata.len() > limit {
        return Err(miette!("file exceeds the size limit"));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| miette!("unable to read file"))?;
    if bytes.len() as u64 > limit {
        return Err(miette!("file exceeds the size limit"));
    }
    Ok(bytes)
}

fn check_opened_file(
    file: &fs::File,
    path: &Path,
    policy: &RuntimeSandboxPolicy,
    write: bool,
) -> Result<()> {
    let actual = opened_file_path(file, path)?;
    if write {
        policy.ensure_path_writable(&actual, "opened file")
    } else {
        policy.ensure_path_readable(&actual, "opened file")
    }
}

fn opened_file_path(file: &fs::File, path: &Path) -> Result<PathBuf> {
    #[cfg(windows)]
    let actual = {
        use std::os::windows::{ffi::OsStringExt, io::AsRawHandle};
        use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;
        let mut buffer = vec![0u16; 32768];
        let length = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle().cast(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                0,
            )
        } as usize;
        if length == 0 || length >= buffer.len() {
            return Err(miette!("unable to verify opened file path"));
        }
        PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length]))
    };
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let actual = {
        use std::os::fd::AsRawFd;
        fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
            .map_err(|_| miette!("unable to verify opened file path"))?
    };
    #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
    let actual = {
        use std::os::unix::fs::MetadataExt;
        let actual = path
            .canonicalize()
            .map_err(|_| miette!("unable to verify opened file path"))?;
        let opened = file
            .metadata()
            .map_err(|_| miette!("unable to inspect opened file"))?;
        let current =
            fs::metadata(&actual).map_err(|_| miette!("unable to inspect current file"))?;
        if (opened.dev(), opened.ino()) != (current.dev(), current.ino()) {
            return Err(miette!("file identity changed during access"));
        }
        actual
    };
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (file, path);
        return Err(miette!("opened-file verification is unavailable"));
    }
    #[cfg(any(unix, windows))]
    {
        let _ = path;
        Ok(actual)
    }
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn list_directory(
    cwd: &Path,
    policy: &RuntimeSandboxPolicy,
    args: &ListDirectoryArgs,
) -> Result<DirectoryPage> {
    let path = authorized_path(cwd, policy, &args.path, false)?;
    let limit = args.limit.unwrap_or(100).clamp(1, 200);
    let directory = guarded_directory::GuardedDirectory::open(&path, false)
        .map_err(|_| miette!("unable to pin listing directory"))?;
    check_opened_file(directory.handle(), &path, policy, false)?;
    let names = directory
        .names()
        .map_err(|_| miette!("unable to enumerate the authorized directory"))?;
    let mut entries = BTreeMap::new();
    for name in names {
        let entry_path = path.join(&name);
        let Ok(name) = name.into_string() else {
            continue;
        };
        if args.cursor.as_ref().is_some_and(|cursor| &name <= cursor) {
            continue;
        }
        let Ok(actual) = authorized_path(cwd, policy, &entry_path.to_string_lossy(), false) else {
            continue;
        };
        let metadata =
            fs::metadata(&actual).map_err(|_| miette!("unable to inspect directory entry"))?;
        let kind = if metadata.is_dir() {
            "directory"
        } else if metadata.is_file() {
            "file"
        } else {
            "other"
        };
        entries.insert(
            name.clone(),
            DirectoryEntry {
                name,
                path: entry_path.display().to_string(),
                kind,
            },
        );
        if entries.len() > limit + 1 {
            entries.pop_last();
        }
    }
    let has_more = entries.len() > limit;
    if has_more {
        entries.pop_last();
    }
    let next_cursor = if has_more {
        entries.last_key_value().map(|(name, _)| name.clone())
    } else {
        None
    };
    Ok(DirectoryPage {
        entries: entries.into_values().collect(),
        next_cursor,
    })
}

pub fn search_text(
    cwd: &Path,
    policy: &RuntimeSandboxPolicy,
    args: &SearchTextArgs,
) -> Result<SearchResult> {
    if args.query.is_empty() {
        return Err(miette!("search query must not be empty"));
    }
    let root = authorized_path(cwd, policy, &args.path, false)?;
    let query = if args.case_sensitive {
        args.query.clone()
    } else {
        args.query.to_lowercase()
    };
    let limit = args.limit.unwrap_or(50).clamp(1, 200);
    let mut pending = vec![root];
    let mut visited = HashSet::new();
    let mut output_bytes = 0usize;
    let mut result = SearchResult {
        matches: Vec::new(),
        limited: false,
    };
    while let Some(path) = pending.pop() {
        let Ok(actual) = authorized_path(cwd, policy, &path.to_string_lossy(), false) else {
            continue;
        };
        if !visited.insert(actual.clone()) {
            continue;
        }
        let metadata =
            fs::metadata(&actual).map_err(|_| miette!("unable to inspect search path"))?;
        if metadata.is_dir() {
            let directory = guarded_directory::GuardedDirectory::open(&actual, false)
                .map_err(|_| miette!("unable to pin search directory"))?;
            check_opened_file(directory.handle(), &actual, policy, false)?;
            let names = directory
                .names()
                .map_err(|_| miette!("unable to enumerate authorized search directory"))?;
            let mut children = Vec::new();
            for name in names {
                children.push(actual.join(name));
            }
            children.sort();
            pending.extend(children.into_iter().rev());
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        let bytes = read_bytes(&actual, u64::MAX, policy)?;
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        for (index, line) in text.split_inclusive('\n').enumerate() {
            let found = if args.case_sensitive {
                line.contains(&query)
            } else {
                line.to_lowercase().contains(&query)
            };
            if !found {
                continue;
            }
            if result.matches.len() == limit
                || output_bytes.saturating_add(line.len()) > MAX_READ_PAGE_BYTES
            {
                result.limited = true;
                break;
            }
            output_bytes += line.len();
            result.matches.push(SearchMatch {
                path: path.display().to_string(),
                line_number: index + 1,
                text: line.into(),
            });
        }
        if result.limited {
            break;
        }
    }
    Ok(result)
}

pub fn read_file(
    cwd: &Path,
    policy: &RuntimeSandboxPolicy,
    args: &ReadFileArgs,
) -> Result<ReadFileResult> {
    let path = authorized_path(cwd, policy, &args.path, false)?;
    let bytes = read_bytes(&path, u64::MAX, policy)?;
    let digest = sha256(&bytes);
    let text = String::from_utf8(bytes).map_err(|_| miette!("file is not UTF-8 text"))?;
    let start_line = args.start_line.unwrap_or(1);
    let count = args.line_count.unwrap_or(80).clamp(1, 2000);
    let total_lines = text.split_inclusive('\n').count();
    if start_line == 0 || start_line > total_lines.max(1) {
        return Err(miette!("requested start_line is outside the file"));
    }
    let mut content = String::new();
    let mut line_count = 0;
    for line in text.split_inclusive('\n').skip(start_line - 1).take(count) {
        if content.len().saturating_add(line.len()) > MAX_READ_PAGE_BYTES {
            return Err(miette!("file page is too large; request fewer lines"));
        }
        content.push_str(line);
        line_count += 1;
    }
    let end_line = if line_count == 0 {
        0
    } else {
        start_line + line_count - 1
    };
    Ok(ReadFileResult {
        path: args.path.clone(),
        start_line,
        end_line,
        line_count,
        total_lines,
        reached_eof: end_line >= total_lines,
        sha256: digest,
        content,
    })
}

fn check_hash(bytes: &[u8], expected: &str) -> Result<()> {
    if sha256(bytes) != expected {
        return Err(miette!("file changed; read it again before writing"));
    }
    Ok(())
}

fn atomic_write(
    path: &Path,
    bytes: &[u8],
    created: bool,
    permissions: Option<fs::Permissions>,
    policy: &RuntimeSandboxPolicy,
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| miette!("file path has no parent"))?;
    policy.ensure_path_writable(parent, "atomic write directory")?;
    let directory = guarded_directory::GuardedDirectory::open_with(parent, true, |created| {
        policy
            .ensure_path_writable(created, "new file directory")
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "directory creation is outside writable roots",
                )
            })
    })
    .map_err(|error| miette!("unable to pin the file directory: {error}"))?;
    check_opened_file(directory.handle(), parent, policy, true)?;
    let temporary = parent.join(format!(".dl-write-{}", uuid::Uuid::new_v4()));
    let mut temporary_created = false;
    let result = (|| -> std::io::Result<()> {
        let temporary_name = temporary
            .file_name()
            .ok_or_else(|| std::io::Error::other("invalid temporary filename"))?;
        let target_name = path
            .file_name()
            .ok_or_else(|| std::io::Error::other("invalid target filename"))?;
        let mut file = directory.create_new(temporary_name)?;
        temporary_created = true;
        check_opened_file(&file, &temporary, policy, true).map_err(|error| {
            std::io::Error::other(format!("temporary file scope changed: {error:#}"))
        })?;
        file.write_all(bytes)?;
        file.sync_all()?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        drop(file);
        check_opened_file(directory.handle(), parent, policy, true)
            .map_err(|_| std::io::Error::other("opened directory scope changed"))?;
        if resolve_file_path(parent, &parent.to_string_lossy())
            .map_err(|_| std::io::Error::other("write directory changed"))?
            != parent
        {
            return Err(std::io::Error::other("write directory changed"));
        }
        policy
            .ensure_path_writable(path, "file target")
            .map_err(|_| std::io::Error::other("write scope changed"))?;
        if created {
            // Linking the complete file cannot replace a file created concurrently.
            directory.publish_new(temporary_name, target_name)?;
        } else {
            directory.rename(temporary_name, target_name)?;
        }
        directory.sync()?;
        Ok(())
    })();
    if result.is_err()
        && temporary_created
        && policy.is_path_writable(&temporary)
        && let Some(name) = temporary.file_name()
    {
        let _ = directory.remove(name);
    }
    result.map_err(|error| miette!("unable to commit file write: {error}"))
}

pub fn write_file(
    cwd: &Path,
    policy: &RuntimeSandboxPolicy,
    args: &WriteFileArgs,
) -> Result<FileWriteResult> {
    write_bytes(
        cwd,
        policy,
        &args.path,
        args.content.as_bytes(),
        args.expected_sha256.as_deref(),
    )
}

pub(crate) fn write_bytes(
    cwd: &Path,
    policy: &RuntimeSandboxPolicy,
    value: &str,
    bytes: &[u8],
    expected_sha256: Option<&str>,
) -> Result<FileWriteResult> {
    let _guard = FILE_WRITES
        .lock()
        .map_err(|_| miette!("file write lock is unavailable"))?;
    let path = authorized_path(cwd, policy, value, true)?;
    let (created, permissions, before) = match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            policy.ensure_path_readable(&path, "existing file")?;
            let expected = expected_sha256
                .ok_or_else(|| miette!("expected_sha256 is required for existing files"))?;
            let before = read_bytes(&path, 20 * 1024 * 1024, policy)?;
            check_hash(&before, expected)?;
            (false, Some(metadata.permissions()), before)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if expected_sha256.is_some() {
                return Err(miette!(
                    "file no longer exists; null expected_sha256 is required for a new file"
                ));
            }
            (true, None, Vec::new())
        }
        Err(_) => return Err(miette!("unable to inspect file")),
    };
    // Revalidate resolved parents immediately before filesystem mutation.
    let current = authorized_path(cwd, policy, value, true)?;
    if path != current {
        return Err(miette!("file path changed; retry after reading it again"));
    }
    let change = file_change(&before, bytes, created);
    atomic_write(&path, bytes, created, permissions, policy)?;
    Ok(FileWriteResult {
        path: value.into(),
        sha256: sha256(bytes),
        byte_count: bytes.len(),
        created,
        change,
    })
}

pub fn copy_file(
    cwd: &Path,
    policy: &RuntimeSandboxPolicy,
    args: &CopyFileArgs,
) -> Result<FileWriteResult> {
    let source = authorized_path(cwd, policy, &args.source, false)?;
    let bytes = read_authorized_bytes(&source, 20 * 1024 * 1024, policy)?;
    write_bytes(
        cwd,
        policy,
        &args.destination,
        &bytes,
        args.expected_sha256.as_deref(),
    )
}

pub fn edit_file(
    cwd: &Path,
    policy: &RuntimeSandboxPolicy,
    args: &EditFileArgs,
) -> Result<FileWriteResult> {
    if args.old_text.is_empty() {
        return Err(miette!("old_text must not be empty"));
    }
    let _guard = FILE_WRITES
        .lock()
        .map_err(|_| miette!("file write lock is unavailable"))?;
    let path = authorized_path(cwd, policy, &args.path, true)?;
    policy.ensure_path_readable(&path, "existing file")?;
    let bytes = read_bytes(&path, u64::MAX, policy)?;
    check_hash(&bytes, &args.expected_sha256)?;
    let mut content =
        String::from_utf8(bytes.clone()).map_err(|_| miette!("file is not UTF-8 text"))?;
    let mut matches = content
        .as_bytes()
        .windows(args.old_text.len())
        .enumerate()
        .filter(|(_, window)| *window == args.old_text.as_bytes())
        .map(|(offset, _)| offset);
    let offset = matches
        .next()
        .ok_or_else(|| miette!("old_text does not occur in the file"))?;
    if matches.next().is_some() {
        return Err(miette!(
            "old_text is ambiguous; include more surrounding text"
        ));
    }
    content.replace_range(offset..offset + args.old_text.len(), &args.new_text);
    let permissions = fs::metadata(&path)
        .map_err(|_| miette!("unable to inspect file"))?
        .permissions();
    if path != authorized_path(cwd, policy, &args.path, true)? {
        return Err(miette!("file path changed; retry after reading it again"));
    }
    let change = file_change(&bytes, content.as_bytes(), false);
    atomic_write(&path, content.as_bytes(), false, Some(permissions), policy)?;
    Ok(FileWriteResult {
        path: args.path.clone(),
        sha256: sha256(content.as_bytes()),
        byte_count: content.len(),
        created: false,
        change,
    })
}

/// image_state_dir is private infrastructure, never a model-supplied output path.
pub fn view_image(
    cwd: &Path,
    policy: &RuntimeSandboxPolicy,
    image_state_dir: &Path,
    supports_vision: bool,
    args: &ViewImageArgs,
) -> Result<ViewedImage> {
    if !supports_vision {
        return Err(miette!("the model does not support image inputs"));
    }
    let path = authorized_path(cwd, policy, &args.path, false)?;
    let bytes = read_bytes(&path, MAX_VIEW_IMAGE_BYTES, policy)?;
    let (media_type, extension) = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        ("image/png", "png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        ("image/jpeg", "jpg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        ("image/gif", "gif")
    } else if bytes.get(..4) == Some(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        ("image/webp", "webp")
    } else {
        return Err(miette!(
            "unsupported image type; use PNG, JPEG, GIF or WebP"
        ));
    };
    let snapshot = image_state_dir.join(format!("{}.{}", sha256(&bytes), extension));
    crate::persistence::write_bytes_atomic_sync(
        &snapshot,
        &bytes,
        crate::persistence::PersistenceFileMode::Private,
    )
    .map_err(|_| miette!("unable to store image input"))?;
    Ok(ViewedImage {
        media_type: media_type.into(),
        byte_count: bytes.len(),
        model_image: AgentContentPart::Image {
            path: snapshot.display().to_string(),
            media_type: media_type.into(),
            description: Some(args.path.clone()),
        },
    })
}

#[cfg(test)]
mod tests {
    const BUILTIN_BYTES: &[u8] = b"trusted built-in instructions\n";

    fn builtin_fixture() -> (tempfile::TempDir, RuntimeSandboxPolicy, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let path = root.join("skills/computer-use/SKILL.md");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, BUILTIN_BYTES).unwrap();
        let mut policy = RuntimeSandboxPolicy::disabled();
        policy.filesystem.full_disk_read = false;
        policy.filesystem.full_disk_write = false;
        policy.filesystem.readable_roots.clear();
        policy.filesystem.writable_roots.clear();
        policy.filesystem.deny_read_paths = vec![root.clone()];
        policy.filesystem.deny_write_paths = vec![root];
        policy.builtin_resources = vec![BuiltinResource {
            path: path.clone(),
            content: BUILTIN_BYTES,
        }];
        (directory, policy, path)
    }

    #[test]
    fn builtins_are_exact_verified_reads_not_filesystem_roots() {
        let (directory, policy, path) = builtin_fixture();
        assert!(!policy.is_path_readable(&path));
        assert!(!policy.is_path_writable(&path));
        assert!(!read_requires_permission(&path, &policy).unwrap());
        ensure_authorized_read(&path, &policy, "built-in skill").unwrap();
        let result = read_file(
            directory.path(),
            &policy,
            &ReadFileArgs {
                path: path.display().to_string(),
                ..read_args()
            },
        )
        .unwrap();
        assert_eq!(result.content.as_bytes(), BUILTIN_BYTES);
        assert_eq!(result.sha256, sha256(BUILTIN_BYTES));
        assert!(read_authorized_bytes(&path, BUILTIN_BYTES.len() as u64 - 1, &policy).is_err());

        let extra = path.with_file_name("extra.md");
        fs::write(&extra, BUILTIN_BYTES).unwrap();
        assert!(read_requires_permission(&extra, &policy).unwrap());
        assert!(read_authorized_bytes(&extra, u64::MAX, &policy).is_err());
        assert!(
            list_directory(
                directory.path(),
                &policy,
                &ListDirectoryArgs {
                    path: path.parent().unwrap().display().to_string(),
                    cursor: None,
                    limit: None,
                },
            )
            .is_err()
        );
    }

    #[test]
    fn tampered_builtins_never_fall_back_to_full_access() {
        let (_directory, mut policy, path) = builtin_fixture();
        policy.filesystem.full_disk_read = true;
        policy.filesystem.deny_read_paths.clear();
        let mut same_length = BUILTIN_BYTES.to_vec();
        same_length[0] = b'X';
        for bytes in [Vec::new(), same_length, [BUILTIN_BYTES, b"extra"].concat()] {
            fs::write(&path, bytes).unwrap();
            assert!(policy.is_path_readable(&path));
            assert!(read_authorized_bytes(&path, u64::MAX, &policy).is_err());
            assert!(ensure_authorized_read(&path, &policy, "skill").is_err());
            assert!(read_requires_permission(&path, &policy).is_err());
        }
    }

    #[test]
    fn builtin_traversal_is_rejected_before_normalization() {
        let (directory, policy, path) = builtin_fixture();
        let parent = path.parent().unwrap();
        fs::create_dir(parent.join("child")).unwrap();
        let traversal = parent.join("child/../SKILL.md");
        assert!(
            resolve_read_file_path(directory.path(), &policy, &traversal.to_string_lossy())
                .is_err()
        );
        assert!(read_authorized_bytes(&traversal, u64::MAX, &policy).is_err());
        assert!(
            read_file(
                directory.path(),
                &policy,
                &ReadFileArgs {
                    path: traversal.display().to_string(),
                    ..read_args()
                },
            )
            .is_err()
        );
    }

    #[test]
    fn builtin_copy_source_does_not_grant_write_edit_or_copy_target_access() {
        let (_directory, policy, path) = builtin_fixture();
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path().canonicalize().unwrap();
        let mut policy = policy;
        policy.filesystem.readable_roots = vec![root.clone()];
        policy.filesystem.writable_roots = vec![crate::sandbox::WritableRoot {
            root: root.clone(),
            read_only_subpaths: vec![],
        }];
        let copied = root.join("copied.md");
        copy_file(
            &root,
            &policy,
            &CopyFileArgs {
                source: path.display().to_string(),
                destination: copied.display().to_string(),
                expected_sha256: None,
            },
        )
        .unwrap();
        assert_eq!(fs::read(&copied).unwrap(), BUILTIN_BYTES);
        policy.filesystem.full_disk_write = true;
        assert!(!policy.is_path_writable(&path));
        assert!(
            write_file(
                &root,
                &policy,
                &WriteFileArgs {
                    path: path.display().to_string(),
                    content: "replacement".into(),
                    expected_sha256: Some(sha256(BUILTIN_BYTES)),
                },
            )
            .is_err()
        );
        assert!(
            edit_file(
                &root,
                &policy,
                &EditFileArgs {
                    path: path.display().to_string(),
                    old_text: "trusted".into(),
                    new_text: "modified".into(),
                    expected_sha256: sha256(BUILTIN_BYTES),
                },
            )
            .is_err()
        );
        assert!(
            copy_file(
                &root,
                &policy,
                &CopyFileArgs {
                    source: copied.display().to_string(),
                    destination: path.display().to_string(),
                    expected_sha256: Some(sha256(BUILTIN_BYTES)),
                },
            )
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), BUILTIN_BYTES);
    }

    #[cfg(unix)]
    #[test]
    fn builtin_symlinks_aliases_and_replaced_ancestors_are_rejected() {
        let (directory, mut policy, path) = builtin_fixture();
        // Full access cannot turn an alias into a trusted published resource.
        policy.filesystem.full_disk_read = true;
        policy.filesystem.deny_read_paths.clear();
        let alias = directory.path().join("alias.md");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        assert!(read_authorized_bytes(&alias, u64::MAX, &policy).is_err());
        assert!(
            resolve_read_file_path(directory.path(), &policy, &alias.to_string_lossy()).is_err()
        );

        let moved_file = directory.path().join("moved.md");
        fs::rename(&path, &moved_file).unwrap();
        std::os::unix::fs::symlink(&moved_file, &path).unwrap();
        assert!(read_authorized_bytes(&path, u64::MAX, &policy).is_err());
        fs::remove_file(&path).unwrap();
        fs::rename(&moved_file, &path).unwrap();

        let parent = path.parent().unwrap();
        let guard = guarded_directory::GuardedDirectory::open(parent, false).unwrap();
        let opened = guard.open_read(path.file_name().unwrap()).unwrap();
        let moved_parent = directory.path().join("moved-parent");
        fs::rename(parent, &moved_parent).unwrap();
        fs::create_dir(parent).unwrap();
        fs::write(&path, BUILTIN_BYTES).unwrap();
        assert!(validate_builtin_handle(&opened, &guard, &policy.builtin_resources[0]).is_err());
        fs::remove_file(&path).unwrap();
        fs::remove_dir(parent).unwrap();
        std::os::unix::fs::symlink(&moved_parent, parent).unwrap();
        assert!(read_authorized_bytes(&path, u64::MAX, &policy).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn builtin_junction_ancestor_is_rejected() {
        let (directory, policy, path) = builtin_fixture();
        let parent = path.parent().unwrap();
        let moved = directory.path().join("moved-parent");
        fs::rename(parent, &moved).unwrap();
        let output = std::process::Command::new("cmd.exe")
            .args([
                "/D",
                "/C",
                r#"mklink /J "%DL_TEST_JUNCTION%" "%DL_TEST_TARGET%""#,
            ])
            .env(
                "DL_TEST_JUNCTION",
                directory.path().join("skills/computer-use"),
            )
            .env("DL_TEST_TARGET", &moved)
            .output()
            .expect("Windows junction regression test requires cmd.exe");
        assert!(
            output.status.success(),
            "unable to create junction: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(read_authorized_bytes(&path, u64::MAX, &policy).is_err());
    }

    #[test]
    fn actual_text_changes_preserve_line_numbers_hashes_and_bounds() {
        let before = b"first\nold\nlast\n";
        let after = b"first\nnew\nextra\nlast\n";
        let change = super::file_change(before, after, false).unwrap();
        assert_eq!((change.added_lines, change.removed_lines), (2, 1));
        assert_eq!(
            change.before_sha256.as_deref(),
            Some(super::sha256(before).as_str())
        );
        assert_eq!(change.after_sha256, super::sha256(after));
        let removed = change
            .lines
            .iter()
            .find(|line| line.kind == "remove")
            .unwrap();
        assert_eq!(
            (removed.old_line, removed.new_line, removed.text.as_str()),
            (Some(2), None, "old")
        );
        let added = change.lines.iter().find(|line| line.kind == "add").unwrap();
        assert_eq!(
            (added.old_line, added.new_line, added.text.as_str()),
            (None, Some(2), "new")
        );
        let created = super::file_change(b"", b"new\n", true).unwrap();
        assert!(created.before_sha256.is_none());
        assert_eq!(created.lines[0].new_line, Some(1));
        assert!(super::file_change(b"binary\0", b"changed", false).is_none());
        assert!(super::file_change(b"", &vec![b'x'; 2 * 1024 * 1024 + 1], true).is_none());
        let same = super::file_change(before, before, false).unwrap();
        assert_eq!((same.added_lines, same.removed_lines), (0, 0));
        assert!(same.lines.is_empty());
    }
    use super::*;

    fn read_args() -> ReadFileArgs {
        ReadFileArgs {
            path: "a.txt".into(),
            start_line: None,
            line_count: None,
        }
    }

    #[test]
    fn pages_preserve_plaintext_and_hash_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "one\r\ntwo\nthree").unwrap();
        let page = read_file(
            dir.path(),
            &RuntimeSandboxPolicy::disabled(),
            &ReadFileArgs {
                start_line: Some(2),
                line_count: Some(1),
                ..read_args()
            },
        )
        .unwrap();
        assert_eq!(page.content, "two\n");
        assert_eq!(page.total_lines, 3);
        assert!(!page.reached_eof);
        assert_eq!(page.sha256, sha256(b"one\r\ntwo\nthree"));
    }

    #[test]
    fn creation_overwrite_and_edits_require_fresh_hashes() {
        let dir = tempfile::tempdir().unwrap();
        let policy = RuntimeSandboxPolicy::disabled();
        let mut args = WriteFileArgs {
            path: "a.txt".into(),
            content: "aaa".into(),
            expected_sha256: None,
        };
        let created = write_file(dir.path(), &policy, &args).unwrap();
        assert!(created.created);
        assert!(write_file(dir.path(), &policy, &args).is_err());
        let mut edit = EditFileArgs {
            path: "a.txt".into(),
            old_text: "aa".into(),
            new_text: "b".into(),
            expected_sha256: created.sha256.clone(),
        };
        assert!(
            edit_file(dir.path(), &policy, &edit)
                .unwrap_err()
                .to_string()
                .contains("ambiguous")
        );
        edit.old_text = "aaa".into();
        edit_file(dir.path(), &policy, &edit).unwrap();
        assert!(edit_file(dir.path(), &policy, &edit).is_err());
        args.expected_sha256 = Some(created.sha256);
        assert!(write_file(dir.path(), &policy, &args).is_err());
        args.expected_sha256 = Some(sha256(b"b"));
        args.content = "done".into();
        write_file(dir.path(), &policy, &args).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "done"
        );
    }

    #[test]
    fn ensure_directory_preserves_readonly_and_checks_each_missing_parent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let mut policy = RuntimeSandboxPolicy::disabled();
        policy.filesystem.full_disk_read = false;
        policy.filesystem.full_disk_write = false;
        policy.filesystem.readable_roots = vec![root.clone()];
        policy.filesystem.writable_roots.clear();
        ensure_directory(&policy, &root).unwrap();
        assert!(ensure_directory(&policy, &root.join("blocked/new")).is_err());
        assert!(!root.join("blocked").exists());
        policy
            .filesystem
            .writable_roots
            .push(crate::sandbox::WritableRoot {
                root: root.clone(),
                read_only_subpaths: vec![],
            });
        ensure_directory(&policy, &root.join("allowed/new")).unwrap();
        assert!(root.join("allowed/new").is_dir());
        policy.filesystem.readable_roots = vec![root.join("ungranted/parent/target")];
        policy.filesystem.writable_roots = vec![crate::sandbox::WritableRoot {
            root: root.join("ungranted/parent/target"),
            read_only_subpaths: vec![],
        }];
        assert!(ensure_directory(&policy, &root.join("ungranted/parent/target")).is_err());
        assert!(!root.join("ungranted").exists());
    }

    #[test]
    fn readonly_and_protected_paths_are_denied_before_access() {
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("private");
        fs::create_dir(&private).unwrap();
        fs::write(private.join("secret.txt"), "private").unwrap();
        let mut policy = RuntimeSandboxPolicy::disabled();
        policy.filesystem.full_disk_write = false;
        policy.filesystem.writable_roots.clear();
        let args = WriteFileArgs {
            path: "new.txt".into(),
            content: "no".into(),
            expected_sha256: None,
        };
        assert!(write_file(dir.path(), &policy, &args).is_err());
        policy.filesystem.deny_read_paths.push(private);
        assert!(
            read_file(
                dir.path(),
                &policy,
                &ReadFileArgs {
                    path: "private/secret.txt".into(),
                    ..read_args()
                }
            )
            .is_err()
        );
        assert!(!dir.path().join("new.txt").exists());
    }

    #[test]
    #[cfg_attr(windows, ignore = "requires Windows symlink creation privileges")]
    fn symlink_escape_is_denied() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("a.txt"), "outside").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(outside.path(), dir.path().join("link"))
            .expect("enable symlink creation privileges before running this test");
        let mut policy = RuntimeSandboxPolicy::disabled();
        policy.filesystem.full_disk_read = false;
        policy.filesystem.readable_roots = vec![dir.path().into()];
        assert!(
            read_file(
                dir.path(),
                &policy,
                &ReadFileArgs {
                    path: "link/a.txt".into(),
                    ..read_args()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn input_schemas_are_closed_and_nullable_values_are_required() {
        for schema in [
            crate::schema_utils::model_schema_for::<ReadFileArgs>(),
            crate::schema_utils::model_schema_for::<WriteFileArgs>(),
            crate::schema_utils::model_schema_for::<EditFileArgs>(),
            crate::schema_utils::model_schema_for::<ListDirectoryArgs>(),
            crate::schema_utils::model_schema_for::<SearchTextArgs>(),
        ] {
            crate::schema_utils::validate_model_facing_schema(&schema).unwrap();
            assert_eq!(
                schema["properties"].as_object().unwrap().len(),
                schema["required"].as_array().unwrap().len()
            );
        }
    }

    #[test]
    fn search_reaches_matches_after_the_former_file_scan_quota() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..2001 {
            fs::write(dir.path().join(format!("{index:04}.txt")), "no match").unwrap();
        }
        fs::write(dir.path().join("last.txt"), "needle after all files\n").unwrap();
        let result = search_text(
            dir.path(),
            &RuntimeSandboxPolicy::disabled(),
            &SearchTextArgs {
                path: ".".into(),
                query: "needle".into(),
                case_sensitive: true,
                limit: Some(1),
            },
        )
        .unwrap();
        assert!(!result.limited);
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].text, "needle after all files\n");
        assert!(result.matches[0].path.ends_with("last.txt"));
    }

    #[test]
    fn listing_and_search_skip_denied_contents_and_page_by_name() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "Needle\r\nsecond\n").unwrap();
        fs::write(dir.path().join("b.txt"), "needle\n").unwrap();
        fs::create_dir(dir.path().join("private")).unwrap();
        fs::write(dir.path().join("private/secret.txt"), "needle secret").unwrap();
        let mut policy = RuntimeSandboxPolicy::disabled();
        policy
            .filesystem
            .deny_read_paths
            .push(dir.path().join("private"));
        let mut args = ListDirectoryArgs {
            path: ".".into(),
            cursor: None,
            limit: Some(1),
        };
        let first = list_directory(dir.path(), &policy, &args).unwrap();
        assert_eq!(first.entries[0].name, "a.txt");
        args.cursor = first.next_cursor;
        let second = list_directory(dir.path(), &policy, &args).unwrap();
        assert_eq!(second.entries[0].name, "b.txt");
        assert!(second.next_cursor.is_none());
        let found = search_text(
            dir.path(),
            &policy,
            &SearchTextArgs {
                path: ".".into(),
                query: "needle".into(),
                case_sensitive: false,
                limit: None,
            },
        )
        .unwrap();
        assert_eq!(found.matches.len(), 2);
        assert_eq!(found.matches[0].text, "Needle\r\n");
        assert!(
            !found
                .matches
                .iter()
                .any(|item| item.text.contains("secret"))
        );
        assert!(!found.limited);
    }

    #[test]
    fn model_image_uses_an_immutable_private_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let original = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;";
        fs::write(dir.path().join("image.gif"), original).unwrap();
        let image = view_image(
            dir.path(),
            &RuntimeSandboxPolicy::disabled(),
            &dir.path().join("private/images"),
            true,
            &ViewImageArgs {
                path: "image.gif".into(),
            },
        )
        .unwrap();
        let AgentContentPart::Image {
            path, media_type, ..
        } = image.model_image
        else {
            panic!("expected model image");
        };
        assert_eq!(media_type, "image/gif");
        fs::write(dir.path().join("image.gif"), "changed").unwrap();
        assert_eq!(fs::read(path).unwrap(), original);
    }
}
