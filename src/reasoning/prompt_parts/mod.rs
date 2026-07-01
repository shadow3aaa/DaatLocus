use crate::{
    app::AppId,
    context::Context,
    events::{EventPayload, EventView},
    preturn_state::PreTurnState,
};

use super::prompt_doc::{PromptBlock, PromptGroupDoc, PromptNode, PromptStateDoc};

pub trait PreTurnContextPart: Send + Sync {
    fn key(&self) -> &'static str;
    fn build(&self, ctx: &mut Context, state: &PreTurnState) -> Option<PromptNode>;
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
pub struct PreTurnProjectInstructionsPart;
pub struct PreTurnPlanPart;
pub struct AfterClaimInputPart;

impl PreTurnContextPart for PreTurnSensoryPart {
    fn key(&self) -> &'static str {
        "sensory"
    }

    fn build(&self, _ctx: &mut Context, state: &PreTurnState) -> Option<PromptNode> {
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

impl PreTurnContextPart for PreTurnProjectInstructionsPart {
    fn key(&self) -> &'static str {
        "project_instructions"
    }

    fn build(&self, ctx: &mut Context, _state: &PreTurnState) -> Option<PromptNode> {
        let project_dir = ctx.coding_project_dir.as_deref()?;
        let cached = ctx.apps.cached_root_project_instructions();
        let loaded;
        let instructions: &[crate::coding_app::ProjectInstructionDocument] = if !cached.is_empty() {
            cached
        } else {
            loaded = match crate::coding_app::load_instruction_documents_in_dir(project_dir) {
                Ok(instructions) if instructions.is_empty() => return None,
                Ok(instructions) => instructions,
                Err(err) => {
                    tracing::warn!(
                        project_dir = %project_dir.display(),
                        "failed to load project instruction context: {err:?}"
                    );
                    return Some(PromptNode::State(PromptStateDoc::new(
                        self.key(),
                        vec![PromptBlock::Paragraph(format!(
                            "project_instruction_error={err}"
                        ))],
                    )));
                }
            };
            &loaded
        };
        let fingerprint = crate::coding_app::project_instruction_fingerprint(instructions);
        let previous = ctx.delivered_root_instruction_fingerprint.take();
        if previous.as_deref() == Some(fingerprint.as_str()) {
            ctx.delivered_root_instruction_fingerprint = previous;
            return None;
        }
        let supersedes = previous.is_some();
        ctx.delivered_root_instruction_fingerprint = Some(fingerprint);
        let mut text = String::new();
        if supersedes {
            text.push_str("The following project instructions supersede the previously delivered version; treat the earlier version as fully replaced.\n\n");
        }
        text.push_str(&crate::coding_app::render_project_instructions(
            self.key(),
            instructions,
        ));
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

    fn build(&self, _ctx: &mut Context, state: &PreTurnState) -> Option<PromptNode> {
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
