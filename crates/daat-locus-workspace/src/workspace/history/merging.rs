use super::*;

fn ancestors(
    connection: &Connection,
    project: i64,
    version: &Version,
) -> Result<BTreeMap<String, Version>> {
    let mut result = BTreeMap::new();
    let mut pending = vec![version.clone()];
    while let Some(version) = pending.pop() {
        if result.contains_key(&version.id) {
            continue;
        }
        for parent in &version.parents {
            pending.push(load_version(connection, project, parent)?);
        }
        result.insert(version.id.clone(), version);
    }
    Ok(result)
}

pub(super) fn merge_base(
    connection: &Connection,
    project: i64,
    left: &Version,
    right: &Version,
) -> Result<Version> {
    let left = ancestors(connection, project, left)?;
    let right = ancestors(connection, project, right)?;
    let common: BTreeSet<_> = left
        .keys()
        .filter(|id| right.contains_key(*id))
        .cloned()
        .collect();
    let mut nearest = common.clone();
    for id in &common {
        for parent in &left[id].parents {
            nearest.remove(parent);
        }
    }
    match nearest.len() {
        0 => Err(Error::Conflict(
            "workspace versions have no common ancestor".into(),
        )),
        1 => Ok(left[nearest.first().expect("one ancestor")].clone()),
        _ => {
            // With ambiguous ancestry, compare conservatively: equal paths still
            // merge, while differing existing paths require an explicit resolution.
            let mut empty = Node::load(
                connection,
                &left[nearest.first().expect("common ancestors")].root,
            )?;
            empty.children.clear();
            insert_version(
                connection,
                project,
                &empty.save(connection)?,
                None,
                None,
                "Compare multiple common ancestors",
            )
        }
    }
}

struct Merger<'a> {
    connection: &'a Connection,
    resolutions: BTreeMap<String, Option<ObjectId>>,
    used: BTreeSet<String>,
    conflicts: Vec<String>,
}

impl Merger<'_> {
    fn equal(&self, left: Option<&ObjectId>, right: Option<&ObjectId>) -> Result<bool> {
        match (left, right) {
            (None, None) => Ok(true),
            (Some(left), Some(right)) => trees_equal(self.connection, left, right),
            _ => Ok(false),
        }
    }

    fn conflict(&mut self, path: &str, left: Option<&ObjectId>) -> Option<ObjectId> {
        if let Some(resolved) = self.resolutions.get(path) {
            self.used.insert(path.to_owned());
            return resolved.clone();
        }
        self.conflicts.push(path.to_owned());
        left.cloned()
    }

    fn node(
        &mut self,
        path: &str,
        base: Option<&ObjectId>,
        left: Option<&ObjectId>,
        right: Option<&ObjectId>,
    ) -> Result<Option<ObjectId>> {
        if self.equal(left, right)? {
            return Ok(left.cloned());
        }
        if self.equal(base, left)? {
            return Ok(right.cloned());
        }
        if self.equal(base, right)? {
            return Ok(left.cloned());
        }
        let (Some(left_id), Some(right_id)) = (left, right) else {
            return Ok(self.conflict(path, left));
        };
        let left_node = Node::load(self.connection, left_id)?;
        let right_node = Node::load(self.connection, right_id)?;
        let base_node = base.map(|id| Node::load(self.connection, id)).transpose()?;
        if left_node.entry.kind == EntryKind::Directory
            && right_node.entry.kind == EntryKind::Directory
            && base_node
                .as_ref()
                .is_none_or(|node| node.entry.kind == EntryKind::Directory)
        {
            let mut result = left_node.clone();
            if left_node.entry.mode != right_node.entry.mode {
                if base_node
                    .as_ref()
                    .is_some_and(|node| node.entry.mode == left_node.entry.mode)
                {
                    result.entry.mode = right_node.entry.mode;
                } else if !base_node
                    .as_ref()
                    .is_some_and(|node| node.entry.mode == right_node.entry.mode)
                {
                    return Ok(self.conflict(path, left));
                }
            }
            let empty = BTreeMap::new();
            let base_children = base_node.as_ref().map_or(&empty, |node| &node.children);
            let names: BTreeSet<_> = base_children
                .keys()
                .chain(left_node.children.keys())
                .chain(right_node.children.keys())
                .cloned()
                .collect();
            result.children.clear();
            for name in names {
                let child = if path.is_empty() {
                    name.clone()
                } else {
                    format!("{path}/{name}")
                };
                if let Some(id) = self.node(
                    &child,
                    base_children.get(&name),
                    left_node.children.get(&name),
                    right_node.children.get(&name),
                )? {
                    result.children.insert(name, id);
                }
            }
            result.entry.modified_ns = now_ns();
            return Ok(Some(result.save(self.connection)?));
        }
        if let Some(base) = &base_node
            && let Some(merged) = merge_text(self.connection, base, &left_node, &right_node)?
        {
            return Ok(Some(merged.save(self.connection)?));
        }
        Ok(self.conflict(path, left))
    }
}

pub(super) fn merge_trees(
    connection: &Connection,
    project: i64,
    base: &Version,
    left: &Version,
    right: &Version,
    resolutions: &[Resolution],
) -> Result<(ObjectId, Vec<String>)> {
    let mut choices = BTreeMap::new();
    for resolution in resolutions {
        relative_path(&resolution.path)?;
        let root = resolution
            .source
            .as_deref()
            .map(|source| resolve_saved(connection, project, source, "Capture conflict resolution"))
            .transpose()?;
        let node = match root {
            Some(version) => {
                Some(lookup(connection, &version.root, &resolution.path)?.save(connection)?)
            }
            None => None,
        };
        if choices.insert(resolution.path.clone(), node).is_some() {
            return Err(Error::Invalid(format!(
                "duplicate resolution: {}",
                resolution.path
            )));
        }
    }
    let mut merger = Merger {
        connection,
        resolutions: choices,
        used: BTreeSet::new(),
        conflicts: vec![],
    };
    let root = merger
        .node("", Some(&base.root), Some(&left.root), Some(&right.root))?
        .ok_or_else(|| Error::Invalid("cannot delete a workspace root".into()))?;
    if let Some(unused) = merger
        .resolutions
        .keys()
        .find(|path| !merger.used.contains(*path))
    {
        return Err(Error::Invalid(format!(
            "resolution does not name a conflict: {unused}"
        )));
    }
    let root = if merger.conflicts.is_empty() {
        distinct_inodes(connection, &root, &mut BTreeSet::new())?
    } else {
        root
    };
    Ok((root, merger.conflicts))
}

// Independent renames can place one source inode at multiple result paths.
// Each result path remains an independent file in this filesystem.
fn distinct_inodes(
    connection: &Connection,
    root: &ObjectId,
    seen: &mut BTreeSet<u64>,
) -> Result<ObjectId> {
    let mut node = Node::load(connection, root)?;
    let mut changed = !seen.insert(node.entry.inode);
    if changed {
        node.entry.inode = next_inode(connection)?;
    }
    for child in node.children.values_mut() {
        let next = distinct_inodes(connection, child, seen)?;
        changed |= next != *child;
        *child = next;
    }
    if changed {
        node.save(connection)
    } else {
        Ok(root.clone())
    }
}
