use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::DashboardState;
use crate::{openskills::OpenSkillDashboardSummary, telegram_acl::PendingAccessRequest};

pub(super) fn render_skills_list(state: &DashboardState) -> String {
    if state.skills.is_empty() {
        let mut lines = vec![
            "No OpenSkills loaded.".to_string(),
            "Scanned fixed roots: project .agents/skills, ~/.daat-locus/skills, ~/.agents/skills."
                .to_string(),
        ];
        if !state.skill_errors.is_empty() {
            lines.push(String::new());
            lines.push("Load errors:".to_string());
            lines.extend(state.skill_errors.iter().map(|error| {
                format!(
                    "  {} | {}",
                    error.path,
                    truncate_command_text(&error.message, 160)
                )
            }));
        }
        return lines.join("\n");
    }

    let auto_count = state
        .skills
        .iter()
        .filter(|skill| skill.auto_use_enabled)
        .count();
    let manual_count = state.skills.len().saturating_sub(auto_count);
    let mut lines = vec![
        format!(
            "OpenSkills loaded: {} (auto: {auto_count}, manual-only: {manual_count})",
            state.skills.len()
        ),
        "Use /skills in the dashboard to browse, inspect, or toggle skills.".to_string(),
        String::new(),
        "Skills:".to_string(),
    ];
    lines.extend(state.skills.iter().map(|skill| {
        format!(
            "  {:<22} {:<12} {} - {}",
            skill.name,
            skill_status_label(skill),
            skill.scope,
            truncate_command_text(&skill.description, 140)
        )
    }));
    if !state.skill_errors.is_empty() {
        lines.push(String::new());
        lines.push("Load errors:".to_string());
        lines.extend(state.skill_errors.iter().map(|error| {
            format!(
                "  {} | {}",
                error.path,
                truncate_command_text(&error.message, 160)
            )
        }));
    }
    lines.join("\n")
}

pub(super) fn render_skill_detail(state: &DashboardState, target: &str) -> String {
    let skill = match resolve_skill_target(state, target) {
        Ok(skill) => skill,
        Err(message) => return message,
    };
    [
        format!("Name: {}", skill.name),
        format!("Status: {}", skill_status_description(skill)),
        format!("Scope: {}", skill.scope),
        format!("Path: {}", skill.path),
        format!("Description: {}", skill.description),
    ]
    .join("\n")
}

pub(super) fn resolve_skill_target<'a>(
    state: &'a DashboardState,
    target: &str,
) -> Result<&'a OpenSkillDashboardSummary, String> {
    let target = target.trim();
    if target.is_empty() {
        return Err("skill name or path is required".to_string());
    }

    let path_matches = state
        .skills
        .iter()
        .filter(|skill| skill.path == target)
        .collect::<Vec<_>>();
    if path_matches.len() == 1 {
        return Ok(path_matches[0]);
    }

    let name_matches = state
        .skills
        .iter()
        .filter(|skill| skill.name == target)
        .collect::<Vec<_>>();
    match name_matches.len() {
        1 => Ok(name_matches[0]),
        0 => Err(format!("unknown skill: {target}")),
        _ => {
            let paths = name_matches
                .iter()
                .map(|skill| format!("  {}", skill.path))
                .collect::<Vec<_>>()
                .join("\n");
            Err(format!(
                "ambiguous skill name: {target}\nuse full path:\n{paths}"
            ))
        }
    }
}

fn skill_status_label(skill: &OpenSkillDashboardSummary) -> &'static str {
    if skill.auto_use_enabled {
        "auto"
    } else {
        "manual-only"
    }
}

pub(super) fn skill_status_description(skill: &OpenSkillDashboardSummary) -> String {
    if skill.auto_use_enabled {
        "auto-use enabled".to_string()
    } else if skill.user_disabled {
        "manual-only: disabled by /skills".to_string()
    } else if !skill.allow_implicit_invocation {
        "manual-only: policy disallows implicit invocation".to_string()
    } else {
        "manual-only".to_string()
    }
}

pub(super) fn truncate_command_text(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out = text
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    out.push_str("...");
    out
}

pub(super) fn truncate_display_width(text: &str, max_width: usize) -> String {
    let text = text.trim();
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width <= 3 {
        return ".".repeat(max_width);
    }

    let body_width = max_width.saturating_sub(3);
    let mut out = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width.saturating_add(ch_width) > body_width {
            break;
        }
        out.push(ch);
        width = width.saturating_add(ch_width);
    }
    out.push_str("...");
    out
}

pub(super) fn render_pending_access_requests(
    action: &str,
    requests: &[PendingAccessRequest],
) -> String {
    if requests.is_empty() {
        return "no pending requests".to_string();
    }

    let mut lines = vec![format!(
        "pending requests - send '/telegram {action} <chat_id>' to proceed:"
    )];
    lines.extend(requests.iter().map(|request| {
        format!(
            "  {} | {} | {} | {}",
            request.chat_id, request.title, request.sender, request.last_message_preview
        )
    }));
    lines.join("\n")
}

pub(super) fn render_available_app_statuses(state: &DashboardState) -> String {
    let apps = state
        .app_status_outputs
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    if apps.is_empty() {
        "available apps: none".to_string()
    } else {
        format!("available apps: {}", apps.join(", "))
    }
}

pub(super) fn render_app_status_text(state: &DashboardState, target: &str) -> String {
    let target = target.trim().to_ascii_lowercase();
    state
        .app_status_outputs
        .iter()
        .find(|(name, _)| name == &target)
        .map(|(_, output)| output.clone())
        .unwrap_or_else(|| {
            let apps = render_available_app_statuses(state);
            format!("unknown app: {target}\n{apps}")
        })
}

pub(super) fn fallback_output(output: &str) -> String {
    if output.trim().is_empty() {
        "no data".to_string()
    } else {
        output.to_string()
    }
}

pub(super) fn format_pending_request_choices(requests: &[PendingAccessRequest]) -> String {
    requests
        .iter()
        .take(4)
        .map(|request| format!("{} {}", request.chat_id, request.sender))
        .collect::<Vec<_>>()
        .join(" | ")
}
