use crate::{context::Context, preturn_state::PreTurnState};

use super::{
    prompt_doc::PromptDocument,
    prompt_parts::{
        AfterClaimContextInput, AfterClaimContextPart, AfterClaimInputPart,
        AfterClaimWorkflowPrimitiveRoutingPart, PreTurnAppSurfacePart, PreTurnContextPart,
        PreTurnPlanPart, PreTurnSensoryPart, PreTurnWorkflowStatePart,
    },
    prompts::{SYSTEM_CORE, build_app_how_to_use_prompt, build_app_usage_prompt},
    turn_compile::{
        PromptPersonaSpec, load_or_create_prompt_persona_spec_sync, load_prompt_persona_spec_sync,
        resolve_prompt_persona_language,
    },
};

const WORKSPACE_PATH_PLACEHOLDER: &str = "{{workspace_path}}";
const PERSONA_SECTION_PLACEHOLDER: &str = "{{persona_section}}";
const SKILLS_SECTION_PLACEHOLDER: &str = "{{skills_section}}";
const APP_DOCS_SECTION_PLACEHOLDER: &str = "{{app_docs_section}}";
const COMPILED_ADDITIONS_SECTION_PLACEHOLDER: &str = "{{compiled_additions_section}}";

pub struct PreTurnContextAssembler {
    parts: Vec<Box<dyn PreTurnContextPart>>,
}

pub struct AfterClaimContextAssembler {
    parts: Vec<Box<dyn AfterClaimContextPart>>,
}

struct RuntimeSystemPromptSections {
    workspace_path: String,
    persona_section: String,
    skills_section: String,
    app_docs_section: String,
    compiled_additions_section: String,
}

pub fn runtime_system_prompt_text(ctx: &Context) -> String {
    let configured_locale = ctx.config.locale.as_str();
    let persona = load_or_create_prompt_persona_spec_sync(configured_locale);
    render_runtime_system_prompt(RuntimeSystemPromptSections {
        workspace_path: format!(
            "Your absolute workspace path is `{}`.",
            ctx.execution_cwd.display()
        ),
        persona_section: render_persona_section(
            &persona,
            &resolve_prompt_persona_language(&persona, configured_locale),
            configured_locale,
        ),
        skills_section: ctx.openskills.render_prompt_block().unwrap_or_default(),
        app_docs_section: render_app_docs_section(ctx),
        compiled_additions_section: render_compiled_additions_section(
            ctx.compiled_prompts.runtime_system_additions(),
        ),
    })
}

pub fn runtime_system_prompt_text_from_additions(additions: &[String]) -> String {
    let persona = load_prompt_persona_spec_sync();
    render_runtime_system_prompt(RuntimeSystemPromptSections {
        workspace_path:
            "The absolute runtime workspace path is injected into the real system prompt."
                .to_string(),
        persona_section: render_persona_section(
            &persona,
            persona.language.trim(),
            "injected in live runtime prompt",
        ),
        skills_section: String::new(),
        app_docs_section: String::new(),
        compiled_additions_section: render_compiled_additions_section(additions),
    })
}

fn render_runtime_system_prompt(sections: RuntimeSystemPromptSections) -> String {
    let rendered = SYSTEM_CORE
        .replace(WORKSPACE_PATH_PLACEHOLDER, sections.workspace_path.trim())
        .replace(PERSONA_SECTION_PLACEHOLDER, sections.persona_section.trim())
        .replace(SKILLS_SECTION_PLACEHOLDER, sections.skills_section.trim())
        .replace(
            APP_DOCS_SECTION_PLACEHOLDER,
            sections.app_docs_section.trim(),
        )
        .replace(
            COMPILED_ADDITIONS_SECTION_PLACEHOLDER,
            sections.compiled_additions_section.trim(),
        );
    compact_blank_lines(&rendered)
}

fn render_persona_section(
    persona: &PromptPersonaSpec,
    language: &str,
    configured_locale: &str,
) -> String {
    format!(
        "# Persona\n\nname: {}\nlanguage: {}\nconfigured_locale: {}\n\n{}",
        persona.name.trim(),
        language.trim(),
        configured_locale.trim(),
        persona.identity_summary.trim()
    )
}

fn render_app_docs_section(ctx: &Context) -> String {
    let mut sections = Vec::new();
    let state_renders = ctx.apps.state_renders();
    for (app_id, _state) in &state_renders {
        if let Some(usage) = ctx.apps.usage(app_id) {
            sections.push(format!(
                "## {app_id} Usage\n\n{}",
                build_app_usage_prompt(app_id.clone(), &usage)
            ));
        }
        if let Some(how_to_use) = ctx.apps.how_to_use(app_id) {
            sections.push(format!(
                "## {app_id} Operation\n\n{}",
                build_app_how_to_use_prompt(app_id.clone(), &how_to_use)
            ));
        }
    }
    if sections.is_empty() {
        String::new()
    } else {
        format!("# App Documentation\n\n{}", sections.join("\n\n"))
    }
}

fn render_compiled_additions_section(additions: &[String]) -> String {
    let items = additions
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .map(|line| format!("- {line}"))
        .collect::<Vec<_>>();
    if items.is_empty() {
        String::new()
    } else {
        format!("# Runtime Prompt Additions\n\n{}", items.join("\n"))
    }
}

fn compact_blank_lines(input: &str) -> String {
    let mut lines = Vec::new();
    let mut blank_count = 0usize;
    for line in input.lines() {
        if line.trim().is_empty() {
            blank_count += 1;
            if blank_count <= 2 {
                lines.push(String::new());
            }
        } else {
            blank_count = 0;
            lines.push(line.trim_end().to_string());
        }
    }
    lines.join("\n").trim().to_string()
}

impl PreTurnContextAssembler {
    pub fn new(parts: Vec<Box<dyn PreTurnContextPart>>) -> Self {
        Self { parts }
    }

    pub fn default_runtime() -> Self {
        Self::new(vec![
            Box::new(PreTurnSensoryPart),
            Box::new(PreTurnPlanPart),
            Box::new(PreTurnWorkflowStatePart),
            Box::new(PreTurnAppSurfacePart),
        ])
    }

    pub fn assemble(&self, ctx: &Context, state: &PreTurnState) -> PromptDocument {
        PromptDocument::new(
            self.parts
                .iter()
                .filter_map(|part| part.build(ctx, state))
                .collect(),
        )
    }
}

impl AfterClaimContextAssembler {
    pub fn new(parts: Vec<Box<dyn AfterClaimContextPart>>) -> Self {
        Self { parts }
    }

    pub fn default_runtime() -> Self {
        Self::new(vec![
            Box::new(AfterClaimInputPart),
            Box::new(AfterClaimWorkflowPrimitiveRoutingPart),
        ])
    }

    pub fn assemble(&self, ctx: &Context, input: &AfterClaimContextInput) -> PromptDocument {
        PromptDocument::new(
            self.parts
                .iter()
                .filter_map(|part| part.build(ctx, input))
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_system_prompt_from_additions_is_single_markdown_document() {
        let text = runtime_system_prompt_text_from_additions(&["extra rule".to_string()]);

        assert!(text.starts_with("# Runtime Identity"));
        assert!(text.contains("# Event Handling"));
        assert!(text.contains("# Planning"));
        assert!(text.contains("# Primitive Workflows"));
        assert!(text.contains("# Runtime Prompt Additions\n\n- extra rule"));
        assert!(!text.contains("<core>"));
        assert!(!text.contains("<event>"));
        assert!(!text.contains("{{"));
    }
}
