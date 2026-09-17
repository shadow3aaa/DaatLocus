//! Study Mode capability domain: knowledge graph, question bank, progress.
//!
//! The `Study` app is installed only in study sessions. It exposes the
//! `study__*` tool surface and generated `study__get_state`, renders the graph
//! state for dashboards, and carries the mode contract in its docs.

pub mod store;

use std::borrow::Cow;
use std::fmt::Write as _;

use async_trait::async_trait;
use daat_locus_macros::model_schema;
use miette::{Result, miette};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    app::{
        App, AppDocs, AppId, AppStateRender, AppToolExecutionContext, AppToolExecutionResult,
        AppToolSpec,
    },
    reasoning::{episode::EpisodeActionRecord, prompts::APP_STUDY, runtime::AgentToolCall},
    schema_utils::{ModelSchema, model_schema_for},
};

use self::store::{
    CreateNodeInput, NewQuestionInput, StudyFindSimilar, StudyStore, StudyWriteOrigin,
    UpdateNodeInput,
};

#[model_schema]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StudyRelation {
    Prerequisite,
    PartOf,
    Related,
    Contrast,
    ExampleOf,
    AppliesTo,
}

impl StudyRelation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Prerequisite => "prerequisite",
            Self::PartOf => "part_of",
            Self::Related => "related",
            Self::Contrast => "contrast",
            Self::ExampleOf => "example_of",
            Self::AppliesTo => "applies_to",
        }
    }
}

impl JsonSchema for StudyRelation {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        "StudyRelation".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "enum": ["prerequisite", "part_of", "related", "contrast", "example_of", "applies_to"],
        })
    }
}

#[model_schema]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StudyAttemptOutcome {
    Correct,
    Partial,
    Incorrect,
}

impl StudyAttemptOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Correct => "correct",
            Self::Partial => "partial",
            Self::Incorrect => "incorrect",
        }
    }
}

impl JsonSchema for StudyAttemptOutcome {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        "StudyAttemptOutcome".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "enum": ["correct", "partial", "incorrect"],
        })
    }
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct StudySourceArg {
    pub title: String,
    pub url: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct StudyListModulesArgs {}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct StudySearchNodesArgs {
    pub query: String,
    /// Restrict the search to one module id; null searches the whole graph.
    pub module_id: Option<String>,
    pub limit: Option<u64>,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct StudyReadNodeArgs {
    pub node_id: String,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct StudyCreateModuleArgs {
    pub title: String,
    pub description: String,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct StudyCreateNodeArgs {
    pub module_id: String,
    pub title: String,
    pub summary: String,
    pub body: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// At least one source is required for every new node.
    pub sources: Vec<StudySourceArg>,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct StudyUpdateNodeArgs {
    pub node_id: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub body: Option<String>,
    pub aliases: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
    pub sources: Option<Vec<StudySourceArg>>,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct StudyLinkNodesArgs {
    pub from_node_id: String,
    pub to_node_id: String,
    pub relation: StudyRelation,
    pub note: Option<String>,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct StudyUnlinkNodesArgs {
    pub from_node_id: String,
    pub to_node_id: String,
    /// Remove only this relation type; null removes every relation between the two nodes.
    pub relation: Option<StudyRelation>,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct StudyMergeNodesArgs {
    /// Duplicate node that should disappear after the merge.
    pub merged_node_id: String,
    /// Canonical node that keeps the identity.
    pub canonical_node_id: String,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct StudyFindSimilarArgs {
    pub title_or_node_id: String,
    pub limit: Option<u64>,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct StudyQuestionArg {
    pub question: String,
    pub answer: String,
    /// `easy`, `medium`, or `hard`; defaults to `medium`.
    pub difficulty: Option<String>,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct StudyAddQuestionsArgs {
    pub node_id: String,
    pub questions: Vec<StudyQuestionArg>,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct StudyRecordAttemptArgs {
    pub question_id: String,
    pub outcome: StudyAttemptOutcome,
    pub note: Option<String>,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct StudyUpdateProgressArgs {
    pub node_id: String,
    /// Understanding level as a percentage between 0 and 100.
    pub understanding: u64,
    /// Short reason, such as `user_declared` or `quiz_passed`.
    pub evidence: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct StudyMaintenanceReportArgs {}

pub struct StudyApp {
    store: StudyStore,
}

impl StudyApp {
    pub fn new(store: StudyStore) -> Self {
        Self { store }
    }

    #[cfg(test)]
    pub fn store(&self) -> &StudyStore {
        &self.store
    }
}

fn parse_args<T>(call: &AgentToolCall) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(call.arguments.clone()).map_err(|err| {
        miette!(
            "invalid arguments for study tool `{}`: {err}; received: {}",
            call.name,
            call.arguments
        )
    })
}

fn result_payload(
    summary: impl Into<String>,
    payload: Value,
    lines: Vec<String>,
) -> AppToolExecutionResult {
    let summary = summary.into();
    let mut result = AppToolExecutionResult::from_activity_event(summary, payload, None, None);
    if !lines.is_empty() {
        result.summary = format!("{}: {}", result.summary, lines.join(", "));
    }
    result
}

impl StudyApp {
    fn render_snapshot_lines(&self) -> Result<Vec<String>> {
        let snapshot = self.store.graph_snapshot()?;
        let stats = &snapshot.stats;
        let mut lines = vec![
            "kind=study".to_string(),
            format!(
                "modules={} nodes={} edges={}",
                stats.module_count, stats.node_count, stats.edge_count
            ),
            format!(
                "understanding avg={}% mastered={} in_progress={} unseen={}",
                stats.average_understanding,
                stats.mastered_count,
                stats.in_progress_count,
                stats.unseen_count
            ),
            format!(
                "questions={} stale_questions={} attempts={}",
                stats.question_count, stats.stale_question_count, stats.attempt_count
            ),
        ];
        let maintenance = &snapshot.maintenance;
        if !maintenance.orphan_node_ids.is_empty()
            || !maintenance.empty_module_ids.is_empty()
            || !maintenance.duplicate_candidate_groups.is_empty()
        {
            lines.push(format!(
                "maintenance orphan_nodes={} empty_modules={} duplicate_groups={}",
                maintenance.orphan_node_ids.len(),
                maintenance.empty_module_ids.len(),
                maintenance.duplicate_candidate_groups.len()
            ));
        }
        Ok(lines)
    }
}

#[async_trait]
impl App for StudyApp {
    fn id(&self) -> AppId {
        AppId::study()
    }

    fn render_state(&self) -> AppStateRender {
        let lines = self
            .render_snapshot_lines()
            .unwrap_or_else(|err| vec!["kind=study".to_string(), format!("error={err}")]);
        AppStateRender {
            title: "Study".to_string(),
            lines,
        }
    }

    fn docs(&self) -> AppDocs {
        APP_STUDY.app_docs()
    }

    fn state_revision(&self) -> Option<i64> {
        Some(self.store.revision())
    }

    fn tool_specs(&self) -> Vec<AppToolSpec> {
        vec![
            app_tool_with_schema(
                "list_modules",
                "List study modules with node counts and progress distribution.",
                empty_object_schema(),
            ),
            app_tool::<StudySearchNodesArgs>(
                "search_nodes",
                "Search knowledge nodes by title, alias, summary, or body text; optionally scoped to one module.",
            ),
            app_tool::<StudyReadNodeArgs>(
                "read_node",
                "Read one node: full body, progress, questions, and neighbors grouped by relation.",
            ),
            app_tool::<StudyCreateModuleArgs>(
                "create_module",
                "Create a knowledge module (a new domain or learning field).",
            ),
            app_tool::<StudyCreateNodeArgs>(
                "create_node",
                "Create a knowledge node inside a module. New nodes require at least one source. Returns similar existing nodes so you can merge instead of duplicating.",
            ),
            app_tool::<StudyUpdateNodeArgs>(
                "update_node",
                "Update a node's title, summary, body, aliases, tags, or sources. Content changes bump the content version and make existing questions stale.",
            ),
            app_tool::<StudyLinkNodesArgs>(
                "link_nodes",
                "Create a typed relation between two nodes. `prerequisite` edges must stay acyclic.",
            ),
            app_tool::<StudyUnlinkNodesArgs>(
                "unlink_nodes",
                "Remove a relation between two nodes when the structure needs correcting.",
            ),
            app_tool::<StudyMergeNodesArgs>(
                "merge_nodes",
                "Merge a duplicate node into a canonical node: moves relations, aliases, tags, sources, questions, and progress.",
            ),
            app_tool::<StudyFindSimilarArgs>(
                "find_similar",
                "Find duplicate candidates for a title, alias, or existing node before creating or merging.",
            ),
            app_tool::<StudyAddQuestionsArgs>(
                "add_questions",
                "Append questions to a node's question bank. Questions record the node content version they came from.",
            ),
            app_tool::<StudyRecordAttemptArgs>(
                "record_attempt",
                "Record the outcome of a question attempt after grading the user's answer.",
            ),
            app_tool::<StudyUpdateProgressArgs>(
                "update_progress",
                "Set a node's understanding percentage (0-100) with evidence. Use this after an assessment or when the user directly declares a level.",
            ),
            app_tool_with_schema(
                "maintenance_report",
                "Report orphan nodes, empty modules, duplicate candidates, and stale questions.",
                empty_object_schema(),
            ),
        ]
    }

    fn summarize_tool_call(&self, call: &AgentToolCall) -> Result<EpisodeActionRecord> {
        Ok(EpisodeActionRecord {
            kind: call.name.clone(),
            summary: summarize_study_call(call),
        })
    }

    /// Study graph changes are visualized in the Study tab's network and
    /// sidebar, so study tool calls do not add conversation activity rows.
    fn tool_call_activity_event(
        &self,
        _call: &AgentToolCall,
    ) -> Result<Option<crate::activity_event::ToolCallActivityEvent>> {
        Ok(None)
    }

    async fn execute_tool(
        &mut self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<AppToolExecutionResult> {
        let result = self.run_tool(call).await?;
        self.publish_state_revision(context);
        Ok(result)
    }
}

impl StudyApp {
    /// Push the current graph revision to the dashboard so clients can refetch
    /// the network while a turn is still running.
    fn publish_state_revision(&self, context: &AppToolExecutionContext) {
        let Some(tx) = context.dashboard_tx.as_ref() else {
            return;
        };
        let revision = self.store.revision();
        tx.send_modify(|state| {
            state.set_app_state_revision(AppId::study().as_str(), revision);
        });
    }

    async fn run_tool(&self, call: &AgentToolCall) -> Result<AppToolExecutionResult> {
        match call.name.as_str() {
            "list_modules" => {
                let _: StudyListModulesArgs = parse_args(call)?;
                let modules = self.store.list_modules()?;
                let lines = modules
                    .iter()
                    .map(|module| {
                        format!(
                            "{} nodes={} mastered={} in_progress={} avg={}%",
                            module.module.title,
                            module.node_count,
                            module.mastered_count,
                            module.in_progress_count,
                            module.average_understanding
                        )
                    })
                    .collect::<Vec<_>>();
                Ok(result_payload(
                    "listed modules",
                    json!({ "modules": modules }),
                    lines,
                ))
            }
            "search_nodes" => {
                let args: StudySearchNodesArgs = parse_args(call)?;
                let nodes = self.store.search_nodes(
                    &args.query,
                    args.module_id.as_deref(),
                    args.limit.unwrap_or(20) as usize,
                )?;
                let lines = nodes
                    .iter()
                    .map(|node| format!("{} [{}%]", node.title, node.progress.understanding))
                    .collect::<Vec<_>>();
                Ok(result_payload(
                    format!("found {} nodes", nodes.len()),
                    json!({ "nodes": nodes }),
                    lines,
                ))
            }
            "read_node" => {
                let args: StudyReadNodeArgs = parse_args(call)?;
                let detail = self.store.node_detail(&args.node_id)?;
                let lines = vec![
                    format!("title={}", detail.node.title),
                    format!(
                        "understanding={}% questions={} neighbors={}",
                        detail.progress.understanding,
                        detail.questions.len(),
                        detail.neighbors.len()
                    ),
                ];
                Ok(result_payload(
                    format!("read node {}", detail.node.title),
                    serde_json::to_value(&detail).unwrap_or(Value::Null),
                    lines,
                ))
            }
            "create_module" => {
                let args: StudyCreateModuleArgs = parse_args(call)?;
                let module = self.store.create_module(
                    &args.title,
                    &args.description,
                    StudyWriteOrigin::Agent,
                )?;
                Ok(result_payload(
                    format!("created module {}", module.title),
                    json!({ "module": module }),
                    vec![format!("title={}", module.title)],
                ))
            }
            "create_node" => {
                let args: StudyCreateNodeArgs = parse_args(call)?;
                let result = self.store.create_node(
                    &CreateNodeInput {
                        module_id: args.module_id,
                        title: args.title,
                        summary: args.summary,
                        body: args.body,
                        aliases: args.aliases,
                        tags: args.tags,
                        sources: args
                            .sources
                            .into_iter()
                            .map(|source| store::StudySource {
                                title: source.title,
                                url: source.url,
                            })
                            .collect(),
                    },
                    StudyWriteOrigin::Agent,
                )?;
                let mut lines = vec![format!("title={}", result.node.title)];
                if !result.similar.is_empty() {
                    lines.push(format!("similar_existing={}", result.similar.len()));
                }
                Ok(result_payload(
                    format!("created node {}", result.node.title),
                    serde_json::to_value(&result).unwrap_or(Value::Null),
                    lines,
                ))
            }
            "update_node" => {
                let args: StudyUpdateNodeArgs = parse_args(call)?;
                let result = self.store.update_node(
                    &args.node_id,
                    &UpdateNodeInput {
                        title: args.title,
                        summary: args.summary,
                        body: args.body,
                        aliases: args.aliases,
                        tags: args.tags,
                        sources: args.sources.map(|sources| {
                            sources
                                .into_iter()
                                .map(|source| store::StudySource {
                                    title: source.title,
                                    url: source.url,
                                })
                                .collect()
                        }),
                    },
                )?;
                Ok(result_payload(
                    format!("updated node {}", result.node.title),
                    serde_json::to_value(&result).unwrap_or(Value::Null),
                    vec![format!(
                        "content_version={} changed={}",
                        result.node.content_version, result.content_changed
                    )],
                ))
            }
            "link_nodes" => {
                let args: StudyLinkNodesArgs = parse_args(call)?;
                let result = self.store.link_nodes(
                    &args.from_node_id,
                    &args.to_node_id,
                    args.relation.as_str(),
                    args.note.as_deref().unwrap_or_default(),
                )?;
                Ok(result_payload(
                    format!("linked nodes ({})", args.relation.as_str()),
                    serde_json::to_value(&result).unwrap_or(Value::Null),
                    vec![format!(
                        "{} -> {} ({})",
                        args.from_node_id,
                        args.to_node_id,
                        args.relation.as_str()
                    )],
                ))
            }
            "unlink_nodes" => {
                let args: StudyUnlinkNodesArgs = parse_args(call)?;
                let result = self.store.unlink_nodes(
                    &args.from_node_id,
                    &args.to_node_id,
                    args.relation.map(StudyRelation::as_str),
                )?;
                Ok(result_payload(
                    format!("removed {} relations", result.removed_edges),
                    serde_json::to_value(&result).unwrap_or(Value::Null),
                    vec![format!("{} -/-> {}", args.from_node_id, args.to_node_id)],
                ))
            }
            "merge_nodes" => {
                let args: StudyMergeNodesArgs = parse_args(call)?;
                let result = self
                    .store
                    .merge_nodes(&args.merged_node_id, &args.canonical_node_id)?;
                Ok(result_payload(
                    format!("merged into {}", result.canonical.title),
                    serde_json::to_value(&result).unwrap_or(Value::Null),
                    vec![format!(
                        "merged={} moved_edges={} moved_questions={}",
                        result.merged_node_id, result.moved_edges, result.moved_questions
                    )],
                ))
            }
            "find_similar" => {
                let args: StudyFindSimilarArgs = parse_args(call)?;
                let candidates = self
                    .store
                    .find_similar(&args.title_or_node_id, args.limit.unwrap_or(8) as usize)?;
                let lines = candidates
                    .iter()
                    .map(|candidate| format!("{} [{}]", candidate.title, candidate.id))
                    .collect::<Vec<_>>();
                Ok(result_payload(
                    format!("found {} candidates", candidates.len()),
                    serde_json::to_value(StudyFindSimilar { candidates }).unwrap_or(Value::Null),
                    lines,
                ))
            }
            "add_questions" => {
                let args: StudyAddQuestionsArgs = parse_args(call)?;
                let questions = args
                    .questions
                    .into_iter()
                    .map(|question| NewQuestionInput {
                        question: question.question,
                        answer: question.answer,
                        difficulty: question.difficulty.unwrap_or_default(),
                    })
                    .collect::<Vec<_>>();
                let result = self.store.add_questions(&args.node_id, &questions)?;
                Ok(result_payload(
                    format!("added {} questions", result.added_questions.len()),
                    serde_json::to_value(&result).unwrap_or(Value::Null),
                    vec![format!("stale_questions={}", result.stale_question_count)],
                ))
            }
            "record_attempt" => {
                let args: StudyRecordAttemptArgs = parse_args(call)?;
                let result = self.store.record_attempt(
                    &args.question_id,
                    args.outcome.as_str(),
                    args.note.as_deref().unwrap_or_default(),
                )?;
                Ok(result_payload(
                    format!("recorded attempt ({})", result.outcome),
                    serde_json::to_value(&result).unwrap_or(Value::Null),
                    vec![format!("question={}", result.question_id)],
                ))
            }
            "update_progress" => {
                let args: StudyUpdateProgressArgs = parse_args(call)?;
                let result = self.store.update_progress(
                    &args.node_id,
                    args.understanding as i64,
                    args.evidence.as_deref().unwrap_or_default(),
                    StudyWriteOrigin::Agent,
                )?;
                Ok(result_payload(
                    format!("understanding={}%", result.understanding),
                    serde_json::to_value(&result).unwrap_or(Value::Null),
                    vec![format!(
                        "node={} understanding={}%",
                        result.node_id, result.understanding
                    )],
                ))
            }
            "maintenance_report" => {
                let _: StudyMaintenanceReportArgs = parse_args(call)?;
                let report = self.store.maintenance_report()?;
                let lines = vec![
                    format!("orphan_nodes={}", report.orphan_node_ids.len()),
                    format!("empty_modules={}", report.empty_module_ids.len()),
                    format!(
                        "duplicate_groups={}",
                        report.duplicate_candidate_groups.len()
                    ),
                    format!("stale_questions={}", report.stale_question_count),
                ];
                Ok(result_payload(
                    "maintenance report",
                    serde_json::to_value(&report).unwrap_or(Value::Null),
                    lines,
                ))
            }
            other => Err(miette!("unknown study tool `{other}`")),
        }
    }
}

fn app_tool<T: ModelSchema>(name: &str, description: &str) -> AppToolSpec {
    app_tool_with_schema(name, description, model_schema_for::<T>())
}

fn app_tool_with_schema(name: &str, description: &str, input_schema: Value) -> AppToolSpec {
    AppToolSpec {
        name: name.to_string(),
        description: description.to_string(),
        input_schema,
    }
}

fn empty_object_schema() -> Value {
    json!({
        "type": "object",
        "properties": {},
        "required": [],
        "additionalProperties": false,
    })
}

fn summarize_study_call(call: &AgentToolCall) -> String {
    const MAX_CHARS: usize = 120;
    let mut summary = String::new();
    if let Value::Object(map) = &call.arguments {
        for (key, value) in map.iter().take(4) {
            if value.is_null() {
                continue;
            }
            if !summary.is_empty() {
                summary.push(' ');
            }
            let _ = write!(summary, "{key}={}", value);
        }
    }
    if summary.is_empty() {
        summary = call.name.clone();
    }
    if summary.chars().count() > MAX_CHARS {
        let truncated: String = summary.chars().take(MAX_CHARS).collect();
        format!("{truncated}...")
    } else {
        summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema_utils::validate_model_facing_schema;

    fn test_app() -> (tempfile::TempDir, StudyApp) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store =
            StudyStore::open_at(dir.path().join("graph.sqlite3")).expect("open study store");
        (dir, StudyApp::new(store))
    }

    fn call(name: &str, arguments: Value) -> AgentToolCall {
        AgentToolCall {
            id: "study-test-call".to_string(),
            name: name.to_string(),
            arguments,
        }
    }

    #[test]
    fn study_tool_schemas_are_portable_model_schemas() {
        let (_dir, app) = test_app();
        let specs = app.tool_specs();
        assert!(specs.len() >= 13);
        for spec in &specs {
            validate_model_facing_schema(&spec.input_schema)
                .unwrap_or_else(|err| panic!("tool `{}` schema invalid: {err}", spec.name));
        }
        let relation_schema = specs
            .iter()
            .find(|spec| spec.name == "link_nodes")
            .map(|spec| spec.input_schema.clone())
            .expect("link_nodes spec");
        assert_eq!(
            relation_schema["properties"]["relation"]["enum"],
            json!([
                "prerequisite",
                "part_of",
                "related",
                "contrast",
                "example_of",
                "applies_to"
            ])
        );
    }

    #[test]
    fn app_id_study_uses_expected_namespace() {
        assert_eq!(AppId::study().as_str(), "study");
        assert_eq!(
            AppId::study().mangle_tool_name("search_nodes"),
            "study__search_nodes"
        );
    }

    #[tokio::test]
    async fn create_node_tool_requires_sources() {
        let (_dir, mut app) = test_app();
        let module = app
            .store()
            .create_module("Algebra", "", StudyWriteOrigin::Agent)
            .expect("module");
        let error = app
            .execute_tool(
                &call(
                    "create_node",
                    json!({
                        "module_id": module.id,
                        "title": "Group",
                        "summary": "",
                        "body": "",
                        "aliases": [],
                        "tags": [],
                        "sources": []
                    }),
                ),
                &test_context(),
            )
            .await
            .expect_err("sources required");
        assert!(
            error.to_string().contains("source"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn progress_tool_updates_state_render() {
        let (_dir, mut app) = test_app();
        let module = app
            .store()
            .create_module("Algebra", "", StudyWriteOrigin::Agent)
            .expect("module");
        let node = app
            .store()
            .create_node(
                &CreateNodeInput {
                    module_id: module.id,
                    title: "Group".to_string(),
                    summary: String::new(),
                    body: String::new(),
                    aliases: Vec::new(),
                    tags: Vec::new(),
                    sources: vec![store::StudySource {
                        title: "textbook".to_string(),
                        url: "https://example.com".to_string(),
                    }],
                },
                StudyWriteOrigin::Agent,
            )
            .expect("node")
            .node;

        let result = app
            .execute_tool(
                &call(
                    "update_progress",
                    json!({
                        "node_id": node.id,
                        "understanding": 88,
                        "evidence": "user_declared"
                    }),
                ),
                &test_context(),
            )
            .await
            .expect("update progress");
        assert_eq!(result.payload["understanding"], 88);

        let state = app.render_state();
        assert!(
            state.lines.iter().any(|line| line.contains("avg=88%")),
            "state render missing progress: {state:?}"
        );
    }

    fn test_context() -> AppToolExecutionContext {
        AppToolExecutionContext {
            execution_cwd: std::env::temp_dir(),
            sandbox_policy: crate::sandbox::RuntimeSandboxPolicy {
                filesystem: crate::sandbox::FileSystemSandboxPolicy {
                    full_disk_read: true,
                    full_disk_write: true,
                    readable_roots: Vec::new(),
                    writable_roots: Vec::new(),
                    deny_read_paths: Vec::new(),
                    deny_write_paths: Vec::new(),
                },
                protected_env_vars: Vec::new(),
                strong_filesystem: crate::sandbox::StrongFilesystemSandboxMode::Off,
            },
            dashboard_tx: None,
            tool_output_max_tokens: 4096,
            turn_epoch: 0,
        }
    }
}
