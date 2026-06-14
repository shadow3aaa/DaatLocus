use crate::app::{AppHowToUse, AppId, AppUsage};

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
    pub description: &'static str,
    pub when_to_use: &'static [&'static str],
    pub how_to_use: &'static str,
}

#[derive(Clone, Copy)]
pub(crate) struct PromptPersona {
    pub name: &'static str,
    pub language: &'static str,
    pub identity_summary: &'static str,
}

impl AppPrompt {
    pub(crate) fn usage(self) -> AppUsage {
        AppUsage {
            description: self.description.to_string(),
            when_to_use: self
                .when_to_use
                .iter()
                .map(|item| (*item).to_string())
                .collect(),
            body_markdown: None,
        }
    }

    pub(crate) fn app_how_to_use(self) -> AppHowToUse {
        AppHowToUse {
            lines: Vec::new(),
            body_markdown: Some(self.how_to_use.to_string()),
        }
    }
}

pub fn build_app_usage_prompt(_app_id: AppId, usage: &AppUsage) -> String {
    if let Some(body) = usage.body_markdown.as_deref()
        && !body.trim().is_empty()
    {
        return body.trim().to_string();
    }
    let mut builder = PromptTextBuilder::new();
    builder.push_labeled_section("description", usage.description.clone());
    if !usage.when_to_use.is_empty() {
        builder.push_labeled_section("when_to_use", render_bullet_list(usage.when_to_use.clone()));
    }
    builder.build()
}

pub fn build_app_how_to_use_prompt(app_id: AppId, how_to_use: &AppHowToUse) -> String {
    if let Some(body) = how_to_use.body_markdown.as_deref()
        && !body.trim().is_empty()
    {
        return body.trim().to_string();
    }
    let mut builder = PromptTextBuilder::new();
    let _ = app_id;
    builder.push_paragraph(render_bullet_list(how_to_use.lines.clone()));
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
            assert!(!prompt.description.is_empty());
            assert!(!prompt.when_to_use.is_empty());
            assert!(!prompt.how_to_use.is_empty());
        }
    }

    #[test]
    fn generated_persona_prompt_struct_is_nonempty() {
        assert!(!PERSONA_DEFAULT.name.is_empty());
        assert!(!PERSONA_DEFAULT.language.is_empty());
        assert!(!PERSONA_DEFAULT.identity_summary.is_empty());
    }
}
