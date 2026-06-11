use crate::{
    app::{AppHowToUse, AppId, AppStateRender, AppUsage},
    context::Context,
};

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
    pub when_to_focus: &'static [&'static str],
    pub how_to_use: &'static str,
}

impl AppPrompt {
    pub(crate) fn usage(self) -> AppUsage {
        AppUsage {
            description: self.description.to_string(),
            when_to_focus: self
                .when_to_focus
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

const WORKSPACE_PATH_PLACEHOLDER: &str = "{{workspace_path}}";

pub fn build_workspace_unit_prompt(context: &Context) -> String {
    workspace_unit_prompt_with_path_line(&format!(
        "Your absolute workspace path is `{}`.",
        context.execution_cwd.display()
    ))
}

pub fn build_workspace_unit_placeholder_prompt() -> String {
    workspace_unit_prompt_with_path_line(
        "The absolute runtime workspace path is injected into the real system prompt.",
    )
}

fn workspace_unit_prompt_with_path_line(path_line: &str) -> String {
    SYSTEM_WORKSPACE.replace(WORKSPACE_PATH_PLACEHOLDER, path_line)
}

pub fn build_runtime_background_hint_items(context: &Context) -> Vec<String> {
    let focused = context.apps.focused();
    let composed_app_ids = context
        .apps
        .focused_composed_surfaces()
        .into_iter()
        .map(|surface| surface.app_id)
        .collect::<Vec<_>>();
    context
        .apps
        .state_renders()
        .into_iter()
        .filter(|(app_id, _)| focused.as_ref() != Some(app_id))
        .filter(|(app_id, _)| !composed_app_ids.contains(app_id))
        .filter_map(|(app_id, state)| background_app_attention_hint(app_id, &state))
        .collect()
}

pub fn build_app_pre_focus_note_prompt(app_id: AppId, state: &AppStateRender) -> String {
    let mut builder = PromptTextBuilder::new();
    builder.push_paragraph(format!(
        "`{app_id}` is not currently focused. If you need to operate it, call `focus_app` first."
    ));
    if let Some(hint) = background_app_attention_hint(app_id, state) {
        builder.push_paragraph(hint);
    }
    builder.build()
}

fn background_app_attention_hint(app_id: AppId, state: &AppStateRender) -> Option<String> {
    if !app_requires_attention(app_id.clone(), state) {
        return None;
    }

    if app_id.is_terminal() {
        let summary = if !list_field(&state.lines, "unread_sessions").is_empty() {
            "The background terminal has unread output.".to_string()
        } else {
            "The background terminal needs attention.".to_string()
        };
        return Some(format!(
            "{} If you decide to handle the terminal, call `focus_app` with app=\"terminal\" first.",
            summary
        ));
    }

    None
}

fn list_field(lines: &[String], key: &str) -> Vec<String> {
    lines
        .iter()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
        .map(|value| {
            if value == "none" {
                Vec::new()
            } else {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .map(ToString::to_string)
                    .collect()
            }
        })
        .unwrap_or_default()
}

fn app_requires_attention(app_id: AppId, state: &AppStateRender) -> bool {
    if app_id.is_terminal() {
        !list_field(&state.lines, "unread_sessions").is_empty()
    } else {
        false
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
    if !usage.when_to_focus.is_empty() {
        builder.push_labeled_section(
            "focus_guidance",
            render_bullet_list(usage.when_to_focus.clone()),
        );
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
                .any(|(id, source)| *id == "system/event" && *source == SYSTEM_EVENT)
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
            assert!(!prompt.when_to_focus.is_empty());
            assert!(!prompt.how_to_use.is_empty());
        }
    }
}
