//! Turn-compile evaluation pipeline infrastructure.
//! Many items in this module exist for offline evaluation and training runs
//! that are not linked into the main binary path.

use std::{
    io::Write,
    path::{Path, PathBuf},
};

use miette::{Result, miette};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::{
    daat_locus_paths::daat_locus_paths_sync,
    reasoning::{
        compiled::{
            CompiledPromptStore, CompiledRuntimeSystemPrompt, RUNTIME_SYSTEM_PROMPT_COMPILE_KEY,
        },
        prompts::PERSONA_DEFAULT,
    },
};

pub const PROMPT_PERSONA_FILE_NAME: &str = "persona.md";
const PROMPT_PERSONA_CONFIGURED_LOCALE_LANGUAGE: &str = "configured-locale";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PromptPersonaSpec {
    pub name: String,
    #[serde(default = "default_prompt_persona_language")]
    pub language: String,
    pub identity_summary: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct PromptPersonaFrontmatter {
    pub name: String,
    #[serde(default = "default_prompt_persona_language")]
    pub language: String,
}

fn default_prompt_persona_language() -> String {
    PROMPT_PERSONA_CONFIGURED_LOCALE_LANGUAGE.to_string()
}

#[cfg(test)]
pub struct TurnRolloutRunner;

#[cfg(test)]
struct TurnTraceSourceTurn {
    id: String,
    current_doing: String,
    description: String,
    observation: String,
    actions: Vec<EpisodeActionRecord>,
    history_messages: Vec<HistoryMessage>,
}

#[cfg(test)]
impl TurnRolloutRunner {
    fn trace_from_turns(span_id: &str, turns: &[TurnTraceSourceTurn]) -> TurnTraceArtifact {
        let steps = turns
            .iter()
            .map(turn_trace_step_from_source_turn)
            .collect::<Vec<_>>();
        let final_turn = turns
            .last()
            .expect("turn trace source should contain at least one turn");
        let final_assistant_message = final_turn
            .history_messages
            .iter()
            .rev()
            .find(|message| message.is_assistant())
            .and_then(|message| message.text_content().map(str::to_string))
            .filter(|message| !message.trim().is_empty());
        let final_reply_message = last_finish_and_send_reply_message(&final_turn.history_messages);
        TurnTraceArtifact {
            span_id: span_id.to_string(),
            turn_count: turns.len(),
            steps,
            final_assistant_message,
            final_reply_message,
        }
    }
}

pub fn prompt_persona_path_sync() -> PathBuf {
    daat_locus_paths_sync().config_file(PROMPT_PERSONA_FILE_NAME)
}

pub fn load_prompt_persona_spec_sync() -> PromptPersonaSpec {
    let path = prompt_persona_path_sync();
    load_prompt_persona_spec_from_path_sync(&path, None, false)
}

pub fn load_or_create_prompt_persona_spec_sync(locale: &str) -> PromptPersonaSpec {
    let path = prompt_persona_path_sync();
    load_prompt_persona_spec_from_path_sync(&path, Some(locale), true)
}

fn load_prompt_persona_spec_from_path_sync(
    path: &Path,
    locale_hint: Option<&str>,
    create_if_missing: bool,
) -> PromptPersonaSpec {
    if !path.exists() {
        let default = prompt_persona_spec_from_default_prompt(locale_hint);
        if create_if_missing {
            write_default_prompt_persona_file_sync(path, &default);
        }
        return default;
    }

    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) => {
            warn!(
                "failed to read prompt persona spec '{}': {error}",
                path.display()
            );
            return prompt_persona_spec_from_default_prompt(locale_hint);
        }
    };

    match parse_prompt_persona_markdown(&content) {
        Ok(parsed) => parsed,
        Err(error) => {
            warn!(
                "failed to parse prompt persona spec '{}': {error}",
                path.display()
            );
            prompt_persona_spec_from_default_prompt(locale_hint)
        }
    }
}

pub fn resolve_prompt_persona_language(
    persona: &PromptPersonaSpec,
    configured_locale: &str,
) -> String {
    let language = persona.language.trim();
    if language.is_empty() || language == PROMPT_PERSONA_CONFIGURED_LOCALE_LANGUAGE {
        configured_locale.trim().to_string()
    } else {
        language.to_string()
    }
}

fn write_default_prompt_persona_file_sync(path: &Path, spec: &PromptPersonaSpec) {
    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        warn!(
            "failed to create prompt persona config dir '{}': {error}",
            parent.display()
        );
        return;
    }

    let content = render_prompt_persona_markdown(spec);
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return,
        Err(error) => {
            warn!(
                "failed to create default prompt persona spec '{}': {error}",
                path.display()
            );
            return;
        }
    };

    if let Err(error) = file.write_all(content.as_bytes()) {
        warn!(
            "failed to write default prompt persona spec '{}': {error}",
            path.display()
        );
    }
}

fn parse_prompt_persona_markdown(content: &str) -> Result<PromptPersonaSpec> {
    let (frontmatter_text, body) = split_prompt_persona_frontmatter(content)?;
    let frontmatter: PromptPersonaFrontmatter = serde_yaml::from_str(frontmatter_text)
        .map_err(|error| miette!("parse persona frontmatter failed: {error}"))?;
    let identity_summary = body.trim().to_string();
    if frontmatter.name.trim().is_empty() {
        return Err(miette!(
            "persona frontmatter field 'name' must not be empty"
        ));
    }
    if identity_summary.is_empty() {
        return Err(miette!("persona markdown body must not be empty"));
    }
    Ok(PromptPersonaSpec {
        name: frontmatter.name.trim().to_string(),
        language: normalized_persona_language(&frontmatter.language),
        identity_summary,
    })
}

fn normalized_persona_language(language: &str) -> String {
    let language = language.trim();
    if language.is_empty() {
        default_prompt_persona_language()
    } else {
        language.to_string()
    }
}

fn split_prompt_persona_frontmatter(content: &str) -> Result<(&str, &str)> {
    let rest = content
        .strip_prefix("---\r\n")
        .or_else(|| {
            content
                .strip_prefix("---\n")
                .or_else(|| content.strip_prefix("---"))
        })
        .ok_or_else(|| miette!("persona file missing frontmatter start"))?;
    let delimiter = rest
        .find("\n---\n")
        .map(|index| (index, 5))
        .or_else(|| rest.find("\r\n---\r\n").map(|index| (index, 7)))
        .or_else(|| rest.find("\n---\r\n").map(|index| (index, 6)))
        .or_else(|| rest.find("\r\n---\n").map(|index| (index, 6)))
        .ok_or_else(|| miette!("persona file missing frontmatter end"))?;
    Ok((&rest[..delimiter.0], &rest[delimiter.0 + delimiter.1..]))
}

pub fn render_prompt_persona_markdown(spec: &PromptPersonaSpec) -> String {
    let frontmatter = PromptPersonaFrontmatter {
        name: spec.name.clone(),
        language: spec.language.clone(),
    };
    let frontmatter_text = serde_yaml::to_string(&frontmatter)
        .unwrap_or_else(|_| format!("name: {}\nlanguage: {}\n", spec.name, spec.language));
    format!(
        "---\n{}---\n\n{}\n",
        frontmatter_text,
        spec.identity_summary.trim()
    )
}

#[cfg(test)]
fn turn_trace_step_from_source_turn(turn: &TurnTraceSourceTurn) -> TurnTraceStep {
    TurnTraceStep {
        turn_id: turn.id.clone(),
        current_doing: turn.current_doing.clone(),
        description: turn.description.clone(),
        observation: turn.observation.clone(),
        actions: turn.actions.clone(),
        assistant_message: last_assistant_message(turn),
        reply_message: last_finish_and_send_reply_message(&turn.history_messages),
    }
}

pub fn current_runtime_system_prompt_artifact_from_store(
    compiled_prompts: &CompiledPromptStore,
) -> CompiledRuntimeSystemPrompt {
    CompiledRuntimeSystemPrompt {
        compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
        best_candidate: "runtime_baseline".to_string(),
        system_additions: compiled_prompts.runtime_system_additions().to_vec(),
        selected_demo_titles: Vec::new(),
        report: None,
    }
}

impl Default for PromptPersonaSpec {
    fn default() -> Self {
        prompt_persona_spec_from_default_prompt(None)
    }
}

fn prompt_persona_spec_from_default_prompt(locale_hint: Option<&str>) -> PromptPersonaSpec {
    let language = match PERSONA_DEFAULT.language.trim() {
        "" => default_prompt_persona_language(),
        PROMPT_PERSONA_CONFIGURED_LOCALE_LANGUAGE => locale_hint
            .map(str::trim)
            .filter(|locale| !locale.is_empty())
            .unwrap_or(PROMPT_PERSONA_CONFIGURED_LOCALE_LANGUAGE)
            .to_string(),
        language => language.to_string(),
    };
    PromptPersonaSpec {
        name: PERSONA_DEFAULT.name.trim().to_string(),
        language: normalized_persona_language(&language),
        identity_summary: PERSONA_DEFAULT.identity_summary.trim().to_string(),
    }
}

#[cfg(test)]
fn last_assistant_message(turn: &TurnTraceSourceTurn) -> Option<String> {
    turn.history_messages
        .iter()
        .rev()
        .find(|message| message.is_assistant())
        .and_then(|message| {
            message
                .text_content()
                .map(|content| content.trim().to_string())
        })
        .filter(|message| !message.is_empty())
}

#[cfg(test)]
mod tests {
    use crate::reasoning::runtime::HistoryMessage;

    use super::*;

    #[test]
    fn parse_prompt_persona_markdown_uses_frontmatter_and_body() {
        let parsed = parse_prompt_persona_markdown(
            r#"---
name: Test Persona
language: en-US
---

Be concise.
Preserve intent.
"#,
        )
        .expect("persona markdown should parse");

        assert_eq!(parsed.name, "Test Persona");
        assert_eq!(parsed.language, "en-US");
        assert_eq!(parsed.identity_summary, "Be concise.\nPreserve intent.");
    }

    #[test]
    fn parse_prompt_persona_markdown_defaults_language() {
        let parsed = parse_prompt_persona_markdown(
            r#"---
name: Test Persona
---

Use the configured locale by default.
"#,
        )
        .expect("persona markdown should parse");

        assert_eq!(parsed.language, "configured-locale");
        assert_eq!(
            parsed.identity_summary,
            "Use the configured locale by default."
        );
    }

    #[test]
    fn parse_prompt_persona_markdown_accepts_crlf_frontmatter() {
        let parsed = parse_prompt_persona_markdown(
            "---\r\nname: Test Persona\r\nlanguage: zh-CN\r\n---\r\n\r\nUse Chinese.\r\n",
        )
        .expect("persona markdown should parse");

        assert_eq!(parsed.name, "Test Persona");
        assert_eq!(parsed.language, "zh-CN");
        assert_eq!(parsed.identity_summary, "Use Chinese.");
    }

    #[test]
    fn default_prompt_persona_spec_uses_generated_default() {
        let parsed =
            parse_prompt_persona_markdown(crate::reasoning::prompts::PERSONA_DEFAULT_SOURCE)
                .expect("generated persona default should parse");
        assert_eq!(PromptPersonaSpec::default(), parsed);
    }

    #[test]
    fn default_prompt_persona_file_is_created_without_overwriting_existing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config").join(PROMPT_PERSONA_FILE_NAME);
        let initial = PromptPersonaSpec {
            name: "Initial Persona".to_string(),
            language: "en-US".to_string(),
            identity_summary: "Initial body.".to_string(),
        };
        write_default_prompt_persona_file_sync(&path, &initial);
        let initial_content = std::fs::read_to_string(&path).expect("initial persona file");
        let parsed_initial = parse_prompt_persona_markdown(&initial_content)
            .expect("written initial persona should parse");
        assert_eq!(parsed_initial, initial);

        let replacement = PromptPersonaSpec {
            name: "Replacement Persona".to_string(),
            language: "zh-CN".to_string(),
            identity_summary: "Replacement body.".to_string(),
        };
        write_default_prompt_persona_file_sync(&path, &replacement);
        let final_content = std::fs::read_to_string(&path).expect("final persona file");
        assert_eq!(final_content, initial_content);
    }

    #[test]
    fn missing_prompt_persona_file_is_created_with_configured_locale_hint() {
        for locale in ["zh-CN", "en-US"] {
            let temp = tempfile::tempdir().expect("tempdir");
            let path = temp.path().join("config").join(PROMPT_PERSONA_FILE_NAME);

            let loaded = load_prompt_persona_spec_from_path_sync(&path, Some(locale), true);
            assert_eq!(loaded.language, locale);

            let content = std::fs::read_to_string(&path).expect("written persona file");
            assert!(content.contains("{{name}}"));
            let written =
                parse_prompt_persona_markdown(&content).expect("written persona should parse");
            assert_eq!(written.language, locale);
        }
    }

    #[test]
    fn readonly_prompt_persona_load_does_not_create_missing_file() {
        for locale in ["zh-CN", "en-US"] {
            let temp = tempfile::tempdir().expect("tempdir");
            let path = temp.path().join("config").join(PROMPT_PERSONA_FILE_NAME);

            let loaded = load_prompt_persona_spec_from_path_sync(&path, Some(locale), false);
            assert_eq!(loaded.language, locale);
            assert!(!path.exists());
        }
    }

    #[test]
    fn prompt_persona_language_placeholder_resolves_to_configured_locale() {
        let persona = PromptPersonaSpec {
            name: "Test Persona".to_string(),
            language: "configured-locale".to_string(),
            identity_summary: "Body.".to_string(),
        };

        assert_eq!(resolve_prompt_persona_language(&persona, "zh-CN"), "zh-CN");
    }

    #[test]
    fn render_turn_trace_for_judge_includes_actions_and_assistant() {
        let turns = vec![TurnTraceSourceTurn {
            id: "turn-1".to_string(),
            current_doing: "analyze main".to_string(),
            description: "read main.rs".to_string(),
            observation: "needs more inspection".to_string(),
            actions: vec![crate::reasoning::episode::EpisodeActionRecord {
                kind: "assistant_message".to_string(),
                summary: "planning".to_string(),
            }],
            history_messages: vec![HistoryMessage {
                message: crate::reasoning::runtime::AgentMessage::assistant("I will continue."),
                activity_event: None,
                tool_call_activity_events: Vec::new(),
            }],
        }];

        let trace = TurnRolloutRunner::trace_from_turns("span-1", &turns);
        let rendered = render_turn_trace_for_judge(&trace);

        assert!(rendered.contains("turn[1].actions=assistant_message(planning)"));
        assert!(rendered.contains("turn[1].assistant_message=I will continue."));
    }

    #[test]
    fn unique_synthetic_telegram_id_is_positive_and_nonzero() {
        let id = unique_synthetic_telegram_id();
        assert!(id > 0);
    }
}
