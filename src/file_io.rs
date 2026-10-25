//! Plain file operations with execution-time scope checks and whole-file freshness.
pub(crate) mod guarded_directory;
use std::{
    borrow::Cow,
    collections::BTreeMap,
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
/// A search never descends further than this many directory levels below its root.
const MAX_SEARCH_DEPTH: usize = 32;
static FILE_WRITES: Mutex<()> = Mutex::new(());

#[model_schema]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadFileArgs {
    pub path: String,
    /// One-based first line; null starts at line 1. Zero is invalid.
    pub start_line: Option<usize>,
    /// Number of lines to read; null uses the default page size.
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
pub struct EditFileEntry {
    pub old_text: String,
    pub new_text: String,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditFileArgs {
    pub path: String,
    /// Single-edit form (mutually exclusive with `edits`): the exact unique
    /// text to replace, matched with LF/CRLF equivalence.
    pub old_text: Option<String>,
    /// Single-edit form replacement text (required when old_text is given).
    pub new_text: Option<String>,
    /// Ordered multi-edit form (mutually exclusive with `old_text` and
    /// `new_text`): each entry must match uniquely in the progressively
    /// updated text; a failing entry aborts the whole batch before any write.
    pub edits: Option<Vec<EditFileEntry>>,
    pub expected_sha256: String,
}

impl EditFileArgs {
    /// The ordered edits to apply, resolving the single pair to a batch of one.
    fn resolved_edits(&self) -> Result<Vec<EditFileEntry>> {
        match (&self.edits, &self.old_text, &self.new_text) {
            (Some(edits), None, None) => {
                if edits.is_empty() {
                    return Err(miette!("edits must not be empty"));
                }
                Ok(edits.clone())
            }
            (Some(_), _, _) => Err(miette!(
                "edits cannot be combined with old_text or new_text"
            )),
            (None, Some(old_text), Some(new_text)) => Ok(vec![EditFileEntry {
                old_text: old_text.clone(),
                new_text: new_text.clone(),
            }]),
            (None, Some(_), None) | (None, None, Some(_)) => {
                Err(miette!("old_text and new_text must be provided together"))
            }
            (None, None, None) => Err(miette!(
                "provide either old_text and new_text or a non-empty edits array"
            )),
        }
    }
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
    /// Directory levels below `path` to descend into. Required; 1 searches only the entries
    /// directly inside `path`, and the maximum is 32.
    pub depth: usize,
    /// Also walk paths excluded by repository/global Git rules and .git metadata. Defaults to false.
    pub include_ignored: Option<bool>,
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

/// Restore a Windows extended-length prefix that separator normalization
/// flattened into `//?/...`. `std::path` reads that spelling as a UNC prefix,
/// so the files layer would reject the target as remote instead of opening the
/// local volume it names.
#[cfg(windows)]
fn normalize_verbatim_prefix(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let is_separator = |byte: u8| byte == b'\\' || byte == b'/';
    // `\\?\` disables OS separator translation, so its remainder must use
    // backslashes even when the caller sent forward slashes.
    if bytes.len() >= 4
        && is_separator(bytes[0])
        && is_separator(bytes[1])
        && bytes[2] == b'?'
        && is_separator(bytes[3])
    {
        let mut restored = String::with_capacity(value.len());
        restored.push_str(r"\\?\");
        restored.push_str(&value[4..].replace('/', "\\"));
        return Some(restored);
    }
    None
}

/// Paths arrive as model- or client-supplied text, so a previously mangled
/// extended-length prefix is repaired before any policy check sees it.
fn normalize_path_input(value: &str) -> Cow<'_, str> {
    #[cfg(windows)]
    if let Some(restored) = normalize_verbatim_prefix(value) {
        return Cow::Owned(restored);
    }
    Cow::Borrowed(value)
}

fn unresolved_file_path(cwd: &Path, value: &str) -> Result<PathBuf> {
    if value.is_empty() || value.len() > 32768 {
        return Err(miette!("file path is empty"));
    }
    let value = normalize_path_input(value);
    let path = Path::new(value.as_ref());
    let logical = if path.is_absolute() {
        path.to_path_buf()
    } else {
        // Preserve dot components even with a verbatim Windows cwd. The
        // manifest exemption must see the spelling the caller supplied.
        let mut joined = cwd.as_os_str().to_os_string();
        joined.push(std::path::MAIN_SEPARATOR_STR);
        joined.push(path);
        PathBuf::from(joined)
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
    if policy.is_path_readable(&logical) {
        resolve_file_path(cwd, value)
    } else {
        // Preserve the spelling for trusted-read validation. A missing or
        // invalid exemption can still request an ordinary canonical scope.
        Ok(logical)
    }
}

/// This is a file-read exemption, not a new filesystem root or process grant.
/// Ordinary scoped reads are independent of this verified manifest exemption.
pub(crate) fn ensure_authorized_read(
    path: &Path,
    policy: &RuntimeSandboxPolicy,
    label: &str,
) -> Result<()> {
    guarded_directory::local_path(path)
        .map_err(|_| miette!("remote or device file paths are unsupported"))?;
    if policy.is_path_readable(path) {
        return Ok(());
    }
    if let Some(resource) = builtin_resource_for_path(path, policy)? {
        read_builtin_bytes(resource, resource.content.len() as u64)?;
        Ok(())
    } else {
        policy.ensure_path_readable(path, label)
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
    // An alias of a built-in must not be canonicalized into the exemption.
    // Independently authorized ordinary reads do not use this classification.
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

pub(crate) fn read_builtin_bytes(resource: &BuiltinResource, limit: u64) -> Result<Vec<u8>> {
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
    let value = normalize_path_input(value);
    let logical = RuntimeSandboxPolicy::resolve_path(Path::new(value.as_ref()), Some(cwd));
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
    if !policy.is_path_readable(path)
        && let Some(resource) = builtin_resource_for_path(path, policy)?
    {
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

/// Read at most the leading `limit` bytes of a file for bounded inspection.
/// Unlike [`read_bytes`], oversized files are truncated instead of rejected.
pub(crate) fn read_prefix(
    path: &Path,
    limit: u64,
    policy: &RuntimeSandboxPolicy,
) -> Result<Vec<u8>> {
    guarded_directory::local_path(path)
        .map_err(|_| miette!("remote or device file paths are unsupported"))?;
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
    policy.ensure_path_readable(&actual, "opened file")?;
    let metadata = file
        .metadata()
        .map_err(|_| miette!("unable to inspect file"))?;
    if !metadata.is_file() {
        return Err(miette!("file target is not a regular file"));
    }
    let mut bytes = Vec::with_capacity(metadata.len().min(limit) as usize);
    file.take(limit)
        .read_to_end(&mut bytes)
        .map_err(|_| miette!("unable to read file"))?;
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
    search_text_with_global_rules(cwd, policy, args, true)
}

fn search_text_with_global_rules(
    cwd: &Path,
    policy: &RuntimeSandboxPolicy,
    args: &SearchTextArgs,
    global_rules: bool,
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
    if args.depth == 0 || args.depth > MAX_SEARCH_DEPTH {
        return Err(miette!(
            "search depth must be between 1 and {MAX_SEARCH_DEPTH}"
        ));
    }
    let mut output_bytes = 0usize;
    let mut result = SearchResult {
        matches: Vec::new(),
        limited: false,
    };
    let use_ignore_files = !args.include_ignored.unwrap_or(false);
    let filter_cwd = cwd.to_path_buf();
    let filter_policy = policy.clone();
    let mut walker = ignore::WalkBuilder::new(&root);
    walker
        .standard_filters(false)
        .parents(use_ignore_files)
        .git_ignore(use_ignore_files)
        .git_global(use_ignore_files && global_rules)
        .require_git(true)
        .follow_links(false)
        .max_depth(Some(args.depth))
        .sort_by_file_name(|left, right| left.cmp(right))
        .filter_entry(move |entry| {
            if use_ignore_files && entry.file_name() == ".git" {
                return false;
            }
            // Rule discovery is internal metadata I/O. Admission to the search
            // still checks Actor authority before descending into a directory.
            let Ok(actual) = authorized_path(
                &filter_cwd,
                &filter_policy,
                &entry.path().to_string_lossy(),
                false,
            ) else {
                return false;
            };
            if entry.file_type().is_some_and(|kind| kind.is_dir()) {
                return guarded_directory::GuardedDirectory::open(&actual, false).is_ok_and(
                    |directory| {
                        check_opened_file(directory.handle(), &actual, &filter_policy, false)
                            .is_ok()
                    },
                );
            }
            true
        });
    for entry in walker.build() {
        let entry = entry.map_err(|_| miette!("unable to traverse search directory"))?;
        // Library diagnostics can contain private rule text or ancestor paths.
        if entry.error().is_some() {
            return Err(miette!("unable to load search ignore rules"));
        }
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let Ok(actual) = authorized_path(cwd, policy, &entry.path().to_string_lossy(), false)
        else {
            continue;
        };
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
                path: actual.display().to_string(),
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

/// The line ending a matched region of text actually uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditLineEnding {
    Lf,
    Crlf,
}

/// Collapses CRLF line endings to LF for line-ending-equivalent matching.
/// Removing an ASCII carriage return cannot break a multi-byte sequence.
fn normalize_line_endings_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// The original byte offset that corresponds to `target` normalized (LF)
/// byte offset after CRLF collapsing, walking the text without materializing
/// a per-byte offset table.
fn original_offset_for_normalized(content: &str, target: usize) -> usize {
    let bytes = content.as_bytes();
    let mut normalized = 0usize;
    let mut index = 0usize;
    while index < bytes.len() && normalized < target {
        if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
            normalized += 1;
            index += 2;
        } else {
            normalized += 1;
            index += 1;
        }
    }
    index
}

/// The first line ending in `text`, if it contains any.
fn first_line_ending(text: &str) -> Option<EditLineEnding> {
    let bytes = text.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            return Some(if index > 0 && bytes[index - 1] == b'\r' {
                EditLineEnding::Crlf
            } else {
                EditLineEnding::Lf
            });
        }
    }
    None
}

/// The line ending a text uses most; a tie keeps the first one seen.
fn dominant_line_ending(text: &str) -> Option<EditLineEnding> {
    let bytes = text.as_bytes();
    let mut crlf = 0usize;
    let mut lf = 0usize;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            if index > 0 && bytes[index - 1] == b'\r' {
                crlf += 1;
            } else {
                lf += 1;
            }
        }
    }
    if crlf == 0 && lf == 0 {
        None
    } else if crlf == lf {
        first_line_ending(text)
    } else if crlf > lf {
        Some(EditLineEnding::Crlf)
    } else {
        Some(EditLineEnding::Lf)
    }
}

/// Rewrites every line ending in `text` to `ending`.
fn rewrite_line_endings(text: &str, ending: EditLineEnding) -> String {
    let normalized = text.replace("\r\n", "\n");
    match ending {
        EditLineEnding::Lf => normalized,
        EditLineEnding::Crlf => normalized.replace('\n', "\r\n"),
    }
}

/// Replaces the single occurrence of `old_text` while treating LF and CRLF
/// as equivalent, then writes `new_text` with the line endings the matched
/// region actually uses (a region without line endings falls back to the
/// file's dominant ending; a file without any keeps `new_text` unchanged).
/// Bytes outside the match are preserved exactly.
fn apply_line_ending_tolerant_edit(
    content: &str,
    old_text: &str,
    new_text: &str,
) -> Result<String> {
    if old_text.is_empty() {
        return Err(miette!("old_text must not be empty"));
    }
    let normalized = normalize_line_endings_to_lf(content);
    let old_normalized = normalize_line_endings_to_lf(old_text);
    let mut matches = normalized
        .as_bytes()
        .windows(old_normalized.len())
        .enumerate()
        .filter(|(_, window)| *window == old_normalized.as_bytes())
        .map(|(offset, _)| offset);
    let start = matches
        .next()
        .ok_or_else(|| miette!("old_text does not occur in the file"))?;
    if matches.next().is_some() {
        return Err(miette!(
            "old_text is ambiguous; include more surrounding text"
        ));
    }
    let end = start + old_normalized.len();
    let (original_start, original_end) = (
        original_offset_for_normalized(content, start),
        original_offset_for_normalized(content, end),
    );
    let matched = &content[original_start..original_end];
    let ending = first_line_ending(matched).or_else(|| dominant_line_ending(content));
    let replacement = match ending {
        Some(ending) => rewrite_line_endings(new_text, ending),
        None => new_text.to_owned(),
    };
    let mut edited =
        String::with_capacity(content.len() - (original_end - original_start) + replacement.len());
    edited.push_str(&content[..original_start]);
    edited.push_str(&replacement);
    edited.push_str(&content[original_end..]);
    Ok(edited)
}

/// Applies every edit in order on the progressively updated text. Each edit
/// must match uniquely; the first failing edit aborts the whole batch with
/// its position, and nothing is written.
fn apply_edit_batch(content: &str, edits: &[EditFileEntry]) -> Result<String> {
    if edits.is_empty() {
        return Err(miette!("edits must not be empty"));
    }
    let mut edited = content.to_owned();
    let total = edits.len();
    for (index, edit) in edits.iter().enumerate() {
        edited = apply_line_ending_tolerant_edit(&edited, &edit.old_text, &edit.new_text)
            .map_err(|error| miette!("edit {} of {} failed: {error}", index + 1, total))?;
    }
    Ok(edited)
}

/// Applies one ordered batch of text edits with a single write: either the
/// single `old_text`/`new_text` pair or the `edits` array. Every edit must
/// match uniquely (LF/CRLF equivalent) in the progressively updated text and
/// is written with the matched region's line endings; a failing edit aborts
/// the whole batch before anything is written.
pub fn edit_file(
    cwd: &Path,
    policy: &RuntimeSandboxPolicy,
    args: &EditFileArgs,
) -> Result<FileWriteResult> {
    let edits = args.resolved_edits()?;
    let _guard = FILE_WRITES
        .lock()
        .map_err(|_| miette!("file write lock is unavailable"))?;
    let path = authorized_path(cwd, policy, &args.path, true)?;
    policy.ensure_path_readable(&path, "existing file")?;
    let bytes = read_bytes(&path, u64::MAX, policy)?;
    check_hash(&bytes, &args.expected_sha256)?;
    let content =
        String::from_utf8(bytes.clone()).map_err(|_| miette!("file is not UTF-8 text"))?;
    let content = apply_edit_batch(&content, &edits)?;
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

    /// Fixtures that exercise missing-scope, built-in-exemption and
    /// search boundaries must stay outside the system temporary
    /// directories: the baseline temporary authorization would otherwise
    /// mask the boundaries these tests verify.
    fn non_temporary_scratch() -> tempfile::TempDir {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
        std::fs::create_dir_all(&base).unwrap();
        tempfile::Builder::new()
            .prefix("notemp-file-io-")
            .tempdir_in(&base)
            .unwrap()
    }

    fn builtin_fixture() -> (tempfile::TempDir, RuntimeSandboxPolicy, PathBuf) {
        let directory = non_temporary_scratch();
        let root = directory.path().canonicalize().unwrap();
        let path = root.join("skills/computer-use/SKILL.md");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, BUILTIN_BYTES).unwrap();
        let mut policy = RuntimeSandboxPolicy::disabled();
        policy.filesystem.full_disk_read = false;
        policy.filesystem.full_disk_write = false;
        policy.filesystem.readable_roots.clear();
        policy.filesystem.writable_roots.clear();
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
        assert!(ensure_authorized_read(&path, &policy, "filesystem boundary").is_ok());
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
        assert!(ensure_authorized_read(&extra, &policy, "filesystem boundary").is_err());
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
    fn changed_builtins_require_ordinary_scope_and_never_gain_manifest_trust() {
        let (directory, policy, path) = builtin_fixture();
        let mut approved = policy.clone();
        approved.filesystem.readable_roots = vec![path.clone()];
        let mut same_length = BUILTIN_BYTES.to_vec();
        same_length[0] = b'X';
        for bytes in [Vec::new(), same_length, [BUILTIN_BYTES, b"extra"].concat()] {
            fs::write(&path, &bytes).unwrap();
            assert!(read_authorized_bytes(&path, u64::MAX, &policy).is_err());
            assert!(ensure_authorized_read(&path, &policy, "skill").is_err());
            assert!(ensure_authorized_read(&path, &policy, "filesystem boundary").is_err());
            assert_eq!(
                read_authorized_bytes(&path, u64::MAX, &approved).unwrap(),
                bytes
            );
            ensure_authorized_read(&path, &approved, "ordinary file").unwrap();
            assert!(ensure_authorized_read(&path, &approved, "filesystem boundary").is_ok());
            assert!(read_builtin_bytes(&approved.builtin_resources[0], u64::MAX).is_err());
            assert!(!approved.is_path_readable(&directory.path().join("unrequested")));
            assert!(!approved.is_path_writable(&path));
        }
    }

    #[test]
    fn builtin_traversal_is_rejected_before_normalization() {
        let (directory, policy, path) = builtin_fixture();
        let parent = path.parent().unwrap();
        fs::create_dir(parent.join("child")).unwrap();
        // Path::join would remove `..` from a Windows verbatim fixture path.
        let traversal = PathBuf::from(format!(
            "{}{separator}child{separator}..{separator}SKILL.md",
            parent.display(),
            separator = std::path::MAIN_SEPARATOR,
        ));
        assert!(
            traversal
                .components()
                .any(|part| part == Component::ParentDir)
        );
        let requested =
            resolve_read_file_path(directory.path(), &policy, &traversal.to_string_lossy())
                .unwrap();
        assert!(ensure_authorized_read(&requested, &policy, "filesystem boundary").is_err());
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
        // The caller supplies this relative string directly. In particular,
        // the production cwd join must not erase evidence before validation.
        let requested = resolve_read_file_path(parent, &policy, "child/../SKILL.md").unwrap();
        assert!(ensure_authorized_read(&requested, &policy, "filesystem boundary").is_err());
        assert!(
            read_file(
                parent,
                &policy,
                &ReadFileArgs {
                    path: "child/../SKILL.md".into(),
                    ..read_args()
                },
            )
            .is_err()
        );

        // Dot components remain legal for ordinary authorized files.
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path().canonicalize().unwrap();
        fs::create_dir(root.join("child")).unwrap();
        fs::write(root.join("ordinary.txt"), "ordinary content\n").unwrap();
        let mut policy = policy;
        policy.filesystem.readable_roots = vec![root.clone()];
        let ordinary = read_file(
            &root,
            &policy,
            &ReadFileArgs {
                path: "child/../ordinary.txt".into(),
                ..read_args()
            },
        )
        .unwrap();
        assert_eq!(ordinary.content, "ordinary content\n");
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
                    old_text: Some("trusted".into()),
                    new_text: Some("modified".into()),
                    edits: None,
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

        // A normal directory grant permits mutation, without widening the
        // manifest exemption or granting unrelated paths.
        let granted = path.parent().unwrap().to_path_buf();
        policy.filesystem.readable_roots.push(granted.clone());
        policy
            .filesystem
            .writable_roots
            .push(crate::sandbox::WritableRoot {
                root: granted,
                read_only_subpaths: vec![],
            });
        write_file(
            &root,
            &policy,
            &WriteFileArgs {
                path: path.display().to_string(),
                content: "replacement".into(),
                expected_sha256: Some(sha256(BUILTIN_BYTES)),
            },
        )
        .unwrap();
        edit_file(
            &root,
            &policy,
            &EditFileArgs {
                path: path.display().to_string(),
                old_text: Some("replacement".into()),
                new_text: Some("modified".into()),
                edits: None,
                expected_sha256: sha256(b"replacement"),
            },
        )
        .unwrap();
        assert_eq!(
            read_authorized_bytes(&path, u64::MAX, &policy).unwrap(),
            b"modified"
        );
        assert!(read_builtin_bytes(&policy.builtin_resources[0], u64::MAX).is_err());
        copy_file(
            &root,
            &policy,
            &CopyFileArgs {
                source: copied.display().to_string(),
                destination: path.display().to_string(),
                expected_sha256: Some(sha256(b"modified")),
            },
        )
        .unwrap();
        assert_eq!(fs::read(&path).unwrap(), BUILTIN_BYTES);
        assert!(
            !policy.is_path_writable(&path.parent().unwrap().parent().unwrap().join("unrequested"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn builtin_symlinks_aliases_and_replaced_ancestors_are_rejected() {
        let (directory, policy, path) = builtin_fixture();
        // An alias cannot acquire the manifest's no-scope read exemption.
        let alias = directory.path().join("alias.md");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        assert!(read_authorized_bytes(&alias, u64::MAX, &policy).is_err());
        let requested =
            resolve_read_file_path(directory.path(), &policy, &alias.to_string_lossy()).unwrap();
        assert!(ensure_authorized_read(&requested, &policy, "filesystem boundary").is_err());

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
        use std::os::windows::{fs::MetadataExt, process::CommandExt};
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        let (directory, policy, path) = builtin_fixture();
        let parent = path.parent().unwrap();
        let moved = directory.path().join("moved-parent");
        fs::rename(parent, &moved).unwrap();
        // cmd.exe does not use CRT argument quoting. Keep its command fixed
        // and relative to the fixture, avoiding verbatim paths in mklink.
        let output = std::process::Command::new("cmd.exe")
            .current_dir(directory.path())
            .args(["/D", "/V:OFF", "/C"])
            .raw_arg(r#"mklink /J "skills\computer-use" "moved-parent""#)
            .output()
            .expect("Windows junction regression test requires cmd.exe");
        assert!(
            output.status.success(),
            "unable to create junction: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert_ne!(
            fs::symlink_metadata(parent).unwrap().file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT,
            0,
        );
        assert_eq!(fs::read(&path).unwrap(), BUILTIN_BYTES);
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

    // Ordinary fixtures must not inherit a developer's global Git configuration.
    fn search_text(
        cwd: &Path,
        policy: &RuntimeSandboxPolicy,
        args: &SearchTextArgs,
    ) -> Result<SearchResult> {
        super::search_text_with_global_rules(cwd, policy, args, false)
    }

    #[test]
    fn global_gitignore_is_internal_and_can_be_disabled() {
        const CHILD_ROOT: &str = "DAAT_LOCUS_GLOBAL_IGNORE_TEST_ROOT";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let root = PathBuf::from(root);
            let mut policy = RuntimeSandboxPolicy::disabled();
            policy.filesystem.full_disk_read = false;
            policy.filesystem.readable_roots = vec![root.clone()];
            let args = SearchTextArgs {
                path: ".".into(),
                query: "needle".into(),
                case_sensitive: true,
                depth: 1,
                include_ignored: None,
                limit: None,
            };
            let filtered = super::search_text(&root, &policy, &args).unwrap();
            assert_eq!(filtered.matches.len(), 1);
            assert!(filtered.matches[0].path.ends_with("visible.txt"));
            let all = super::search_text(
                &root,
                &policy,
                &SearchTextArgs {
                    include_ignored: Some(true),
                    ..args
                },
            )
            .unwrap();
            assert_eq!(all.matches.len(), 2);
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("visible.txt"), "needle visible\n").unwrap();
        fs::write(root.join("hidden.txt"), "needle hidden\n").unwrap();
        let rules = temp.path().join("global-ignore");
        fs::write(&rules, "hidden.txt\n").unwrap();
        let config = temp.path().join("gitconfig");
        fs::write(
            &config,
            format!(
                "[core]\nexcludesFile = {}\n",
                rules.to_string_lossy().replace('\\', "/")
            ),
        )
        .unwrap();
        // Only the child receives these environment settings; parallel tests and
        // the developer's actual configuration remain untouched.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "file_io::tests::global_gitignore_is_internal_and_can_be_disabled",
                "--nocapture",
            ])
            .env(CHILD_ROOT, &root)
            .env("GIT_CONFIG_GLOBAL", &config)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn edit_matches_across_line_endings_and_writes_the_region_style() {
        // CRLF file x LF old: the match succeeds and new_text is written CRLF.
        let edited = apply_line_ending_tolerant_edit(
            "alpha\r\nbeta line\r\ngamma\r\n",
            "beta line\ngamma",
            "delta\nepsilon",
        )
        .unwrap();
        assert_eq!(edited, "alpha\r\ndelta\r\nepsilon\r\n");
        // LF file x CRLF old: the match succeeds and new_text is written LF.
        let edited = apply_line_ending_tolerant_edit(
            "alpha\nbeta line\ngamma\n",
            "beta line\r\ngamma",
            "delta\r\nepsilon",
        )
        .unwrap();
        assert_eq!(edited, "alpha\ndelta\nepsilon\n");
        // Identical line endings in both directions.
        let edited = apply_line_ending_tolerant_edit("a\r\nb\r\n", "a\r\nb", "x\r\ny").unwrap();
        assert_eq!(edited, "x\r\ny\r\n");
        let edited = apply_line_ending_tolerant_edit("a\nb\n", "a\nb", "x\ny").unwrap();
        assert_eq!(edited, "x\ny\n");
    }

    #[test]
    fn edit_preserves_untouched_line_endings_in_mixed_files() {
        // A mixed file keeps every untouched byte; the matched region's own
        // line ending wins for the replacement.
        let content = "one\r\ntwo\nthree\r\nfour\nfive\r\n";
        let edited =
            apply_line_ending_tolerant_edit(content, "two\nthree", "TWO\nTHREE\nMORE").unwrap();
        assert_eq!(edited, "one\r\nTWO\nTHREE\nMORE\r\nfour\nfive\r\n");
        // A single-line match has no local line ending and falls back to the
        // dominant file ending (CRLF here).
        let edited =
            apply_line_ending_tolerant_edit(content, "four", "four-and-a-half\nnext").unwrap();
        assert_eq!(
            edited,
            "one\r\ntwo\nthree\r\nfour-and-a-half\r\nnext\nfive\r\n"
        );
    }

    #[test]
    fn edit_match_errors_keep_their_semantics_on_the_normalized_view() {
        let error = apply_line_ending_tolerant_edit("a\nb\n", "missing", "x").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("old_text does not occur in the file"),
            "{error}"
        );
        // Two occurrences that differ only in line endings are ambiguous.
        let error =
            apply_line_ending_tolerant_edit("keep\nnext\na\nkeep\r\nnext\nb\n", "keep\nnext", "x")
                .unwrap_err();
        assert!(
            error.to_string().contains("old_text is ambiguous"),
            "{error}"
        );
        // Two byte-identical occurrences stay ambiguous.
        let error = apply_line_ending_tolerant_edit("x\ny\nx\ny\n", "x\ny", "z").unwrap_err();
        assert!(
            error.to_string().contains("old_text is ambiguous"),
            "{error}"
        );
    }

    #[test]
    fn edit_keeps_bom_and_non_ascii_content() {
        let content = "\u{feff}# 标题\r\n第一行\r\n第二行\r\n结尾\r\n";
        let edited =
            apply_line_ending_tolerant_edit(content, "第一行\n第二行", "甲\n乙\n丙").unwrap();
        assert_eq!(edited, "\u{feff}# 标题\r\n甲\r\n乙\r\n丙\r\n结尾\r\n");
        // No line ending in the match or the file: new_text is kept as provided.
        let edited = apply_line_ending_tolerant_edit("单行", "单行", "单行\n继续").unwrap();
        assert_eq!(edited, "单行\n继续");
    }

    fn edit_entry(old_text: &str, new_text: &str) -> EditFileEntry {
        EditFileEntry {
            old_text: old_text.into(),
            new_text: new_text.into(),
        }
    }

    fn edit_args(
        old_text: Option<&str>,
        new_text: Option<&str>,
        edits: Option<Vec<EditFileEntry>>,
    ) -> EditFileArgs {
        EditFileArgs {
            path: "a.txt".into(),
            old_text: old_text.map(str::to_owned),
            new_text: new_text.map(str::to_owned),
            edits,
            expected_sha256: String::new(),
        }
    }

    #[test]
    fn edit_forms_are_mutually_exclusive_and_resolve_to_ordered_batches() {
        assert_eq!(
            edit_args(Some("a"), Some("b"), None)
                .resolved_edits()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            edit_args(
                None,
                None,
                Some(vec![edit_entry("a", "b"), edit_entry("c", "d")])
            )
            .resolved_edits()
            .unwrap()
            .len(),
            2
        );
        assert!(
            edit_args(None, None, Some(Vec::new()))
                .resolved_edits()
                .unwrap_err()
                .to_string()
                .contains("edits must not be empty")
        );
        assert!(
            edit_args(Some("a"), Some("b"), Some(vec![edit_entry("a", "b")]))
                .resolved_edits()
                .unwrap_err()
                .to_string()
                .contains("cannot be combined")
        );
        assert!(edit_args(None, None, None).resolved_edits().is_err());
        assert!(edit_args(Some("a"), None, None).resolved_edits().is_err());
        assert!(edit_args(None, Some("b"), None).resolved_edits().is_err());
    }

    #[test]
    fn edit_batch_applies_every_hunk_on_progressively_updated_content() {
        // The third edit can only match text the second edit produced.
        let edited = apply_edit_batch(
            "alpha\r\nbeta\r\ngamma\r\ndelta\r\n",
            &[
                edit_entry("alpha\nbeta", "one\ntwo"),
                edit_entry("two\ngamma", "two\nTHREE"),
                edit_entry("THREE\ndelta", "THREE\nend"),
            ],
        )
        .unwrap();
        assert_eq!(edited, "one\r\ntwo\r\nTHREE\r\nend\r\n");
        // An LF file keeps LF while CRLF payloads still match.
        let edited = apply_edit_batch(
            "alpha\nbeta\ngamma\n",
            &[
                edit_entry("alpha\r\nbeta", "one\ntwo"),
                edit_entry("two\r\ngamma", "two\nTHREE"),
            ],
        )
        .unwrap();
        assert_eq!(edited, "one\ntwo\nTHREE\n");
    }

    #[test]
    fn edit_batch_reports_the_failing_entry_and_aborts() {
        let error = apply_edit_batch(
            "alpha\nbeta\n",
            &[edit_entry("alpha", "one"), edit_entry("missing", "two")],
        )
        .unwrap_err();
        assert!(error.to_string().contains("edit 2 of 2 failed"), "{error}");
        assert!(
            error.to_string().contains("does not occur in the file"),
            "{error}"
        );
        let error = apply_edit_batch("same\nsame\n", &[edit_entry("same", "one")]).unwrap_err();
        assert!(error.to_string().contains("edit 1 of 1 failed"), "{error}");
        assert!(error.to_string().contains("ambiguous"), "{error}");
        assert!(
            apply_edit_batch("same\n", &[])
                .unwrap_err()
                .to_string()
                .contains("edits must not be empty")
        );
    }

    #[test]
    fn edit_file_batch_abort_leaves_the_file_untouched() {
        let dir = non_temporary_scratch();
        let file = dir.path().join("doc.txt");
        fs::write(&file, "alpha\r\nbeta\r\ngamma\r\n").unwrap();
        let error = edit_file(
            dir.path(),
            &RuntimeSandboxPolicy::disabled(),
            &EditFileArgs {
                path: "doc.txt".into(),
                old_text: None,
                new_text: None,
                edits: Some(vec![
                    edit_entry("alpha\nbeta", "one\ntwo"),
                    edit_entry("missing", "x"),
                ]),
                expected_sha256: sha256(b"alpha\r\nbeta\r\ngamma\r\n"),
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("edit 2 of 2 failed"), "{error}");
        assert_eq!(fs::read(&file).unwrap(), b"alpha\r\nbeta\r\ngamma\r\n");
    }

    #[test]
    fn edit_file_single_pair_stays_supported() {
        let dir = non_temporary_scratch();
        let file = dir.path().join("doc.txt");
        fs::write(&file, "alpha\n").unwrap();
        let error = edit_file(
            dir.path(),
            &RuntimeSandboxPolicy::disabled(),
            &EditFileArgs {
                path: "doc.txt".into(),
                old_text: Some("missing".into()),
                new_text: Some("x".into()),
                edits: None,
                expected_sha256: sha256(b"alpha\n"),
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("edit 1 of 1 failed"), "{error}");
        assert_eq!(fs::read(&file).unwrap(), b"alpha\n");
    }

    #[test]
    fn edit_file_batch_applies_all_edits_with_one_write() {
        let dir = non_temporary_scratch();
        let file = dir.path().join("doc.txt");
        fs::write(&file, "alpha\r\nbeta\r\ngamma\r\n").unwrap();
        let result = edit_file(
            dir.path(),
            &RuntimeSandboxPolicy::disabled(),
            &EditFileArgs {
                path: "doc.txt".into(),
                old_text: None,
                new_text: None,
                edits: Some(vec![
                    edit_entry("alpha\nbeta", "one\ntwo"),
                    edit_entry("gamma", "gamma\nend"),
                ]),
                expected_sha256: sha256(b"alpha\r\nbeta\r\ngamma\r\n"),
            },
        )
        .unwrap();
        assert!(!result.created);
        assert_eq!(result.byte_count, "one\r\ntwo\r\ngamma\r\nend\r\n".len());
        assert_eq!(result.sha256, sha256(b"one\r\ntwo\r\ngamma\r\nend\r\n"));
        assert_eq!(fs::read(&file).unwrap(), b"one\r\ntwo\r\ngamma\r\nend\r\n");
    }

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
            old_text: Some("aa".into()),
            new_text: Some("b".into()),
            edits: None,
            expected_sha256: created.sha256.clone(),
        };
        assert!(
            edit_file(dir.path(), &policy, &edit)
                .unwrap_err()
                .to_string()
                .contains("ambiguous")
        );
        edit.old_text = Some("aaa".into());
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
        let dir = non_temporary_scratch();
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
        let dir = crate::sandbox::test_support::scoped_directory();
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
                depth: 1,
                include_ignored: None,
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
        for include_ignored in [None, Some(true)] {
            let found = search_text(
                dir.path(),
                &policy,
                &SearchTextArgs {
                    path: ".".into(),
                    query: "needle".into(),
                    case_sensitive: false,
                    depth: 3,
                    include_ignored,
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

    #[test]
    fn search_skips_gitignored_directories_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join(".gitignore"), "/target\nnode_modules/\n").unwrap();
        fs::write(root.join(".git/metadata"), "needle in metadata\n").unwrap();
        fs::write(root.join(".hidden"), "needle in hidden\n").unwrap();
        fs::write(root.join(".ignore"), "source.txt\n").unwrap();
        fs::create_dir_all(root.join("nested/build")).unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::create_dir_all(root.join("node_modules")).unwrap();
        fs::write(root.join("nested/.gitignore"), "build/\n").unwrap();
        fs::write(root.join("target/hidden.txt"), "needle in target\n").unwrap();
        fs::write(root.join("node_modules/hidden.txt"), "needle in modules\n").unwrap();
        fs::write(root.join("nested/build/hidden.txt"), "needle in build\n").unwrap();
        fs::write(root.join("nested/keep.txt"), "needle in nested\n").unwrap();
        fs::write(root.join("source.txt"), "needle in source\n").unwrap();
        let args = SearchTextArgs {
            path: ".".into(),
            query: "needle".into(),
            case_sensitive: true,
            depth: 4,
            include_ignored: None,
            limit: None,
        };

        let found = search_text(&root, &RuntimeSandboxPolicy::disabled(), &args).unwrap();
        let mut texts = found
            .matches
            .iter()
            .map(|item| item.text.trim())
            .collect::<Vec<_>>();
        texts.sort();
        assert_eq!(
            texts,
            ["needle in hidden", "needle in nested", "needle in source"]
        );

        let unfiltered = search_text(
            &root,
            &RuntimeSandboxPolicy::disabled(),
            &SearchTextArgs {
                include_ignored: Some(true),
                ..args
            },
        )
        .unwrap();
        assert_eq!(unfiltered.matches.len(), 7);
    }

    #[test]
    fn subdirectory_search_inherits_repository_rules_and_stops_at_git_boundary() {
        for git_file in [false, true] {
            let dir = non_temporary_scratch();
            let outer = dir.path().canonicalize().unwrap();
            let root = outer.join("repo");
            let subdir = root.join("src/deep");
            fs::create_dir_all(&subdir).unwrap();
            if git_file {
                fs::write(root.join(".git"), "gitdir: elsewhere\n").unwrap();
            } else {
                fs::create_dir(root.join(".git")).unwrap();
            }
            fs::write(outer.join(".gitignore"), "*.txt\n").unwrap();
            fs::write(root.join(".gitignore"), "*.log\n/root-only.txt\nignored/\n").unwrap();
            fs::write(root.join("src/.gitignore"), "!keep.log\n").unwrap();
            for name in ["drop.log", "keep.log", "source.txt", "root-only.txt"] {
                fs::write(subdir.join(name), "needle\n").unwrap();
            }
            let mut policy = RuntimeSandboxPolicy::disabled();
            policy.filesystem.full_disk_read = false;
            policy.filesystem.readable_roots = vec![outer.clone()];
            let args = SearchTextArgs {
                path: subdir.to_string_lossy().into_owned(),
                query: "needle".into(),
                case_sensitive: true,
                depth: 1,
                include_ignored: None,
                limit: None,
            };
            let found = search_text(&root, &policy, &args).unwrap();
            let mut names = found
                .matches
                .iter()
                .map(|item| {
                    Path::new(&item.path)
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect::<Vec<_>>();
            names.sort();
            assert_eq!(names, ["keep.log", "root-only.txt", "source.txt"]);
            let file_args = SearchTextArgs {
                path: subdir.join("drop.log").to_string_lossy().into_owned(),
                ..args.clone()
            };
            assert_eq!(
                search_text(&root, &policy, &file_args)
                    .unwrap()
                    .matches
                    .len(),
                1
            );
            let all = search_text(
                &root,
                &policy,
                &SearchTextArgs {
                    include_ignored: Some(true),
                    ..args.clone()
                },
            )
            .unwrap();
            assert_eq!(all.matches.len(), 4);

            // Internal rules still apply when only the search subtree is in scope.
            policy.filesystem.readable_roots = vec![subdir.clone()];
            assert_eq!(
                search_text(&subdir, &policy, &args).unwrap().matches.len(),
                3
            );
            // Loading internal metadata does not expand the searched file scope.
            assert!(
                search_text(
                    &subdir,
                    &policy,
                    &SearchTextArgs {
                        path: root.to_string_lossy().into_owned(),
                        ..args.clone()
                    }
                )
                .is_err()
            );
            assert!(
                read_file(
                    &subdir,
                    &policy,
                    &ReadFileArgs {
                        path: root.join(".gitignore").to_string_lossy().into_owned(),
                        start_line: None,
                        line_count: None,
                    }
                )
                .is_err()
            );

            // WalkBuilder treats an explicitly supplied root as searchable.
            let excluded = root.join("ignored/deep");
            fs::create_dir_all(&excluded).unwrap();
            fs::write(excluded.join("hidden.txt"), "needle\n").unwrap();
            policy.filesystem.readable_roots = vec![root.clone()];
            let excluded_args = SearchTextArgs {
                path: excluded.to_string_lossy().into_owned(),
                ..args
            };
            assert_eq!(
                search_text(&root, &policy, &excluded_args)
                    .unwrap()
                    .matches
                    .len(),
                1
            );
            assert_eq!(
                search_text(
                    &root,
                    &policy,
                    &SearchTextArgs {
                        include_ignored: Some(true),
                        ..excluded_args
                    }
                )
                .unwrap()
                .matches
                .len(),
                1
            );
        }
    }

    #[test]
    fn search_uses_git_globs_and_nested_unignores() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("nested/deep")).unwrap();
        fs::write(
            root.join(".gitignore"),
            "/root-only.txt\n*.log\nreport[0-9].txt\nnested/**/hidden.txt\n",
        )
        .unwrap();
        fs::write(root.join("nested/.gitignore"), "!keep.log\n").unwrap();
        for name in [
            "root-only.txt",
            "nested/root-only.txt",
            "debug.log",
            "nested/keep.log",
            "report1.txt",
            "reportx.txt",
            "nested/hidden.txt",
            "nested/deep/hidden.txt",
            "nested/deep/nothidden.txt",
        ] {
            fs::write(root.join(name), "needle\n").unwrap();
        }
        let result = search_text(
            &root,
            &RuntimeSandboxPolicy::disabled(),
            &SearchTextArgs {
                path: ".".into(),
                query: "needle".into(),
                case_sensitive: true,
                depth: 4,
                include_ignored: None,
                limit: None,
            },
        )
        .unwrap();
        let mut paths = result
            .matches
            .iter()
            .map(|item| {
                Path::new(&item.path)
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect::<Vec<_>>();
        paths.sort();
        assert_eq!(
            paths,
            [
                "nested/deep/nothidden.txt",
                "nested/keep.log",
                "nested/root-only.txt",
                "reportx.txt"
            ]
        );
    }

    #[test]
    fn search_honors_the_required_depth_limit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("one/two")).unwrap();
        fs::write(root.join("top.txt"), "needle top\n").unwrap();
        fs::write(root.join("one/one.txt"), "needle one\n").unwrap();
        fs::write(root.join("one/two/deep.txt"), "needle deep\n").unwrap();
        let args = SearchTextArgs {
            path: ".".into(),
            query: "needle".into(),
            case_sensitive: true,
            depth: 1,
            include_ignored: None,
            limit: None,
        };

        let shallow = search_text(&root, &RuntimeSandboxPolicy::disabled(), &args).unwrap();
        assert_eq!(shallow.matches.len(), 1);
        assert!(shallow.matches[0].path.ends_with("top.txt"));

        let deeper = search_text(
            &root,
            &RuntimeSandboxPolicy::disabled(),
            &SearchTextArgs { depth: 3, ..args },
        )
        .unwrap();
        assert_eq!(deeper.matches.len(), 3);
    }

    #[test]
    fn search_rejects_a_missing_or_out_of_range_depth() {
        // The model contract requires an explicit depth; a silent default would
        // let one call walk an arbitrarily deep tree.
        assert!(
            serde_json::from_value::<SearchTextArgs>(serde_json::json!({
                "path": ".",
                "query": "needle",
                "case_sensitive": true,
                "limit": null,
            }))
            .is_err()
        );

        let dir = tempfile::tempdir().unwrap();
        let policy = RuntimeSandboxPolicy::disabled();
        let mut args = SearchTextArgs {
            path: ".".into(),
            query: "needle".into(),
            case_sensitive: true,
            depth: 0,
            include_ignored: None,
            limit: None,
        };
        assert!(search_text(dir.path(), &policy, &args).is_err());
        args.depth = MAX_SEARCH_DEPTH + 1;
        assert!(search_text(dir.path(), &policy, &args).is_err());
        args.depth = MAX_SEARCH_DEPTH;
        assert!(search_text(dir.path(), &policy, &args).is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn mangled_extended_length_paths_still_name_the_same_local_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::write(root.join("a.txt"), "needle\n").unwrap();
        // Earlier separator normalization produced `//?/C:/...`, which
        // std::path parses as a UNC prefix instead of an extended-length one.
        let mangled = root.join("a.txt").display().to_string().replace('\\', "/");
        assert!(mangled.starts_with("//?/"));
        let page = read_file(
            &root,
            &RuntimeSandboxPolicy::disabled(),
            &ReadFileArgs {
                path: mangled,
                start_line: None,
                line_count: None,
            },
        )
        .unwrap();
        assert_eq!(page.content, "needle\n");
    }

    #[cfg(windows)]
    #[test]
    fn verbatim_prefixes_round_trip_and_unc_is_not_a_device_path() {
        let restored = normalize_path_input("//?/UNC/server/share/skill.md");
        assert_eq!(&*restored, r"\\?\UNC\server\share\skill.md");
        let untouched = normalize_path_input(r"\\?\C:\Users\shadow\skill.md");
        assert_eq!(&*untouched, r"\\?\C:\Users\shadow\skill.md");

        // A network target keeps its remote namespace; it never becomes a
        // device namespace or an accepted local volume.
        let dir = tempfile::tempdir().unwrap();
        let error = read_file(
            dir.path(),
            &RuntimeSandboxPolicy::disabled(),
            &ReadFileArgs {
                path: r"\\?\UNC\server\share\skill.md".into(),
                start_line: None,
                line_count: None,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("remote or device"));
    }
}
