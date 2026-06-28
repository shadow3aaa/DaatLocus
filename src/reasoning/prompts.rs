use crate::app::{AppDocs, AppId};

use super::prompt_text::{PromptTextBuilder, render_bullet_list};

mod generated {
    include!(concat!(env!("OUT_DIR"), "/prompt_bindings.rs"));
}

pub(crate) use generated::*;

pub(crate) fn prompt_bullet_lines(markdown: &str) -> Vec<String> {
    markdown
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| line.strip_prefix("- ").unwrap_or(line).to_string())
        .collect()
}

#[derive(Clone, Copy)]
pub(crate) struct AppPrompt {
    pub docs: &'static str,
}

#[derive(Clone, Copy)]
pub(crate) struct PromptPersona {
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

pub fn build_app_docs_prompt(app_id: AppId, docs: &AppDocs) -> String {
    if let Some(body) = docs.body_markdown.as_deref()
        && !body.trim().is_empty()
    {
        return body.trim().to_string();
    }
    let mut builder = PromptTextBuilder::new();
    let _ = app_id;
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
    }

    #[test]
    fn generated_persona_prompt_struct_is_nonempty() {
        assert!(!PERSONA_DEFAULT.name.is_empty());
        assert!(!PERSONA_DEFAULT.language.is_empty());
        assert!(!PERSONA_DEFAULT.identity_summary.is_empty());
    }
}
