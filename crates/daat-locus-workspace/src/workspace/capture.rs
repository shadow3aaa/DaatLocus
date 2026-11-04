use super::*;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize)]
pub struct CaptureProgress {
    pub path: String,
    pub files: u64,
    pub bytes: u64,
}

struct Capture<'a> {
    connection: &'a Connection,
    origin: &'a Directory,
    project: i64,
    rules: Vec<Gitignore>,
    tracked: BTreeSet<String>,
    progress: CaptureProgress,
}

pub(super) fn snapshot(
    connection: &Connection,
    origin: &Directory,
    project: i64,
    path: &str,
) -> Result<ObjectId> {
    let raw = origin.clone().with_ignored(true);
    let origin = if path.is_empty() { origin } else { &raw };
    let mut capture = Capture {
        connection,
        origin,
        project,
        rules: vec![],
        tracked: BTreeSet::new(),
        progress: CaptureProgress::default(),
    };
    if !origin.include_ignored {
        capture.tracked = tracked(origin)?;
    }
    capture.report(path)?;
    let root = capture
        .node(path)?
        .ok_or_else(|| Error::Invalid("cannot ignore snapshot root".into()))?;
    capture.report(path)?;
    Ok(root)
}

impl Capture<'_> {
    fn report(&mut self, path: &str) -> Result<()> {
        if let Some(progress) = &self.origin.progress {
            self.progress.path = path.into();
            progress(&self.progress)?;
        }
        Ok(())
    }

    fn load_rules(&mut self, path: &str) -> Result<()> {
        if self.origin.include_ignored {
            return Ok(());
        }
        let name = if path.is_empty() {
            ".gitignore".into()
        } else {
            format!("{path}/.gitignore")
        };
        let Some(bytes) = read_optional(self.origin, &name)? else {
            return Ok(());
        };
        add_rules(&mut self.rules, self.origin.path(), path, &name, &bytes)?;
        Ok(())
    }

    fn node(&mut self, path: &str) -> Result<Option<ObjectId>> {
        let origin = self.origin;
        let connection = self.connection;
        let project = self.project;
        let metadata = origin.metadata(path)?;
        let inode = match connection
            .query_row(
                "SELECT inode FROM source_inodes WHERE project=?1 AND path=?2",
                params![project, path],
                |row| Ok(row.get::<_, i64>(0)? as u64),
            )
            .optional()?
        {
            Some(inode) => inode,
            None => {
                let inode = next_inode(connection)?;
                connection.execute(
                    "INSERT INTO source_inodes VALUES(?1,?2,?3)",
                    params![project, path, inode as i64],
                )?;
                inode
            }
        };
        let kind = if metadata.is_symlink() {
            EntryKind::Symlink
        } else if metadata.is_dir() {
            EntryKind::Directory
        } else if metadata.is_file() {
            EntryKind::File
        } else {
            return Err(Error::Unsupported(format!(
                "special filesystem entry {path}"
            )));
        };
        #[cfg(unix)]
        let mode = {
            use cap_std::fs::PermissionsExt;
            metadata.permissions().mode() & 0o777
        };
        #[cfg(not(unix))]
        let mode = if metadata.permissions().readonly() {
            0o444
        } else if kind == EntryKind::Directory {
            0o755
        } else {
            0o644
        };
        let modified_ns = metadata
            .modified()?
            .into_std()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |time| time.as_nanos().min(i64::MAX as u128) as i64);
        let ignored = ignored(
            &self.rules,
            origin.path(),
            path,
            kind == EntryKind::Directory,
        );
        if ignored && !self.tracked.contains(path) {
            return Ok(None);
        }
        let mut node = Node {
            entry: Entry {
                inode,
                kind,
                size: 0,
                mode,
                modified_ns,
            },
            blocks: vec![],
            children: BTreeMap::new(),
            target: None,
            include_ignored: None,
            #[cfg(windows)]
            reparse: None,
        };
        match kind {
            EntryKind::Directory => {
                let previous = self.rules.len();
                if !ignored {
                    self.load_rules(path)?;
                }
                for name in origin.names(path)? {
                    let child = if path.is_empty() {
                        name.clone()
                    } else {
                        format!("{path}/{name}")
                    };
                    if let Some(root) = self.node(&child)? {
                        node.children.insert(name, root);
                    }
                }
                self.rules.truncate(previous);
            }
            EntryKind::File => {
                self.progress.files += 1;
                node.entry.size = origin.read_chunks(path, |chunk| {
                    self.progress.bytes += chunk.len() as u64;
                    self.report(path)?;
                    node.blocks.push(put(connection, 0, chunk)?);
                    Ok(())
                })?;
            }
            EntryKind::Symlink => {
                self.progress.files += 1;
                self.report(path)?;
                #[cfg(not(windows))]
                let target = origin.read_link(path)?;
                #[cfg(windows)]
                let target = {
                    let point = origin.read_reparse(path)?;
                    let target = point.target()?;
                    node.reparse = Some(point);
                    target
                };
                node.entry.size = target.len() as u64;
                node.target = Some(target);
            }
        }
        if path.is_empty() {
            node.include_ignored = Some(origin.include_ignored);
        }
        Ok(Some(node.save(connection)?))
    }
}

fn read_optional(origin: &Directory, path: &str) -> Result<Option<Vec<u8>>> {
    match origin.metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Ok(None),
        Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let mut bytes = Vec::new();
    origin.read_chunks(path, |chunk| {
        bytes.extend_from_slice(chunk);
        Ok(())
    })?;
    Ok(Some(bytes))
}

fn tracked(origin: &Directory) -> Result<BTreeSet<String>> {
    let mut worktree = origin.clone();
    let git = loop {
        match worktree.metadata(".git") {
            Ok(metadata) if metadata.is_dir() || metadata.is_symlink() => {
                break Directory::open(&worktree.path().join(".git"), origin.authorizer())?;
            }
            Ok(metadata) if metadata.is_file() => {
                let bytes = read_optional(&worktree, ".git")?
                    .ok_or_else(|| Error::Conflict("Git directory changed".into()))?;
                let text = std::str::from_utf8(&bytes)
                    .map_err(|error| Error::Invalid(error.to_string()))?;
                let path = text
                    .trim()
                    .strip_prefix("gitdir: ")
                    .ok_or_else(|| Error::Invalid("invalid Git directory file".into()))?;
                break Directory::open(&worktree.path().join(path), origin.authorizer())?;
            }
            Ok(_) => return Ok(BTreeSet::new()),
            Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let Some(parent) = worktree.path().parent() else {
            return Ok(BTreeSet::new());
        };
        if (origin.authorizer())(parent, crate::Access::Read).is_err() {
            return Ok(BTreeSet::new());
        }
        worktree = Directory::open(parent, origin.authorizer())?;
    };
    let Some(bytes) = read_optional(&git, "index")? else {
        return Ok(BTreeSet::new());
    };
    let prefix = origin
        .path()
        .strip_prefix(worktree.path())
        .map_err(|error| Error::Invalid(error.to_string()))?;
    Ok(index_paths(&bytes, |path| read_optional(&git, path))?
        .into_iter()
        .filter_map(|path| {
            Path::new(&path)
                .strip_prefix(prefix)
                .ok()
                .map(|path| path.to_string_lossy().replace('\\', "/"))
        })
        .collect())
}

fn index_paths(
    bytes: &[u8],
    read_shared: impl Fn(&str) -> Result<Option<Vec<u8>>>,
) -> Result<BTreeSet<String>> {
    let mut decoded = None;
    for hash in [gix_index::hash::Kind::Sha1, gix_index::hash::Kind::Sha256] {
        if let Ok((state, _)) = gix_index::State::from_bytes(
            bytes,
            UNIX_EPOCH.into(),
            hash,
            gix_index::decode::Options::default(),
        ) {
            decoded = Some((state, hash));
            break;
        }
    }
    let (mut state, hash) =
        decoded.ok_or_else(|| Error::Invalid("cannot decode Git index".into()))?;
    if let Some(link) = state.link() {
        let shared = format!("sharedindex.{}", link.shared_index_checksum);
        let data = read_shared(&shared)?
            .ok_or_else(|| Error::Invalid("missing shared Git index".into()))?;
        let temporary = tempfile::tempdir()?;
        let index = temporary.path().join("index");
        std::fs::write(&index, bytes)?;
        std::fs::write(temporary.path().join(shared), data)?;
        let file = gix_index::File::at(index, hash, false, gix_index::decode::Options::default())
            .map_err(|error| Error::Invalid(error.to_string()))?;
        state = (*file).clone();
    }
    let mut tracked = BTreeSet::new();
    for entry in state.entries() {
        let path = std::str::from_utf8(entry.path(&state).as_ref())
            .map_err(|error| Error::Invalid(error.to_string()))?;
        for parent in Path::new(path).ancestors() {
            if let Some(path) = parent.to_str() {
                tracked.insert(path.replace('\\', "/"));
            }
        }
    }
    Ok(tracked)
}

fn add_rules(
    rules: &mut Vec<Gitignore>,
    root: &Path,
    path: &str,
    name: &str,
    bytes: &[u8],
) -> Result<()> {
    let text = std::str::from_utf8(bytes).map_err(|error| Error::Invalid(error.to_string()))?;
    let mut builder = GitignoreBuilder::new(root.join(path));
    for line in text.lines() {
        builder
            .add_line(Some(root.join(name)), line)
            .map_err(|error| Error::Invalid(error.to_string()))?;
    }
    rules.push(
        builder
            .build()
            .map_err(|error| Error::Invalid(error.to_string()))?,
    );
    Ok(())
}

fn ignored(rules: &[Gitignore], root: &Path, path: &str, directory: bool) -> bool {
    !path.is_empty()
        && rules
            .iter()
            .rev()
            .find_map(|rule| {
                let matched = rule.matched_path_or_any_parents(root.join(path), directory);
                (!matched.is_none()).then_some(matched.is_ignore())
            })
            .unwrap_or(false)
}

pub(super) fn saved_tree(
    connection: &Connection,
    head: &ObjectId,
    base: &ObjectId,
) -> Result<ObjectId> {
    if Node::load(connection, head)?
        .include_ignored
        .unwrap_or(true)
    {
        return Ok(head.clone());
    }
    let read = |path: &str| -> Result<Option<Vec<u8>>> {
        match lookup(connection, head, path) {
            Ok(node) if node.entry.kind == EntryKind::File => {
                read_node(connection, &node, 0, usize::MAX).map(Some)
            }
            Ok(_) => Ok(None),
            Err(Error::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    };
    let mut tracked: BTreeSet<_> = flatten(connection, base)?.into_keys().collect();
    if let Some(bytes) = read(".git/index")? {
        tracked.extend(index_paths(&bytes, |path| read(&format!(".git/{path}")))?);
    }
    fn walk(
        connection: &Connection,
        id: &ObjectId,
        path: &str,
        tracked: &BTreeSet<String>,
        rules: &mut Vec<Gitignore>,
        read: &impl Fn(&str) -> Result<Option<Vec<u8>>>,
    ) -> Result<Option<ObjectId>> {
        let mut node = Node::load(connection, id)?;
        let root = Path::new("/");
        let excluded = ignored(rules, root, path, node.entry.kind == EntryKind::Directory);
        if excluded && !tracked.contains(path) {
            return Ok(None);
        }
        if node.entry.kind != EntryKind::Directory {
            return Ok(Some(id.clone()));
        }
        let previous = rules.len();
        if !excluded {
            let name = if path.is_empty() {
                ".gitignore".into()
            } else {
                format!("{path}/.gitignore")
            };
            if let Some(bytes) = read(&name)? {
                add_rules(rules, root, path, &name, &bytes)?;
            }
        }
        let mut children = BTreeMap::new();
        for (name, id) in &node.children {
            let path = if path.is_empty() {
                name.clone()
            } else {
                format!("{path}/{name}")
            };
            if let Some(id) = walk(connection, id, &path, tracked, rules, read)? {
                children.insert(name.clone(), id);
            }
        }
        rules.truncate(previous);
        if children == node.children {
            return Ok(Some(id.clone()));
        }
        node.children = children;
        node.save(connection).map(Some)
    }
    walk(connection, head, "", &tracked, &mut Vec::new(), &read)?
        .ok_or_else(|| Error::Invalid("cannot ignore workspace root".into()))
}
