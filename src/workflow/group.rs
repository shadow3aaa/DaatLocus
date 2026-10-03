//! Session-scoped workflow group.
//!
//! The main agent edits one graph of workflow runs. Edges are admission
//! constraints only: a run stays pending until every run it waits for has
//! finished. They do not transfer output between runs.
//!
//! Retention is per undirected connected component. A component remains while
//! any member is still pending or running, including a pending run blocked by
//! a finished dependency. Once every member is completed, failed, or
//! interrupted, the whole component leaves the active graph together.

use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{WorkflowNodeStatus, WorkflowRunSnapshot};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub struct WorkflowGroupSnapshot {
    #[serde(default)]
    pub nodes: Vec<WorkflowGroupNodeSnapshot>,
    #[serde(default)]
    pub edges: Vec<WorkflowGroupEdgeSnapshot>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowGroupNodeSnapshot {
    pub run_id: String,
    pub workflow_id: String,
    pub status: WorkflowNodeStatus,
    pub started_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<i64>,
    pub input: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub message: String,
    /// Latest inner run snapshot for the second-level inspector. Absent until the
    /// run has actually been spawned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<WorkflowRunSnapshot>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowGroupEdgeSnapshot {
    pub source_run_id: String,
    pub target_run_id: String,
}

#[derive(Clone, Debug)]
struct GroupNode {
    workflow_id: String,
    status: WorkflowNodeStatus,
    started_at_ms: i64,
    completed_at_ms: Option<i64>,
    input: Value,
    output: Option<Value>,
    error: Option<String>,
    message: String,
    snapshot: Option<WorkflowRunSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct GroupEdge {
    source_run_id: String,
    target_run_id: String,
}

#[derive(Clone, Debug, Default)]
pub(super) struct WorkflowGroup {
    nodes: HashMap<String, GroupNode>,
    edges: HashSet<GroupEdge>,
    /// Waiter -> dependencies. Rebuilt only when edges change.
    incoming: HashMap<String, Vec<String>>,
}

impl WorkflowGroup {
    pub(super) fn insert_pending(
        &mut self,
        run_id: &str,
        workflow_id: &str,
        input: Value,
        started_at_ms: i64,
    ) {
        self.nodes.insert(
            run_id.to_string(),
            GroupNode {
                workflow_id: workflow_id.to_string(),
                status: WorkflowNodeStatus::Pending,
                started_at_ms,
                completed_at_ms: None,
                input,
                output: None,
                error: None,
                message: "pending".to_string(),
                snapshot: None,
            },
        );
    }

    /// Record admission edges. `source` is the dependency being waited on and
    /// `target` is the waiter. Edges are committed only when every requested
    /// edge is legal, so a rejected cycle leaves the existing graph unchanged.
    pub(super) fn add_waits(&mut self, run_id: &str, waits_for: &[String]) -> Result<(), String> {
        if !self.nodes.contains_key(run_id) {
            return Err(format!("unknown workflow run `{run_id}`"));
        }
        if self.status(run_id) != Some(WorkflowNodeStatus::Pending) {
            return Err(format!(
                "workflow run `{run_id}` can only wait while it is still pending"
            ));
        }
        let mut candidate_edges = Vec::with_capacity(waits_for.len());
        for dependency in waits_for {
            if dependency == run_id {
                return Err(format!("workflow run `{run_id}` cannot wait for itself"));
            }
            if !self.nodes.contains_key(dependency) {
                return Err(format!("unknown workflow run `{dependency}`"));
            }
            candidate_edges.push(GroupEdge {
                source_run_id: dependency.clone(),
                target_run_id: run_id.to_string(),
            });
        }
        if adds_cycle(&self.edges, &candidate_edges) {
            return Err("workflow wait would create a cycle".to_string());
        }
        for edge in candidate_edges {
            self.edges.insert(edge);
        }
        self.rebuild_incoming();
        Ok(())
    }

    /// Admission is satisfied only when every dependency completed successfully.
    /// Failed or interrupted dependencies do not release the waiter and do not
    /// cascade into a failure of the waiter.
    pub(super) fn is_ready(&self, run_id: &str) -> bool {
        self.status(run_id) == Some(WorkflowNodeStatus::Pending)
            && self
                .incoming
                .get(run_id)
                .into_iter()
                .flatten()
                .all(|source| self.status(source) == Some(WorkflowNodeStatus::Completed))
    }

    /// Pending runs whose dependencies are all completed, in stable order.
    pub(super) fn ready_pending_run_ids(&self) -> Vec<String> {
        let mut ready = self
            .nodes
            .keys()
            .filter(|run_id| self.is_ready(run_id))
            .cloned()
            .collect::<Vec<_>>();
        ready.sort_by(|left, right| {
            let left_started = self
                .nodes
                .get(left)
                .map(|node| node.started_at_ms)
                .unwrap_or_default();
            let right_started = self
                .nodes
                .get(right)
                .map(|node| node.started_at_ms)
                .unwrap_or_default();
            left_started
                .cmp(&right_started)
                .then_with(|| left.cmp(right))
        });
        ready
    }

    pub(super) fn remember_snapshot(&mut self, snapshot: &WorkflowRunSnapshot) {
        let Some(node) = self.nodes.get_mut(&snapshot.run_id) else {
            return;
        };
        node.status = snapshot.status;
        if snapshot.status != WorkflowNodeStatus::Pending {
            node.started_at_ms = snapshot.started_at_ms;
        }
        node.completed_at_ms = snapshot.completed_at_ms;
        node.message = String::new();
        node.snapshot = Some(snapshot.clone());
        if let Some(stored) = node.snapshot.as_ref() {
            node.output = stored.output.clone();
            node.error = stored.error.clone();
        }
    }

    pub(super) fn active_snapshot(&self) -> WorkflowGroupSnapshot {
        let active = self.active_run_ids();
        let mut nodes = self
            .nodes
            .iter()
            .filter(|(run_id, _)| active.contains(*run_id))
            .map(|(run_id, node)| WorkflowGroupNodeSnapshot {
                run_id: run_id.clone(),
                workflow_id: node.workflow_id.clone(),
                status: node.status,
                started_at_ms: node.started_at_ms,
                completed_at_ms: node.completed_at_ms,
                input: node.input.clone(),
                output: node.output.clone(),
                error: node.error.clone(),
                message: node.message.clone(),
                snapshot: node.snapshot.clone(),
            })
            .collect::<Vec<_>>();
        nodes.sort_by(|left, right| {
            left.started_at_ms
                .cmp(&right.started_at_ms)
                .then_with(|| left.run_id.cmp(&right.run_id))
        });
        let mut edges = self
            .edges
            .iter()
            .filter(|edge| {
                active.contains(&edge.source_run_id) && active.contains(&edge.target_run_id)
            })
            .map(|edge| WorkflowGroupEdgeSnapshot {
                source_run_id: edge.source_run_id.clone(),
                target_run_id: edge.target_run_id.clone(),
            })
            .collect::<Vec<_>>();
        edges.sort_by(|left, right| {
            left.source_run_id
                .cmp(&right.source_run_id)
                .then_with(|| left.target_run_id.cmp(&right.target_run_id))
        });
        WorkflowGroupSnapshot { nodes, edges }
    }

    /// Drop every node whose undirected component has fully settled. History of
    /// individual runs is retained by the existing completion path, not here.
    pub(super) fn prune_settled_components(&mut self) {
        let active_ids = self.active_run_ids();
        let before = self.nodes.len();
        self.nodes.retain(|run_id, _| active_ids.contains(run_id));
        self.edges.retain(|edge| {
            active_ids.contains(&edge.source_run_id) && active_ids.contains(&edge.target_run_id)
        });
        if self.nodes.len() != before {
            self.rebuild_incoming();
        }
    }

    fn active_run_ids(&self) -> HashSet<String> {
        let mut active = HashSet::new();
        for component in connected_components(&self.nodes, &self.edges) {
            let settled = component
                .iter()
                .all(|run_id| self.status(run_id).is_some_and(is_terminal));
            if !settled {
                active.extend(component);
            }
        }
        active
    }

    fn rebuild_incoming(&mut self) {
        self.incoming.clear();
        for edge in &self.edges {
            self.incoming
                .entry(edge.target_run_id.clone())
                .or_default()
                .push(edge.source_run_id.clone());
        }
    }

    fn status(&self, run_id: &str) -> Option<WorkflowNodeStatus> {
        self.nodes.get(run_id).map(|node| node.status)
    }

    pub(super) fn remove_node(&mut self, run_id: &str) {
        self.nodes.remove(run_id);
        self.edges
            .retain(|edge| edge.source_run_id != run_id && edge.target_run_id != run_id);
        self.rebuild_incoming();
    }
}

fn is_terminal(status: WorkflowNodeStatus) -> bool {
    matches!(
        status,
        WorkflowNodeStatus::Completed
            | WorkflowNodeStatus::Failed
            | WorkflowNodeStatus::Interrupted
    )
}

fn adds_cycle(existing: &HashSet<GroupEdge>, extra: &[GroupEdge]) -> bool {
    let mut outgoing: HashMap<String, Vec<String>> = HashMap::new();
    let mut indegree: HashMap<String, usize> = HashMap::new();
    let mut consider = |source: &str, target: &str| {
        indegree.entry(source.to_string()).or_insert(0);
        *indegree.entry(target.to_string()).or_insert(0) += 1;
        outgoing
            .entry(source.to_string())
            .or_default()
            .push(target.to_string());
    };
    for edge in existing {
        consider(&edge.source_run_id, &edge.target_run_id);
    }
    for edge in extra {
        consider(&edge.source_run_id, &edge.target_run_id);
    }
    let mut ready = indegree
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(run_id, _)| run_id.clone())
        .collect::<VecDeque<_>>();
    let mut seen = 0usize;
    while let Some(run_id) = ready.pop_front() {
        seen += 1;
        for child in outgoing.get(&run_id).into_iter().flatten().cloned() {
            if let Some(count) = indegree.get_mut(&child) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    ready.push_back(child);
                }
            }
        }
    }
    seen != indegree.len()
}

fn connected_components(
    nodes: &HashMap<String, GroupNode>,
    edges: &HashSet<GroupEdge>,
) -> Vec<HashSet<String>> {
    let mut adjacent: HashMap<&str, Vec<&str>> = HashMap::new();
    for run_id in nodes.keys() {
        adjacent.entry(run_id.as_str()).or_default();
    }
    for edge in edges {
        adjacent
            .entry(edge.source_run_id.as_str())
            .or_default()
            .push(edge.target_run_id.as_str());
        adjacent
            .entry(edge.target_run_id.as_str())
            .or_default()
            .push(edge.source_run_id.as_str());
    }
    let mut unseen = nodes.keys().map(String::as_str).collect::<HashSet<_>>();
    let mut components = Vec::new();
    while let Some(start) = unseen.iter().copied().next() {
        let mut component = HashSet::new();
        let mut queue = VecDeque::from([start]);
        while let Some(run_id) = queue.pop_front() {
            if !unseen.remove(run_id) {
                continue;
            }
            component.insert(run_id.to_string());
            for peer in adjacent.get(run_id).into_iter().flatten().copied() {
                if unseen.contains(peer) {
                    queue.push_back(peer);
                }
            }
        }
        components.push(component);
    }
    components
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pending(group: &mut WorkflowGroup, run_id: &str) {
        group.insert_pending(run_id, "goal", json!({"goal": run_id}), run_id.len() as i64);
    }

    fn finish(group: &mut WorkflowGroup, run_id: &str, status: WorkflowNodeStatus) {
        let node = group.nodes.get_mut(run_id).expect("node");
        node.status = status;
        node.completed_at_ms = Some(node.started_at_ms + 1);
        node.snapshot = Some(WorkflowRunSnapshot {
            run_id: run_id.to_string(),
            workflow_id: node.workflow_id.clone(),
            status,
            started_at_ms: node.started_at_ms,
            completed_at_ms: node.completed_at_ms,
            input: node.input.clone(),
            output: node.output.clone(),
            error: node.error.clone(),
            await_groups: Vec::new(),
            transitions: Vec::new(),
            workers: Vec::new(),
        });
    }

    #[test]
    fn connected_component_stays_until_every_member_finishes() {
        let mut group = WorkflowGroup::default();
        pending(&mut group, "a");
        pending(&mut group, "b");
        pending(&mut group, "c");
        group.add_waits("b", &["a".to_string()]).unwrap();
        let node = group.nodes.get_mut("a").expect("node");
        node.status = WorkflowNodeStatus::Running;
        finish(&mut group, "a", WorkflowNodeStatus::Completed);
        let active = group.active_snapshot();
        assert!(active.nodes.iter().any(|node| node.run_id == "a"));
        assert!(active.nodes.iter().any(|node| node.run_id == "b"));
        assert_eq!(active.edges.len(), 1);
        assert!(
            active
                .nodes
                .iter()
                .find(|node| node.run_id == "a")
                .and_then(|node| node.snapshot.as_ref())
                .is_some_and(|snapshot| snapshot.status == WorkflowNodeStatus::Completed)
        );

        finish(&mut group, "b", WorkflowNodeStatus::Completed);
        let active = group.active_snapshot();
        assert!(active.nodes.iter().all(|node| node.run_id == "c"));
        assert!(active.edges.is_empty());

        finish(&mut group, "c", WorkflowNodeStatus::Completed);
        assert!(group.active_snapshot().nodes.is_empty());
        group.prune_settled_components();
        assert!(group.nodes.is_empty());
    }

    #[test]
    fn failed_dependency_keeps_blocked_component_and_does_not_release() {
        let mut group = WorkflowGroup::default();
        pending(&mut group, "a");
        pending(&mut group, "b");
        group.add_waits("b", &["a".to_string()]).unwrap();
        finish(&mut group, "a", WorkflowNodeStatus::Failed);
        let active = group.active_snapshot();
        assert_eq!(active.nodes.len(), 2);
        assert!(!group.is_ready("b"));
        assert!(group.ready_pending_run_ids().is_empty());
        assert_eq!(
            group.status("b"),
            Some(WorkflowNodeStatus::Pending),
            "a failed dependency must not cascade into the waiter"
        );

        finish(&mut group, "a", WorkflowNodeStatus::Interrupted);
        assert!(!group.is_ready("b"));
    }

    #[test]
    fn completed_dependency_releases_waiter_without_passing_output() {
        let mut group = WorkflowGroup::default();
        pending(&mut group, "a");
        pending(&mut group, "b");
        group.add_waits("b", &["a".to_string()]).unwrap();
        let node = group.nodes.get_mut("a").expect("node");
        node.output = Some(json!({"secret": true}));
        finish(&mut group, "a", WorkflowNodeStatus::Completed);
        assert!(group.is_ready("b"));
        assert_eq!(group.ready_pending_run_ids(), vec!["b".to_string()]);
        assert!(
            group
                .nodes
                .get("b")
                .and_then(|node| node.output.as_ref())
                .is_none()
        );
    }

    #[test]
    fn cycle_is_rejected_and_existing_edges_remain() {
        let mut group = WorkflowGroup::default();
        pending(&mut group, "a");
        pending(&mut group, "b");
        group.add_waits("b", &["a".to_string()]).unwrap();
        let err = group.add_waits("a", &["b".to_string()]).expect_err("cycle");
        assert!(err.contains("cycle"), "{err}");
        assert_eq!(group.edges.len(), 1);

        let self_err = group
            .add_waits("a", &["a".to_string()])
            .expect_err("self cycle");
        assert!(self_err.contains("itself"), "{self_err}");
        assert_eq!(group.edges.len(), 1);
    }

    #[test]
    fn waits_cannot_be_added_after_the_target_leaves_pending() {
        let mut group = WorkflowGroup::default();
        pending(&mut group, "a");
        pending(&mut group, "b");
        let node = group.nodes.get_mut("b").expect("node");
        node.status = WorkflowNodeStatus::Running;
        let err = group
            .add_waits("b", &["a".to_string()])
            .expect_err("not pending");
        assert!(err.contains("pending"), "{err}");
        assert!(group.edges.is_empty());
    }
}
