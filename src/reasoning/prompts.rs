use crate::app::AppDocs;

use super::prompt_text::{PromptTextBuilder, render_bullet_list};

pub mod generated {
    include!(concat!(env!("OUT_DIR"), "/prompt_bindings.rs"));
}

pub use generated::PERSONA_DEFAULT;
pub use generated::{
    APP_BROWSER, APP_CODING, APP_TERMINAL, HISTORY_COMPACTION_PROMPT,
    HISTORY_COMPACTION_SUMMARY_PREFIX, HISTORY_COMPACTION_TOOL_DESCRIPTION,
    HISTORY_COMPACTION_USER_MESSAGE, MID_TURN_SUMMARY_PREFIX,
    PROGRAM_RUNTIME_ERROR_CORRECTION_PLANNER_INSTRUCTIONS,
    PROGRAM_RUNTIME_ERROR_CORRECTION_PLANNER_SYSTEM,
    PROGRAM_SKILL_IMPROVEMENT_PLANNER_INSTRUCTIONS, PROGRAM_SKILL_IMPROVEMENT_PLANNER_SYSTEM,
    RUNTIME_HISTORY_SUMMARY_PREFIX, SESSION_TITLE_SYSTEM_REQUIREMENTS, SESSION_TITLE_SYSTEM_ROLE,
    SESSION_TITLE_TOOL_DESCRIPTION, SESSION_TITLE_USER_MESSAGE_PREFIX, SYSTEM_CORE,
};
#[cfg(test)]
pub use generated::{
    APP_BROWSER_SOURCE, APP_CODING_SOURCE, APP_TERMINAL_SOURCE, PERSONA_DEFAULT_SOURCE,
};

pub fn prompt_bullet_lines(markdown: &str) -> Vec<String> {
    markdown
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| line.strip_prefix("- ").unwrap_or(line).to_string())
        .collect()
}

#[derive(Clone, Copy)]
pub struct AppPrompt {
    pub docs: &'static str,
}

#[derive(Clone, Copy)]
pub struct PromptPersona {
    pub name: &'static str,
    pub language: &'static str,
    pub identity_summary: &'static str,
}

impl AppPrompt {
    pub(crate) fn app_docs(self) -> AppDocs {
        AppDocs {
            lines: Vec::new(),
            body_markdown: Some(self.docs.to_string()),
        }
    }
}

pub fn build_app_docs_prompt(docs: &AppDocs) -> String {
    if let Some(body) = docs.body_markdown.as_deref()
        && !body.trim().is_empty()
    {
        return body.trim().to_string();
    }
    let mut builder = PromptTextBuilder::new();
    builder.push_paragraph(render_bullet_list(docs.lines.clone()));
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_prompt_sources_include_markdown_assets() {
        assert!(
            generated::PROMPT_SOURCES
                .iter()
                .any(|(id, source)| *id == "system/core" && *source == SYSTEM_CORE)
        );
        assert!(
            generated::PROMPT_SOURCES
                .iter()
                .any(|(id, source)| *id == "persona/default" && *source == PERSONA_DEFAULT_SOURCE)
        );
    }

    #[test]
    fn prompt_bullet_lines_strips_markdown_bullets() {
        assert_eq!(
            prompt_bullet_lines("- first\n- second\n\nthird"),
            vec![
                "first".to_string(),
                "second".to_string(),
                "third".to_string()
            ]
        );
    }

    #[test]
    fn generated_app_prompt_structs_are_nonempty() {
        for prompt in [APP_BROWSER, APP_CODING, APP_TERMINAL] {
            assert!(!prompt.docs.is_empty());
        }
        for source in [APP_BROWSER_SOURCE, APP_CODING_SOURCE, APP_TERMINAL_SOURCE] {
            assert!(!source.is_empty());
        }
    }

    #[test]
    fn generated_persona_prompt_struct_is_nonempty() {
        assert!(!PERSONA_DEFAULT.name.is_empty());
        assert!(!PERSONA_DEFAULT.language.is_empty());
        assert!(!PERSONA_DEFAULT.identity_summary.is_empty());
    }
}
