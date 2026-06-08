use crate::{
    app::{AppHowToUse, AppId, AppStateRender, AppUsage},
    context::Context,
};

use super::prompt_text::{PromptTextBuilder, render_bullet_list};

pub const EVENT_UNIT_WHAT: &str = r#"External inputs primarily enter the current turn through events. In an event-driven turn, plain assistant text is not automatically sent to the external user.
`<afterclaim_context> ... </afterclaim_context>` and `<preturn_context> ... </preturn_context>` are structured runtime context messages, not ordinary user chat. Claimed events or app notices inside them are pending world inputs that require explicit tool handling."#;

pub const EVENT_UNIT_HOW: &str = r#"The world only changes when you explicitly call tools. Any event completion that must deliver a final answer to the user, whether `resolved` or `failed`, must call `finish_and_send` with a `reply_message`.
Any claimed app notice that has been handled must be explicitly completed with `notice_resolved`; assistant text alone does not resolve an app notice.
If more work is still needed, do not call `finish_and_send`; continue using tools.
When an intermediate step is clearly complete, you may output text to explain and record progress. That intermediate note is not final delivery and must not be sent through `finish_and_send`.
If there is still an actionable goal, event, or app signal, plain text alone does not change the world and is not valid progress; call a tool instead.
For event-driven turns:
- Call `finish_and_send` only when the final reply is ready.
- Use `dismissed` only for explicit silent completion when no user reply is needed.
- If work still needs to continue, keep calling tools.
- Do not treat assistant text itself as a send action; final delivery must happen through the tool.
For user-facing replies, use the configured locale by default unless the user's message strongly indicates another language.
Read the current structured context carefully, analyze the situation, act first, and then provide the conclusion."#;

pub const APPS_UNIT_WHAT: &str = "An App is an encapsulated capability surface. Each App provides a distinct functional surface.";

pub const APPS_UNIT_WHEN: &str = "Focus an app when a task depends on it or when using it would solve the task better. If an app emits a signal, consider focusing it based on the current task and the signal's importance.";

pub const APPS_UNIT_HOW: &str = "Use `focus_app` to switch to the target app.";

pub const WORKSPACE_UNIT_WHEN: &str = "When you need to perform file operations that belong to you, default to this workspace directory.";

pub const WORKSPACE_UNIT_WHY: &str =
    "A fixed workspace gives you a stable owned area for tasks that require file operations.";

pub const WORKSPACE_UNIT_HOW: &str = "When using relative paths, do not include the workspace directory name again. The workspace unit already gives the absolute workspace path; relative paths are relative to that directory.";

pub const PLAN_UNIT_WHAT: &str = "A plan is the latest step-by-step plan for the current task. It records the sequence of steps needed to finish the task and the current progress of each step.";

pub const PLAN_UNIT_WHEN: &str = "Maintain a plan when the task is non-trivial, multi-step, or requires ongoing progress tracking, so current progress, the next step, and remaining work stay clear.";

pub const PLAN_UNIT_HOW: &str = "Use `update_plan` to maintain the plan. Each call must submit the complete current plan, not a patch for one step. Plan steps should be short, preferably 5 to 7 words, and must be concrete, actionable, and verifiable. While work remains, exactly one step must be `in_progress`; completed steps use `completed`, later steps use `pending`. When all steps are complete, clear the plan instead of retaining completed steps.";

pub const WORKFLOW_UNIT_WHAT: &str = "A primitive is the runtime binding and evolution layer for an evolvable SOP primitive library. Each persisted primitive spec describes one reusable primitive procedure with applicability metadata, preconditions, reusable steps, done criteria, and stable recovery paths.";

pub const WORKFLOW_UNIT_WHEN: &str = "When `<primitive>` shows `bound_primitive_id=<none>`, inspect `<primitive_routing>` in `<afterclaim_context>` as a reusable SOP primitive routing catalog. `primitive_ids` is the full loaded primitive vocabulary for composition awareness; primitive ids are legal only when they are primitive filenames made from lowercase `a-z` and `-`. `relevant_primitives` expands only the top task-relevant primitives with capability, input, output, and when metadata. To bind one primitive, call `activate_composed_primitive` with that primitive id before executing it. To bind a temporary graph of multiple existing primitives, call `activate_composed_primitive` with the ordered primitive ids joined by `-`; exact primitive filename matches win before composition segmentation. If none fits, continue with a normal plan; call `create_primitive_spec` only when the task truly needs a new stable primitive, not to persist a one-off composite task graph. If the user asks to modify the primitive/spec for a past, existing, or previously discussed task class, treat it as SOP primitive maintenance even when the wording is an ordinary instruction and still says workflow. If a primitive or composition is already bound, do not call `activate_composed_primitive` again just to reaffirm it; continue executing under the current binding.";

pub const WORKFLOW_UNIT_HOW: &str = "A primitive binding is runtime state for the current task and does not rewrite the primitive spec. Persisted specs are primitives; task-time compositions are ephemeral runtime plans or graphs with artifact handoff and must not be written back as new primitive specs by default. A composition is legal only when the input to `activate_composed_primitive` is exactly existing primitive filenames joined by `-`; the activation result returns `bound_primitive_id` plus ordered `primitive_ids` and binds that temporary composition without persisting a new primitive. You do not need to manually log daytime workflow outcomes; the runtime writes `PrimitiveRunRecord` directly at work-completion boundaries for sleep-time patch or merge. When the user asks or contextually implies that an existing reusable process should change, bind the primitive-spec-editing meta primitive, then use `read_primitive_spec` and `update_primitive_spec`; do not execute or activate the primitive being edited as the current task primitive.";

pub const HISTORY_COMPACTION_PROMPT: &str = r#"You are performing a context checkpoint compaction task.
Generate a handoff summary for another model that will continue this same thread.

Include:
1. Current progress and key decisions already made
2. Important context, constraints, user preferences, or system boundaries
3. Remaining work and a concrete next step
4. Key data, examples, paths, or identifiers needed to continue

Requirements:
- Be concise
- Be structured
- Keep only information truly necessary for continuation
- Focus on seamless handoff, not restating the entire transcript"#;

pub const HISTORY_COMPACTION_SUMMARY_PREFIX: &str = r#"Earlier runtime context was compacted into the following handoff summary.
Use it to continue the same thread without redoing already-finished work:"#;

pub fn build_workspace_unit_what(context: &Context) -> String {
    format!(
        "Your absolute workspace path is `{}`.",
        context.execution_cwd.display()
    )
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
    builder.push_labeled_section("what", usage.description.clone());
    if !usage.when_to_focus.is_empty() {
        builder.push_labeled_section("when", render_bullet_list(usage.when_to_focus.clone()));
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
