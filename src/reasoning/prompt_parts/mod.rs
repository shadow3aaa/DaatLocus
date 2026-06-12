use crate::{
    app::{AppComposedSurface, AppId},
    context::Context,
    events::{EventPayload, EventView},
    preturn_state::PreTurnState,
};

use super::{
    prompt_doc::{PromptBlock, PromptGroupDoc, PromptNode, PromptStateDoc},
    prompts::build_runtime_background_hint_items,
};

pub trait PreTurnContextPart: Send + Sync {
    fn key(&self) -> &'static str;
    fn build(&self, ctx: &Context, state: &PreTurnState) -> Option<PromptNode>;
}

pub trait AfterClaimContextPart: Send + Sync {
    fn key(&self) -> &'static str;
    fn build(&self, ctx: &Context, input: &AfterClaimContextInput) -> Option<PromptNode>;
}

#[derive(Clone, Default)]
pub struct AfterClaimContextInput {
    pub events: Vec<EventView>,
    pub app_notices: Vec<(AppId, String)>,
}

impl AfterClaimContextInput {
    pub fn is_empty(&self) -> bool {
        self.events.is_empty() && self.app_notices.is_empty()
    }
}

pub struct PreTurnSensoryPart;
pub struct PreTurnPlanPart;
pub struct PreTurnWorkflowStatePart;
pub struct PreTurnAppSurfacePart;
pub struct AfterClaimInputPart;
pub struct AfterClaimWorkflowPrimitiveRoutingPart;

impl PreTurnContextPart for PreTurnSensoryPart {
    fn key(&self) -> &'static str {
        "sensory"
    }

    fn build(&self, _ctx: &Context, state: &PreTurnState) -> Option<PromptNode> {
        let text = state.sensory_runtime_text();
        if text.trim().is_empty() {
            return None;
        }
        Some(PromptNode::State(PromptStateDoc::new(
            self.key(),
            vec![PromptBlock::Paragraph(text)],
        )))
    }
}

impl PreTurnContextPart for PreTurnPlanPart {
    fn key(&self) -> &'static str {
        "plan"
    }

    fn build(&self, _ctx: &Context, state: &PreTurnState) -> Option<PromptNode> {
        let text = state.plan_runtime_text();
        if text.trim().is_empty() {
            return None;
        }
        Some(PromptNode::State(PromptStateDoc::new(
            self.key(),
            vec![PromptBlock::Paragraph(text)],
        )))
    }
}

impl PreTurnContextPart for PreTurnWorkflowStatePart {
    fn key(&self) -> &'static str {
        "primitive"
    }

    fn build(&self, ctx: &Context, _state: &PreTurnState) -> Option<PromptNode> {
        let mut blocks = Vec::new();

        let composition_specs = ctx.bound_primitive_composition_specs();
        if let Some(composition) = ctx.bound_primitive_composition.as_ref() {
            blocks.push(PromptBlock::KeyValueList(vec![
                (
                    "bound_primitive_id".to_string(),
                    composition.composition_id.clone(),
                ),
                (
                    "bound_primitive_kind".to_string(),
                    "composition".to_string(),
                ),
                (
                    "primitive_ids".to_string(),
                    composition.primitive_ids.join(", "),
                ),
            ]));

            for primitive in composition_specs.iter().take(4) {
                blocks.push(PromptBlock::Paragraph(format!(
                    "primitive {}:",
                    primitive.id
                )));
                if !primitive.primitive_steps.is_empty() {
                    blocks.push(PromptBlock::BulletList(
                        primitive.primitive_steps.iter().take(4).cloned().collect(),
                    ));
                }
                if !primitive.done_criteria.is_empty() {
                    blocks.push(PromptBlock::BulletList(
                        primitive
                            .done_criteria
                            .iter()
                            .take(2)
                            .map(|item| format!("done: {item}"))
                            .collect(),
                    ));
                }
            }
        } else if let Some(bound_primitive) = ctx.bound_primitive() {
            let mut pairs = vec![("bound_primitive_id".to_string(), bound_primitive.id.clone())];
            pairs.push(("bound_primitive_kind".to_string(), "single".to_string()));
            if let Some(origin) = ctx.workflows.workflow_origin(&bound_primitive.id) {
                pairs.push((
                    "bound_primitive_origin".to_string(),
                    format!("{origin:?}").to_ascii_lowercase(),
                ));
            }
            blocks.push(PromptBlock::KeyValueList(pairs));
            if !bound_primitive.primitive_steps.is_empty() {
                blocks.push(PromptBlock::BulletList(
                    bound_primitive
                        .primitive_steps
                        .iter()
                        .take(6)
                        .cloned()
                        .collect(),
                ));
            }
            if !bound_primitive.done_criteria.is_empty() {
                blocks.push(PromptBlock::BulletList(
                    bound_primitive
                        .done_criteria
                        .iter()
                        .take(4)
                        .map(|item| format!("done: {item}"))
                        .collect(),
                ));
            }
            if !bound_primitive.recovery.is_empty() {
                blocks.push(PromptBlock::BulletList(
                    bound_primitive
                        .recovery
                        .iter()
                        .take(4)
                        .map(|item| format!("recovery: {item}"))
                        .collect(),
                ));
            }
        } else {
            blocks.push(PromptBlock::KeyValueList(vec![(
                "bound_primitive_id".to_string(),
                "<none>".to_string(),
            )]));
        }

        Some(PromptNode::State(PromptStateDoc::new(self.key(), blocks)))
    }
}

impl PreTurnContextPart for PreTurnAppSurfacePart {
    fn key(&self) -> &'static str {
        "app"
    }

    fn build(&self, ctx: &Context, state: &PreTurnState) -> Option<PromptNode> {
        let mut children = Vec::new();

        let focused = state.focused_app_runtime_text();
        let composed_apps = ctx.apps.focused_composed_surfaces();

        let mut other_app_children = Vec::new();

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
        let app_entries = state.app_state_entries();
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

        let composed_app_children = composed_apps
            .iter()
            .filter_map(|surface| build_composed_app_surface_node(ctx, state, surface))
            .collect::<Vec<_>>();
        if !composed_app_children.is_empty() {
            focused_app_children.push(PromptNode::Group(PromptGroupDoc::new(
                "composed_apps",
                composed_app_children,
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

fn build_composed_app_surface_node(
    _ctx: &Context,
    state: &PreTurnState,
    surface: &AppComposedSurface,
) -> Option<PromptNode> {
    let mut children = Vec::new();
    children.push(PromptNode::State(PromptStateDoc::new(
        "composition",
        vec![PromptBlock::KeyValueList(vec![
            ("role".to_string(), surface.role.clone()),
            (
                "exposed_tools".to_string(),
                render_bracketed_values(surface.exposed_tools.iter().cloned()),
            ),
        ])],
    )));

    if let Some(entry) = state.full_app_state_entry(&surface.app_id) {
        let mut blocks = vec![PromptBlock::KeyValueList(vec![
            ("id".to_string(), entry.app_id),
            ("title".to_string(), entry.title),
        ])];
        if !entry.lines.is_empty() {
            blocks.push(PromptBlock::BulletList(entry.lines));
        }
        children.push(PromptNode::State(PromptStateDoc::new("state", blocks)));
    }

    (!children.is_empty())
        .then(|| PromptNode::Group(PromptGroupDoc::new(surface.app_id.to_string(), children)))
}

fn render_bracketed_values(values: impl IntoIterator<Item = String>) -> String {
    format!(
        "[{}]",
        values
            .into_iter()
            .filter(|value| !value.trim().is_empty())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

impl AfterClaimContextPart for AfterClaimInputPart {
    fn key(&self) -> &'static str {
        "claimed_input"
    }

    fn build(&self, ctx: &Context, input: &AfterClaimContextInput) -> Option<PromptNode> {
        if input.is_empty() {
            return None;
        }

        let mut children = Vec::new();
        if !input.events.is_empty() {
            children.push(PromptNode::State(PromptStateDoc::new(
                "events",
                vec![PromptBlock::Paragraph(render_afterclaim_events(
                    &input.events,
                ))],
            )));
        }
        if !input.app_notices.is_empty() {
            children.push(PromptNode::State(PromptStateDoc::new(
                "app_notices",
                vec![PromptBlock::BulletList(
                    input
                        .app_notices
                        .iter()
                        .map(|(app, reason)| {
                            format!("app={app} reason={}", summarize_context_text(reason, 160))
                        })
                        .collect(),
                )],
            )));
        }
        let skill_injections = ctx
            .openskills
            .explicit_skill_injections_for_text(&claimed_work_query(input));
        if !skill_injections.is_empty() {
            children.push(PromptNode::State(PromptStateDoc::new(
                "explicit_skills",
                vec![PromptBlock::Paragraph(render_explicit_skill_injections(
                    &skill_injections,
                ))],
            )));
        }

        Some(PromptNode::Group(PromptGroupDoc::new(self.key(), children)))
    }
}

impl AfterClaimContextPart for AfterClaimWorkflowPrimitiveRoutingPart {
    fn key(&self) -> &'static str {
        "primitive_routing"
    }

    fn build(&self, ctx: &Context, input: &AfterClaimContextInput) -> Option<PromptNode> {
        let mut blocks = Vec::new();
        if ctx.bound_primitive_id.is_none() {
            // Routing contract already lives in the core system prompt.
        } else if let Some(workflow_id) = ctx.bound_primitive_id.as_deref() {
            blocks.push(PromptBlock::KeyValueList(vec![(
                "current_bound_primitive_id".to_string(),
                workflow_id.to_string(),
            )]));
        }

        let routing = ctx
            .workflows
            .primitive_routing_catalog(&claimed_work_query(input), 8);
        if routing.total_count > 0 {
            blocks.push(PromptBlock::KeyValueList(vec![(
                "primitive_ids".to_string(),
                render_workflow_primitive_ids(&routing.primitive_ids),
            )]));
        }
        if !routing.relevant_primitives.is_empty() {
            let shown_count = routing.relevant_primitives.len();
            blocks.push(PromptBlock::Paragraph("relevant_primitives:".to_string()));
            blocks.push(PromptBlock::BulletList(
                routing
                    .relevant_primitives
                    .iter()
                    .map(render_workflow_primitive_summary)
                    .collect(),
            ));
            if routing.relevant_omitted_count > 0 {
                blocks.push(PromptBlock::Paragraph(format!(
                    "Showing {shown_count} relevant primitive details from {} loaded primitives; {} additional relevant matches are not expanded. The primitive_ids line is the full loaded primitive vocabulary; do not browse it mechanically before continuing.",
                    routing.total_count, routing.relevant_omitted_count
                )));
            }
        } else if routing.total_count > 0 {
            blocks.push(PromptBlock::Paragraph(format!(
                "No relevant primitive details matched the claimed input among {} loaded primitives. Use primitive_ids as filename vocabulary for possible `activate_composed_primitive` input, but do not create a composite workflow merely to satisfy routing; continue with a plan unless a new stable primitive is genuinely needed.",
                routing.total_count
            )));
        }

        if blocks.is_empty() {
            None
        } else {
            Some(PromptNode::State(PromptStateDoc::new(self.key(), blocks)))
        }
    }
}

fn claimed_work_query(input: &AfterClaimContextInput) -> String {
    let mut parts = Vec::new();
    for event in &input.events {
        match &event.payload {
            EventPayload::TelegramIncoming(payload) => {
                parts.push(payload.incoming_text.as_str());
                for attachment in &payload.attachments {
                    if let Some(description) = attachment.description.as_deref() {
                        parts.push(description);
                    }
                }
            }
            EventPayload::TerminalIncoming(payload) => {
                parts.push(payload.incoming_text.as_str());
                for attachment in &payload.attachments {
                    if let Some(description) = attachment.description.as_deref() {
                        parts.push(description);
                    }
                }
            }
        }
    }
    for (_app, reason) in &input.app_notices {
        parts.push(reason.as_str());
    }
    parts.join("\n")
}

fn render_workflow_primitive_summary(summary: &crate::workflow::PrimitiveSummary) -> String {
    let prefix = match summary.origin {
        crate::workflow::PrimitiveOrigin::Builtin => "[builtin] ",
        crate::workflow::PrimitiveOrigin::Workspace => "[workspace] ",
    };
    let mut parts = vec![format!("{prefix}{}", summary.id)];
    if !summary.capability_summary.is_empty() {
        parts.push(format!("does={}", summary.capability_summary));
    }
    if !summary.inputs_summary.is_empty() {
        parts.push(format!("inputs={}", summary.inputs_summary));
    }
    if !summary.outputs_summary.is_empty() {
        parts.push(format!("outputs={}", summary.outputs_summary));
    }
    if !summary.when_to_use_summary.is_empty() {
        parts.push(format!("when={}", summary.when_to_use_summary));
    }
    parts.join(" | ")
}
fn render_workflow_primitive_ids(ids: &[crate::workflow::PrimitiveId]) -> String {
    ids.iter()
        .map(|primitive| {
            let prefix = match primitive.origin {
                crate::workflow::PrimitiveOrigin::Builtin => "[builtin] ",
                crate::workflow::PrimitiveOrigin::Workspace => "[workspace] ",
            };
            format!("{prefix}{}", primitive.id)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_afterclaim_events(events: &[EventView]) -> String {
    let mut lines = Vec::new();
    for (index, event) in events.iter().enumerate() {
        if index > 0 {
            lines.push(String::new());
        }
        match &event.payload {
            EventPayload::TelegramIncoming(payload) => {
                let attachment_summary = if payload.attachments.is_empty() {
                    String::new()
                } else {
                    format!(" attachments={}", payload.attachments.len())
                };
                lines.push(format!(
                    "- {}. [telegram / {} / arrived_at_ms={}] {} @ {} (chat_id={}){}: {}",
                    event.event_id,
                    event.status,
                    event.arrived_at_ms,
                    payload.sender,
                    payload.chat_title,
                    payload.chat_id,
                    attachment_summary,
                    compact_horizontal_whitespace(&payload.incoming_text)
                ));
            }
            EventPayload::TerminalIncoming(payload) => {
                let attachment_summary = if payload.attachments.is_empty() {
                    String::new()
                } else {
                    format!(" attachments={}", payload.attachments.len())
                };
                lines.push(format!(
                    "- {}. [terminal / {} / arrived_at_ms={}] {}{}: {}",
                    event.event_id,
                    event.status,
                    event.arrived_at_ms,
                    payload.origin,
                    attachment_summary,
                    compact_horizontal_whitespace(&payload.incoming_text)
                ));
            }
        }
        if let Some(error) = event.last_error.as_deref() {
            lines.push(format!(
                "  last_error={}",
                summarize_context_text(error, 160)
            ));
        }
    }
    lines.join("\n")
}

fn render_explicit_skill_injections(
    injections: &[crate::openskills::OpenSkillInjection],
) -> String {
    injections
        .iter()
        .map(|skill| {
            format!(
                "<skill>\n<name>{}</name>\n<path>{}</path>\n{}\n</skill>",
                skill.name,
                skill.path.display(),
                skill.contents.trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn summarize_context_text(text: &str, max_chars: usize) -> String {
    // Collapse runs of horizontal whitespace (spaces, tabs) into a single
    // space per run, but preserve newlines so that multi-line input
    // structure is not lost.  Only truncate when the result exceeds the
    // character budget.
    let compact = compact_horizontal_whitespace(text);
    let char_count = compact.chars().count();
    if char_count <= max_chars {
        return compact;
    }
    let head = compact.chars().take(max_chars).collect::<String>();
    format!("{head}...")
}

pub(crate) fn compact_horizontal_whitespace(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut in_space_run = false;
    for ch in text.chars() {
        if ch == ' ' || ch == '\t' {
            if !in_space_run {
                result.push(' ');
                in_space_run = true;
            }
        } else {
            in_space_run = false;
            result.push(ch);
        }
    }
    result
}
