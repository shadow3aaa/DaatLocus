use crate::{
    context::Context,
    reasoning::runtime::{PromptMemoryCitation, PromptMemoryFact},
    snapshot::Snapshot,
};

use super::{
    prompt_doc::{PromptBlock, PromptGroupDoc, PromptNode, PromptStateDoc, PromptUnitDoc},
    prompts::{
        APPS_UNIT_HOW, APPS_UNIT_WHAT, APPS_UNIT_WHEN, EVENT_UNIT_HOW, EVENT_UNIT_WHAT,
        MEMORIES_UNIT_HOW, MEMORIES_UNIT_WHAT, MEMORIES_UNIT_WHEN, PLAN_UNIT_HOW, PLAN_UNIT_WHAT,
        PLAN_UNIT_WHEN, WORKFLOW_UNIT_HOW, WORKFLOW_UNIT_WHAT, WORKFLOW_UNIT_WHEN,
        WORKSPACE_UNIT_HOW, WORKSPACE_UNIT_WHEN, WORKSPACE_UNIT_WHY, build_app_usage_prompt,
        build_runtime_app_usages, build_runtime_background_hint_items,
        build_runtime_focused_app_how_to_use_prompt, build_workspace_unit_what,
    },
    turn_compile::load_prompt_persona_spec_sync,
};

pub trait SystemPromptPart: Send + Sync {
    fn key(&self) -> &'static str;
    fn build(&self, ctx: &Context) -> Option<PromptUnitDoc>;
}

pub trait SnapshotPart: Send + Sync {
    fn key(&self) -> &'static str;
    fn build(&self, ctx: &Context, snapshot: &Snapshot) -> Option<PromptNode>;
}

pub struct EventSystemPart;
pub struct AppsSystemPart;
pub struct WorkspaceSystemPart;
pub struct MemoriesSystemPart;
pub struct PlanSystemPart;
pub struct WorkflowSystemPart;
pub struct PersonaSystemPart;
pub struct CompiledAdditionsSystemPart;

pub struct MemoriesSnapshotPart;
pub struct SensorySnapshotPart;
pub struct PlanSnapshotPart;
pub struct WorkflowSnapshotPart;
pub struct EventsSnapshotPart;
pub struct AppSnapshotPart;

impl SystemPromptPart for EventSystemPart {
    fn key(&self) -> &'static str {
        "event"
    }

    fn build(&self, _ctx: &Context) -> Option<PromptUnitDoc> {
        Some(PromptUnitDoc::new(
            self.key(),
            vec![PromptBlock::Paragraph(EVENT_UNIT_WHAT.to_string())],
            Vec::new(),
            Vec::new(),
            vec![PromptBlock::Paragraph(EVENT_UNIT_HOW.to_string())],
        ))
    }
}

impl SystemPromptPart for AppsSystemPart {
    fn key(&self) -> &'static str {
        "apps"
    }

    fn build(&self, _ctx: &Context) -> Option<PromptUnitDoc> {
        Some(PromptUnitDoc::new(
            self.key(),
            vec![PromptBlock::Paragraph(APPS_UNIT_WHAT.to_string())],
            Vec::new(),
            vec![PromptBlock::Paragraph(APPS_UNIT_WHEN.to_string())],
            vec![PromptBlock::Paragraph(APPS_UNIT_HOW.to_string())],
        ))
    }
}

impl SystemPromptPart for WorkspaceSystemPart {
    fn key(&self) -> &'static str {
        "workspace"
    }

    fn build(&self, ctx: &Context) -> Option<PromptUnitDoc> {
        Some(PromptUnitDoc::new(
            self.key(),
            vec![PromptBlock::Paragraph(build_workspace_unit_what(ctx))],
            vec![PromptBlock::Paragraph(WORKSPACE_UNIT_WHY.to_string())],
            vec![PromptBlock::Paragraph(WORKSPACE_UNIT_WHEN.to_string())],
            vec![PromptBlock::Paragraph(WORKSPACE_UNIT_HOW.to_string())],
        ))
    }
}

impl SystemPromptPart for MemoriesSystemPart {
    fn key(&self) -> &'static str {
        "memories"
    }

    fn build(&self, _ctx: &Context) -> Option<PromptUnitDoc> {
        Some(PromptUnitDoc::new(
            self.key(),
            vec![PromptBlock::Paragraph(MEMORIES_UNIT_WHAT.to_string())],
            Vec::new(),
            vec![PromptBlock::Paragraph(MEMORIES_UNIT_WHEN.to_string())],
            vec![PromptBlock::Paragraph(MEMORIES_UNIT_HOW.to_string())],
        ))
    }
}

impl SystemPromptPart for PlanSystemPart {
    fn key(&self) -> &'static str {
        "plan"
    }

    fn build(&self, _ctx: &Context) -> Option<PromptUnitDoc> {
        Some(PromptUnitDoc::new(
            self.key(),
            vec![PromptBlock::Paragraph(PLAN_UNIT_WHAT.to_string())],
            Vec::new(),
            vec![PromptBlock::Paragraph(PLAN_UNIT_WHEN.to_string())],
            vec![PromptBlock::Paragraph(PLAN_UNIT_HOW.to_string())],
        ))
    }
}

impl SystemPromptPart for WorkflowSystemPart {
    fn key(&self) -> &'static str {
        "workflow"
    }

    fn build(&self, _ctx: &Context) -> Option<PromptUnitDoc> {
        Some(PromptUnitDoc::new(
            self.key(),
            vec![PromptBlock::Paragraph(WORKFLOW_UNIT_WHAT.to_string())],
            Vec::new(),
            vec![PromptBlock::Paragraph(WORKFLOW_UNIT_WHEN.to_string())],
            vec![PromptBlock::Paragraph(WORKFLOW_UNIT_HOW.to_string())],
        ))
    }
}

impl SystemPromptPart for PersonaSystemPart {
    fn key(&self) -> &'static str {
        "persona"
    }

    fn build(&self, _ctx: &Context) -> Option<PromptUnitDoc> {
        let persona = load_prompt_persona_spec_sync();
        Some(PromptUnitDoc::new(
            self.key(),
            vec![PromptBlock::KeyValueList(vec![
                ("name".to_string(), persona.name.trim().to_string()),
                ("language".to_string(), persona.language.trim().to_string()),
                (
                    "configured_locale".to_string(),
                    _ctx.config.locale.as_str().to_string(),
                ),
                (
                    "identity_summary".to_string(),
                    persona.identity_summary.trim().to_string(),
                ),
            ])],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ))
    }
}

impl SystemPromptPart for CompiledAdditionsSystemPart {
    fn key(&self) -> &'static str {
        "compiled_additions"
    }

    fn build(&self, ctx: &Context) -> Option<PromptUnitDoc> {
        let additions = ctx
            .compiled_prompts
            .runtime_system_additions()
            .iter()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>();
        if additions.is_empty() {
            return None;
        }
        Some(PromptUnitDoc::new(
            self.key(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![PromptBlock::BulletList(additions)],
        ))
    }
}

impl SnapshotPart for MemoriesSnapshotPart {
    fn key(&self) -> &'static str {
        "recall_memories"
    }

    fn build(&self, ctx: &Context, _snapshot: &Snapshot) -> Option<PromptNode> {
        if ctx.prompt_memory.is_empty() {
            return None;
        }
        let mut children = Vec::new();

        if !ctx.prompt_memory.observations.is_empty() {
            children.push(PromptNode::State(PromptStateDoc::new(
                "observations",
                render_prompt_memory_facts(&ctx.prompt_memory.observations),
            )));
        }
        if !ctx.prompt_memory.raw_memories.is_empty() {
            children.push(PromptNode::State(PromptStateDoc::new(
                "raw_memories",
                render_prompt_memory_facts(&ctx.prompt_memory.raw_memories),
            )));
        }
        if !ctx.prompt_memory.citations.is_empty() {
            children.push(PromptNode::State(PromptStateDoc::new(
                "citations",
                vec![PromptBlock::BulletList(render_prompt_memory_citations(
                    &ctx.prompt_memory.citations,
                ))],
            )));
        }

        if children.is_empty() {
            None
        } else {
            Some(PromptNode::Group(PromptGroupDoc::new(self.key(), children)))
        }
    }
}

impl SnapshotPart for SensorySnapshotPart {
    fn key(&self) -> &'static str {
        "sensory"
    }

    fn build(&self, _ctx: &Context, snapshot: &Snapshot) -> Option<PromptNode> {
        let text = snapshot.sensory_runtime_text();
        if text.trim().is_empty() {
            return None;
        }
        Some(PromptNode::State(PromptStateDoc::new(
            self.key(),
            vec![PromptBlock::Paragraph(text)],
        )))
    }
}

impl SnapshotPart for PlanSnapshotPart {
    fn key(&self) -> &'static str {
        "plan"
    }

    fn build(&self, _ctx: &Context, snapshot: &Snapshot) -> Option<PromptNode> {
        let text = snapshot.plan_runtime_text();
        if text.trim().is_empty() {
            return None;
        }
        Some(PromptNode::State(PromptStateDoc::new(
            self.key(),
            vec![PromptBlock::Paragraph(text)],
        )))
    }
}

impl SnapshotPart for WorkflowSnapshotPart {
    fn key(&self) -> &'static str {
        "workflow"
    }

    fn build(&self, ctx: &Context, _snapshot: &Snapshot) -> Option<PromptNode> {
        let mut blocks = Vec::new();

        if let Some(bound_workflow) = ctx.bound_workflow() {
            let mut pairs = vec![("bound_workflow_id".to_string(), bound_workflow.id.clone())];
            if let Some(origin) = ctx.workflows.workflow_origin(&bound_workflow.id) {
                pairs.push((
                    "bound_workflow_origin".to_string(),
                    format!("{origin:?}").to_ascii_lowercase(),
                ));
            }
            blocks.push(PromptBlock::KeyValueList(pairs));
            if !bound_workflow.workflow_steps.is_empty() {
                blocks.push(PromptBlock::BulletList(
                    bound_workflow
                        .workflow_steps
                        .iter()
                        .take(6)
                        .cloned()
                        .collect(),
                ));
            }
            if !bound_workflow.done_criteria.is_empty() {
                blocks.push(PromptBlock::BulletList(
                    bound_workflow
                        .done_criteria
                        .iter()
                        .take(4)
                        .map(|item| format!("done: {item}"))
                        .collect(),
                ));
            }
            if !bound_workflow.recovery.is_empty() {
                blocks.push(PromptBlock::BulletList(
                    bound_workflow
                        .recovery
                        .iter()
                        .take(4)
                        .map(|item| format!("recovery: {item}"))
                        .collect(),
                ));
            }
        } else {
            blocks.push(PromptBlock::Paragraph(
                "[Current phase: workflow selection] bound_workflow_id=<none>. Before executing any task, call `activate_workflow` to choose the best candidate workflow, or call `create_workflow` to create a new workflow. Only those two tools are currently available."
                    .to_string(),
            ));
        }

        let summaries = ctx.workflows.summaries(8);
        if !summaries.is_empty() {
            blocks.push(PromptBlock::BulletList(
                summaries
                    .into_iter()
                    .map(|summary| {
                        let prefix = match summary.origin {
                            crate::workflow::WorkflowOrigin::Builtin => "[builtin] ",
                            crate::workflow::WorkflowOrigin::Workspace => "[workspace] ",
                        };
                        if summary.when_to_use_summary.is_empty() {
                            format!("{prefix}{}", summary.id)
                        } else {
                            format!(
                                "{prefix}{} | when={}",
                                summary.id, summary.when_to_use_summary
                            )
                        }
                    })
                    .collect(),
            ));
        }

        Some(PromptNode::State(PromptStateDoc::new(self.key(), blocks)))
    }
}

impl SnapshotPart for EventsSnapshotPart {
    fn key(&self) -> &'static str {
        "events"
    }

    fn build(&self, _ctx: &Context, snapshot: &Snapshot) -> Option<PromptNode> {
        let text = snapshot.events_runtime_text();
        if text.trim().is_empty() {
            return None;
        }
        Some(PromptNode::State(PromptStateDoc::new(
            self.key(),
            vec![PromptBlock::Paragraph(text)],
        )))
    }
}

impl SnapshotPart for AppSnapshotPart {
    fn key(&self) -> &'static str {
        "app"
    }

    fn build(&self, ctx: &Context, snapshot: &Snapshot) -> Option<PromptNode> {
        let mut children = Vec::new();

        let focused = snapshot.focused_app_runtime_text();
        let mut other_app_children = Vec::new();

        let app_usages = build_runtime_app_usages(ctx);
        if !app_usages.is_empty() {
            let app_groups = app_usages
                .into_iter()
                .map(|(app_id, usage)| {
                    PromptNode::State(PromptStateDoc::new(
                        app_id.to_string(),
                        vec![PromptBlock::Paragraph(build_app_usage_prompt(
                            app_id, &usage,
                        ))],
                    ))
                })
                .collect::<Vec<_>>();
            other_app_children.extend(app_groups);
        }

        let background_hints = build_runtime_background_hint_items(ctx);
        if !background_hints.is_empty() {
            other_app_children.push(PromptNode::State(PromptStateDoc::new(
                "background_hints",
                vec![PromptBlock::BulletList(background_hints)],
            )));
        }

        if !other_app_children.is_empty() {
            children.push(PromptNode::Group(PromptGroupDoc::new(
                "other_apps",
                other_app_children,
            )));
        }

        let mut focused_app_children = Vec::new();
        let app_entries = snapshot.app_state_entries();
        if !app_entries.is_empty() {
            let mut blocks = Vec::new();
            for entry in app_entries {
                blocks.push(PromptBlock::KeyValueList(vec![
                    ("id".to_string(), entry.app_id),
                    ("title".to_string(), entry.title),
                ]));
                if !entry.lines.is_empty() {
                    blocks.push(PromptBlock::BulletList(entry.lines));
                }
            }
            focused_app_children.push(PromptNode::State(PromptStateDoc::new("state", blocks)));
        }

        if let Some(how_to_use) = build_runtime_focused_app_how_to_use_prompt(ctx)
            && !how_to_use.trim().is_empty()
        {
            focused_app_children.push(PromptNode::State(PromptStateDoc::new(
                "how_to_use",
                vec![PromptBlock::Paragraph(how_to_use)],
            )));
        }

        if !focused_app_children.is_empty() {
            children.push(PromptNode::Group(PromptGroupDoc::new(
                "focused_app",
                vec![PromptNode::Group(PromptGroupDoc::new(
                    focused.clone(),
                    focused_app_children,
                ))],
            )));
        }

        if children.is_empty() {
            return None;
        }

        Some(PromptNode::Group(PromptGroupDoc::new(self.key(), children)))
    }
}

fn render_prompt_memory_facts(facts: &[PromptMemoryFact]) -> Vec<PromptBlock> {
    facts
        .iter()
        .map(|fact| {
            let kind = fact
                .memory_type
                .clone()
                .unwrap_or_else(|| "memory".to_string());
            let context = fact
                .context
                .clone()
                .filter(|value| !value.trim().is_empty())
                .map(|value| format!("\ncontext: {value}"))
                .unwrap_or_default();
            PromptBlock::Paragraph(format!(
                "id: {}\ntype: {}\ntext: {}{}",
                fact.id,
                kind,
                fact.text.trim(),
                context
            ))
        })
        .collect()
}

fn render_prompt_memory_citations(citations: &[PromptMemoryCitation]) -> Vec<String> {
    citations
        .iter()
        .map(|citation| format!("[{}] {}: {}", citation.kind, citation.id, citation.summary))
        .collect()
}
