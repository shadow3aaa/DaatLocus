use std::{collections::HashMap, env, path::PathBuf};

use miette::{Result, miette};
use serde_json::json;

use crate::{
    context::Context,
    device::DeviceAction,
    core::{Effect, Output},
    memory::L3EntryDraft,
    reasoning::{
        episode::{EpisodeOutcome, EpisodeStatus, EpisodeStep},
        examples::ExampleField,
        runtime::{PromptRequest, PromptRole},
    },
};

use super::{
    program::Program,
    programs::sleep_artifact_builder::{SleepArtifactBuilderOutput, SleepArtifactBuilderProgram},
    programs::sleep_l3_promoter::{SleepL3PromoterOutput, SleepL3PromoterProgram},
    programs::sleep_success_l3_promoter::{
        SleepSuccessL3PromoterOutput, SleepSuccessL3PromoterProgram,
    },
    render::openai_tools::OpenAIToolRenderer,
    runtime::execute_program_with_ir_report,
    sleep_artifacts::{
        SleepArtifactBootstrapDemo, SleepArtifactFailurePattern,
        SleepArtifactInstructionHypothesis, SleepArtifactStressCase, SleepArtifactSuggestedFixKind,
        SleepArtifactsStore,
    },
    trace::{ProgramTraceRecord, TraceOrigin},
};

#[derive(Clone)]
pub struct SleepSummary {
    pub failure_patterns: Vec<SleepArtifactFailurePattern>,
    pub bootstrap_demos: usize,
    pub stress_cases: usize,
    pub instruction_hypotheses: usize,
    pub promoted_success_l3_entries: usize,
    pub promoted_failure_l3_entries: usize,
    pub promoted_l3_entries: usize,
}

pub async fn run_sleep(context: &mut Context) -> Result<SleepSummary> {
    let records = load_runtime_trace_records().await?;
    let mut failure_patterns = derive_failure_patterns(&records);
    let episode_outcomes = load_recent_learn_episode_outcomes().await?;
    failure_patterns.extend(derive_episode_failure_patterns(&episode_outcomes));
    let store = SleepArtifactsStore::open().await?;
    store.replace_failure_patterns(&failure_patterns).await?;
    let mut derived = derive_sleep_artifacts(context, &failure_patterns).await?;
    derived
        .bootstrap_demos
        .extend(derive_success_bootstrap_demos(&records));
    derived
        .bootstrap_demos
        .extend(derive_episode_bootstrap_demos(&episode_outcomes));
    derived
        .stress_cases
        .extend(derive_episode_stress_cases(&episode_outcomes));
    derived
        .instruction_hypotheses
        .extend(derive_episode_instruction_hypotheses(&episode_outcomes));
    store
        .replace_bootstrap_demos(&derived.bootstrap_demos)
        .await?;
    store.replace_stress_cases(&derived.stress_cases).await?;
    store
        .replace_instruction_hypotheses(&derived.instruction_hypotheses)
        .await?;
    let mut promoted = promote_failure_patterns_to_l3(context, &failure_patterns).await?;
    let promoted_failure_l3_entries = promoted.len();
    let mut success_promoted = promote_success_patterns_to_l3(context, &records).await?;
    success_promoted.extend(promote_episode_success_patterns_to_l3(context, &episode_outcomes).await?);
    let promoted_success_l3_entries = success_promoted.len();
    promoted.extend(success_promoted);
    if !promoted.is_empty() {
        context.memory.upsert_l3_entries(promoted.clone());
        context.memory.sync_l3_to_disk().await;
    }
    Ok(SleepSummary {
        failure_patterns,
        bootstrap_demos: derived.bootstrap_demos.len(),
        stress_cases: derived.stress_cases.len(),
        instruction_hypotheses: derived.instruction_hypotheses.len(),
        promoted_success_l3_entries,
        promoted_failure_l3_entries,
        promoted_l3_entries: promoted.len(),
    })
}

struct DerivedSleepArtifacts {
    bootstrap_demos: Vec<SleepArtifactBootstrapDemo>,
    stress_cases: Vec<SleepArtifactStressCase>,
    instruction_hypotheses: Vec<SleepArtifactInstructionHypothesis>,
}

async fn load_runtime_trace_records() -> Result<Vec<ProgramTraceRecord>> {
    let path = crate::get_spinova_home()
        .await
        .join("reasoning_traces.jsonl");
    let bytes = match tokio::fs::read_to_string(&path).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(miette!(
                "failed to read reasoning trace file {}: {err}",
                path.display()
            ));
        }
    };

    let mut records = Vec::new();
    for line in bytes.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<ProgramTraceRecord>(line) {
            if record.origin == TraceOrigin::Runtime {
                records.push(record);
            }
        }
    }

    Ok(records)
}

async fn latest_train_source_learn_session_root() -> Result<Option<PathBuf>> {
    let train_root = env::current_dir()
        .map_err(|err| miette!("failed to get current dir for learn outcomes: {err}"))?
        .join("tmp")
        .join("train_source_learn");
    if !train_root.exists() {
        return Ok(None);
    }

    let mut latest_session: Option<(std::time::SystemTime, PathBuf)> = None;
    let mut entries = tokio::fs::read_dir(&train_root)
        .await
        .map_err(|err| miette!("failed to read train_source_learn dir {}: {err}", train_root.display()))?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|err| miette!("failed to read train_source_learn entry: {err}"))?
    {
        let path = entry.path();
        let Ok(metadata) = entry.metadata().await else {
            continue;
        };
        if !metadata.is_dir() {
            continue;
        }
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if latest_session
            .as_ref()
            .is_none_or(|(latest_modified, _)| modified > *latest_modified)
        {
            latest_session = Some((modified, path));
        }
    }

    Ok(latest_session.map(|(_, path)| path))
}

async fn load_recent_learn_episode_outcomes() -> Result<Vec<EpisodeOutcome>> {
    let Some(session_root) = latest_train_source_learn_session_root().await? else {
        return Ok(Vec::new());
    };
    let episodes_root = session_root.join("episodes");
    if !episodes_root.exists() {
        return Ok(Vec::new());
    }

    let mut outcomes = Vec::new();
    let mut episode_dirs = tokio::fs::read_dir(&episodes_root)
        .await
        .map_err(|err| miette!("failed to read learn episodes dir {}: {err}", episodes_root.display()))?;
    while let Some(entry) = episode_dirs
        .next_entry()
        .await
        .map_err(|err| miette!("failed to read learn episode entry: {err}"))?
    {
        let outcome_path = entry.path().join("episode_outcome.json");
        let payload = match tokio::fs::read_to_string(&outcome_path).await {
            Ok(payload) => payload,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(miette!(
                    "failed to read episode outcome {}: {err}",
                    outcome_path.display()
                ));
            }
        };
        let outcome = serde_json::from_str::<EpisodeOutcome>(&payload).map_err(|err| {
            miette!(
                "failed to parse episode outcome {}: {err}",
                outcome_path.display()
            )
        })?;
        outcomes.push(outcome);
    }

    Ok(outcomes)
}

fn derive_failure_patterns(records: &[ProgramTraceRecord]) -> Vec<SleepArtifactFailurePattern> {
    let mut buckets: HashMap<(String, String), PatternAccumulator> = HashMap::new();

    for record in records {
        let Some(error) = record.deserialization_error.as_deref() else {
            continue;
        };

        let label = classify_failure(record, error);
        let description = describe_failure(record, error, &label);
        let suggested_fix_kind = suggested_fix_kind(&label);
        let trace_id = format!(
            "{}:{}:{}",
            record.program_name, record.timestamp_ms, record.attempt
        );

        let entry = buckets
            .entry((record.program_name.clone(), label.clone()))
            .or_insert_with(|| PatternAccumulator {
                suite: record.program_name.clone(),
                label,
                description,
                supporting_trace_ids: Vec::new(),
                frequency: 0,
                severity: 1,
                suggested_fix_kind,
            });

        entry.frequency += 1;
        if entry.supporting_trace_ids.len() < 8 {
            entry.supporting_trace_ids.push(trace_id);
        }
        entry.severity = entry.severity.max(derive_severity(error));
    }

    let mut patterns = buckets
        .into_values()
        .map(|bucket| SleepArtifactFailurePattern {
            suite: bucket.suite.clone(),
            pattern_id: format!("{}:{}", slugify(&bucket.suite), slugify(&bucket.label)),
            description: bucket.description,
            supporting_trace_ids: bucket.supporting_trace_ids,
            frequency: bucket.frequency,
            severity: bucket.severity,
            suggested_fix_kind: bucket.suggested_fix_kind,
        })
        .collect::<Vec<_>>();

    patterns.sort_by(|left, right| {
        right
            .frequency
            .cmp(&left.frequency)
            .then_with(|| right.severity.cmp(&left.severity))
            .then_with(|| left.pattern_id.cmp(&right.pattern_id))
    });

    patterns
}

fn derive_episode_failure_patterns(
    outcomes: &[EpisodeOutcome],
) -> Vec<SleepArtifactFailurePattern> {
    let mut patterns = Vec::new();

    for outcome in outcomes {
        if !matches!(
            outcome.status,
            EpisodeStatus::Failed | EpisodeStatus::Aborted | EpisodeStatus::MaxStepsExceeded
        ) {
            continue;
        }

        let suites = infer_episode_failure_suites(outcome);
        let signal = infer_episode_failure_signal(outcome);
        let severity = signal.severity.max(match outcome.status {
            EpisodeStatus::Aborted | EpisodeStatus::MaxStepsExceeded => 4,
            EpisodeStatus::Failed => 3,
            EpisodeStatus::InProgress | EpisodeStatus::Succeeded => 2,
        });
        let suggested_fix_kind = signal.suggested_fix_kind;

        for suite in suites {
            patterns.push(SleepArtifactFailurePattern {
                suite: suite.clone(),
                pattern_id: format!(
                    "episode:{}:{}:{}",
                    slugify(&suite),
                    slugify(signal.label),
                    slugify(&outcome.task.id)
                ),
                description: signal.description.clone(),
                supporting_trace_ids: vec![format!("episode:{}", outcome.task.id)],
                frequency: outcome.metric.repeated_effects.max(1),
                severity,
                suggested_fix_kind,
            });
        }
    }

    patterns
}

fn derive_episode_bootstrap_demos(
    outcomes: &[EpisodeOutcome],
) -> Vec<SleepArtifactBootstrapDemo> {
    let mut demos = Vec::new();
    let mut per_suite = HashMap::<String, usize>::new();

    for outcome in outcomes {
        if outcome.status != EpisodeStatus::Succeeded {
            continue;
        }
        let Some(step) = outcome
            .steps
            .iter()
            .rev()
            .find(|step| infer_episode_suite_from_step(step).is_some())
        else {
            continue;
        };
        let Some(suite) = infer_episode_suite_from_step(step) else {
            continue;
        };
        let count = per_suite.entry(suite.to_string()).or_insert(0);
        if *count >= 3 {
            continue;
        }
        *count += 1;
        demos.push(SleepArtifactBootstrapDemo {
            suite: suite.to_string(),
            title: format!("Learn outcome {} #{}", outcome.task.id, count),
            input_summary: render_episode_input_summary(outcome, step),
            inputs: episode_example_inputs(outcome, step),
            expected_output: serde_json::to_value(step_to_output(step)).unwrap_or_else(|_| json!({})),
            reference_case_names: Vec::new(),
            source_trace_ids: vec![format!("episode:{}", outcome.task.id)],
            confidence: 0.75,
        });
    }

    demos
}

fn derive_episode_stress_cases(outcomes: &[EpisodeOutcome]) -> Vec<SleepArtifactStressCase> {
    let mut cases = Vec::new();

    for outcome in outcomes {
        if !matches!(
            outcome.status,
            EpisodeStatus::Failed | EpisodeStatus::Aborted | EpisodeStatus::MaxStepsExceeded
        ) {
            continue;
        }
        let Some(step) = outcome.steps.last() else {
            continue;
        };
        let Some(suite) = infer_episode_suite_from_step(step) else {
            continue;
        };
        let constraints = episode_failure_constraints(outcome, step);
        if constraints.is_empty() {
            continue;
        }
        cases.push(SleepArtifactStressCase {
            suite: suite.to_string(),
            name: format!("learn-{}", slugify(&outcome.task.id)),
            input_ir: json!({
                "task_title": outcome.task.title,
                "instruction": outcome.task.instruction,
                "snapshot_text": step.snapshot_text,
                "last_effect": format!("{:?}", step.effect),
                "notes": outcome.metric.notes,
            }),
            expected_constraints: constraints,
            reference_case_names: Vec::new(),
            source_pattern_id: format!("episode:{}", outcome.task.id),
            repeat: outcome.metric.repeated_effects.max(2),
            weight: usize::from(matches!(outcome.status, EpisodeStatus::Aborted | EpisodeStatus::MaxStepsExceeded)) + 1,
        });
    }

    cases
}

fn derive_episode_instruction_hypotheses(
    outcomes: &[EpisodeOutcome],
) -> Vec<SleepArtifactInstructionHypothesis> {
    let mut items = Vec::new();

    for outcome in outcomes {
        if !matches!(
            outcome.status,
            EpisodeStatus::Failed | EpisodeStatus::Aborted | EpisodeStatus::MaxStepsExceeded
        ) {
            continue;
        }
        let Some(step) = outcome.steps.last() else {
            continue;
        };
        let Some(suite) = infer_episode_suite_from_step(step) else {
            continue;
        };
        let Some(text) = episode_instruction_text(outcome, step) else {
            continue;
        };
        items.push(SleepArtifactInstructionHypothesis {
            suite: suite.to_string(),
            text,
            justification: summarize_episode_failure(outcome),
            source_pattern_ids: vec![format!("episode:{}", outcome.task.id)],
        });
    }

    items
}

fn infer_episode_failure_suites(outcome: &EpisodeOutcome) -> Vec<String> {
    let mut suites = vec!["action_phase.execute_task".to_string()];

    let touched_terminal = outcome.steps.iter().any(|step| {
        matches!(
            &step.effect,
            Effect::DeviceAction {
                action: DeviceAction::TerminalInput { .. }
            }
        ) || matches!(step.effect, Effect::Wait | Effect::SilentWait)
    });
    if touched_terminal {
        suites.push("terminal_next_step".to_string());
    }

    suites
}

fn infer_episode_suite_from_step(step: &EpisodeStep) -> Option<&'static str> {
    match &step.effect {
        Effect::FocusDevice {
            device: crate::device::DeviceId::Telegram,
        } => Some("action_phase.attend_notifications"),
        Effect::TaskSelect { .. } => Some("action_phase.execute_task"),
        Effect::TaskAdd {
            project_id: Some(_),
            ..
        } => Some("action_phase.plan_from_project"),
        Effect::TaskAdd {
            project_id: None, ..
        } => Some("action_phase.explore_new_tasks"),
        Effect::DeviceAction {
            action: DeviceAction::TerminalInput { .. },
        }
        | Effect::Wait
        | Effect::SilentWait => Some("terminal_next_step"),
        Effect::FocusDevice {
            device: crate::device::DeviceId::Terminal,
        } => Some("action_phase.execute_task"),
        _ => None,
    }
}

fn render_episode_input_summary(outcome: &EpisodeOutcome, step: &EpisodeStep) -> String {
    format!(
        "task: {}\ninstruction: {}\nlast_effect: {:?}\nsnapshot:\n{}",
        outcome.task.title.trim(),
        outcome.task.instruction.trim(),
        step.effect,
        step.snapshot_text.trim()
    )
}

fn episode_example_inputs(outcome: &EpisodeOutcome, step: &EpisodeStep) -> Vec<ExampleField> {
    vec![
        ExampleField {
            name: "训练任务".to_string(),
            value: outcome.task.instruction.clone(),
        },
        ExampleField {
            name: "完整快照".to_string(),
            value: step.snapshot_text.clone(),
        },
    ]
}

fn step_to_output(step: &EpisodeStep) -> Output {
    Output {
        observation: step.observation_summary.clone(),
        description: step
            .metadata
            .get("description")
            .cloned()
            .unwrap_or_default(),
        current_doing: step
            .metadata
            .get("current_doing")
            .cloned()
            .unwrap_or_default(),
        effect: step.effect.clone(),
    }
}

fn episode_failure_constraints(outcome: &EpisodeOutcome, step: &EpisodeStep) -> Vec<String> {
    let signal = infer_episode_failure_signal(outcome);
    let mut constraints = Vec::new();
    if signal.label == "verify-explicit-import-or-path-error" {
        constraints.push("验证阶段若已拿到明确导入或路径错误，应先检查缺失对象、工作目录、PYTHONPATH 与测试入口，而不是重复重跑 pytest 或继续刷日志。".to_string());
    }
    if signal.label == "verify-command-not-triggered" {
        constraints.push("验证命令无输出或疑似未真正执行时，应先确认命令已触发、带换行并回到正确工作目录，再决定是否重跑。".to_string());
    }
    if signal.label == "verify-log-analysis-loop" {
        constraints.push("验证阶段若已获得明确错误对象，不应继续重复 tail/grep/cat 同一日志，而应围绕错误对象收敛。".to_string());
    }
    if outcome.metric.repeated_effects > 0 {
        constraints.push("不应重复最近已经执行过且未产生新信息的相同 effect".to_string());
    }
    if matches!(
        step.effect,
        Effect::DeviceAction {
            action: DeviceAction::TerminalInput { .. }
        }
    ) {
        constraints.push("终端输出不足以得出新结论时，应更换查看策略而不是重跑同一命令".to_string());
    }
    if matches!(outcome.status, EpisodeStatus::MaxStepsExceeded | EpisodeStatus::Aborted) {
        constraints.push("应在任务停滞前收敛到新的分析动作或明确失败结论".to_string());
    }
    constraints
}

fn episode_instruction_text(outcome: &EpisodeOutcome, step: &EpisodeStep) -> Option<String> {
    let signal = infer_episode_failure_signal(outcome);
    match signal.label {
        "verify-explicit-import-or-path-error" => {
            return Some("当验证阶段已经拿到明确的 ModuleNotFound、ImportError、file not found 或入口路径错误时，应停止重复重跑 pytest 或刷日志，先检查缺失模块/文件是否存在、当前工作目录、PYTHONPATH 与测试入口范围。".to_string());
        }
        "verify-command-not-triggered" => {
            return Some("当验证命令无输出或疑似未真正触发时，应先确认命令格式、换行触发、当前 shell prompt 与工作目录，再决定是否重跑，而不是盲目重复验证。".to_string());
        }
        "verify-log-analysis-loop" => {
            return Some("验证阶段若已获得明确测试错误或日志锚点，应围绕该错误对象收敛处理；不要在同一份日志上反复 tail/grep/cat。".to_string());
        }
        _ => {}
    }
    if outcome.metric.repeated_effects > 0 {
        return Some(
            "当最近一步与上一轮 effect 重复且没有新增信息时，不要再次执行同一动作；应切换到更窄的查看、定位或验证策略。"
                .to_string(),
        );
    }
    if matches!(
        step.effect,
        Effect::DeviceAction {
            action: DeviceAction::TerminalInput { .. }
        }
    ) && matches!(outcome.status, EpisodeStatus::Aborted | EpisodeStatus::MaxStepsExceeded)
    {
        return Some(
            "终端分析任务在多步后仍无结果时，应停止重复输入并改用更聚焦的命令、验证命令或结束判定。"
                .to_string(),
        );
    }
    None
}

fn summarize_episode_failure(outcome: &EpisodeOutcome) -> String {
    let signal = infer_episode_failure_signal(outcome);
    let mut parts = vec![
        format!("训练任务失败: {}", outcome.task.title.trim()),
        format!("status={:?}", outcome.status),
        format!("failure_kind={}", signal.label),
        format!("strategy={}", signal.description),
    ];
    if outcome.metric.repeated_effects > 0 {
        parts.push(format!("repeated_effects={}", outcome.metric.repeated_effects));
    }
    if outcome.metric.stagnation_events > 0 {
        parts.push(format!(
            "stagnation_events={}",
            outcome.metric.stagnation_events
        ));
    }
    if let Some(last_step) = outcome.steps.last() {
        parts.push(format!("last_effect={:?}", last_step.effect));
        if let Some(description) = last_step.metadata.get("description") {
            let description = description.trim();
            if !description.is_empty() {
                parts.push(format!("last_description={description}"));
            }
        }
    }
    if !outcome.metric.notes.is_empty() {
        parts.push(format!(
            "notes={}",
            outcome.metric.notes.join(" | ").trim()
        ));
    }
    parts.join("; ")
}

struct EpisodeFailureSignal {
    label: &'static str,
    description: String,
    suggested_fix_kind: SleepArtifactSuggestedFixKind,
    severity: u8,
}

fn infer_episode_failure_signal(outcome: &EpisodeOutcome) -> EpisodeFailureSignal {
    let text = format!(
        "{}\n{}\n{}",
        outcome.final_observation.summary,
        outcome.final_observation.snapshot_text,
        outcome.metric.notes.join("\n")
    );
    let text = text.to_lowercase();

    if text.contains("modulenotfounderror")
        || text.contains("importerror")
        || text.contains("no module named")
        || text.contains("conftest")
        || text.contains("file not found")
    {
        return EpisodeFailureSignal {
            label: "verify-explicit-import-or-path-error",
            description: "验证阶段已得到明确的导入或路径错误。下一步应围绕缺失模块/文件、工作目录、PYTHONPATH 与测试入口收敛，而不是继续重复重跑 pytest 或反复查看同一日志。".to_string(),
            suggested_fix_kind: SleepArtifactSuggestedFixKind::Instruction,
            severity: 4,
        };
    }

    if text.contains("没有任何测试输出")
        || text.contains("尚未被执行")
        || text.contains("未真正执行")
        || text.contains("命令末尾有换行符")
        || text.contains("输出被截断")
    {
        return EpisodeFailureSignal {
            label: "verify-command-not-triggered",
            description: "验证命令疑似未真正触发或未正确落到预期 shell/工作目录。应先确认命令已带换行并真正执行，再决定是否重跑验证。".to_string(),
            suggested_fix_kind: SleepArtifactSuggestedFixKind::Instruction,
            severity: 3,
        };
    }

    if outcome.metric.repeated_effects > 0
        && (text.contains("pytest.log") || text.contains("tail ") || text.contains("grep ") || text.contains("cat "))
    {
        return EpisodeFailureSignal {
            label: "verify-log-analysis-loop",
            description: "验证阶段已经进入日志分析循环。若已拿到明确错误对象，应停止在同一份日志上反复 tail/grep/cat，转为围绕错误对象收敛处理。".to_string(),
            suggested_fix_kind: SleepArtifactSuggestedFixKind::StressCase,
            severity: 3,
        };
    }

    EpisodeFailureSignal {
        label: "generic-episode-failure",
        description: "训练任务在当前策略下未能及时收敛到新的分析、修改或验证动作，应更早形成明确的失败收敛策略。".to_string(),
        suggested_fix_kind: if outcome.metric.repeated_effects > 0 || outcome.metric.stagnation_events > 0 {
            SleepArtifactSuggestedFixKind::StressCase
        } else {
            SleepArtifactSuggestedFixKind::Instruction
        },
        severity: 2,
    }
}

struct PatternAccumulator {
    suite: String,
    label: String,
    description: String,
    supporting_trace_ids: Vec<String>,
    frequency: usize,
    severity: u8,
    suggested_fix_kind: SleepArtifactSuggestedFixKind,
}

fn classify_failure(record: &ProgramTraceRecord, error: &str) -> String {
    if error.contains("provider_error") {
        return "provider_error".to_string();
    }
    if let Some(field) = extract_quoted_after(error, "missing field ") {
        return format!("missing_field:{field}");
    }
    if let Some(variant) = extract_quoted_after(error, "unknown variant ") {
        return format!("unknown_variant:{variant}");
    }
    if error.contains("invalid type") {
        return "invalid_type".to_string();
    }
    if error.contains("expected value") || error.contains("EOF while parsing") {
        return "malformed_json".to_string();
    }
    if record.program_name == "resolve_telegram_chat" && error.contains("action") {
        return "resolve_chat_schema_drift".to_string();
    }
    "deserialization_error".to_string()
}

fn describe_failure(record: &ProgramTraceRecord, error: &str, label: &str) -> String {
    match label {
        l if l.starts_with("missing_field:") => {
            let field = l.trim_start_matches("missing_field:");
            format!(
                "{} 在运行时输出缺少关键字段 `{}`，需要通过 demos、stress case 或 instruction 保持结构稳定。",
                record.program_name, field
            )
        }
        l if l.starts_with("unknown_variant:") => {
            let variant = l.trim_start_matches("unknown_variant:");
            format!(
                "{} 在运行时输出了未知枚举 `{}`，说明动作/分支边界仍有 schema 漂移。",
                record.program_name, variant
            )
        }
        "invalid_type" => format!(
            "{} 在运行时输出字段类型错误，说明当前候选对结构约束仍不稳定。",
            record.program_name
        ),
        "malformed_json" => format!(
            "{} 在运行时输出了无法解析的 JSON，说明输出格式稳定性不足。",
            record.program_name
        ),
        "provider_error" => format!(
            "{} 在运行时遇到 provider 级错误，需要区分接口兼容问题与程序语义问题。",
            record.program_name
        ),
        "resolve_chat_schema_drift" => {
            "resolve_telegram_chat 在运行时出现了扁平 action/结构漂移，需用 stress case 与 demo 固化输出边界。"
                .to_string()
        }
        _ => format!(
            "{} 在运行时出现结构化输出失败：{}",
            record.program_name, error
        ),
    }
}

fn suggested_fix_kind(label: &str) -> SleepArtifactSuggestedFixKind {
    if label.starts_with("missing_field:") || label.starts_with("unknown_variant:") {
        return SleepArtifactSuggestedFixKind::StressCase;
    }
    if label == "resolve_chat_schema_drift" {
        return SleepArtifactSuggestedFixKind::Demo;
    }
    SleepArtifactSuggestedFixKind::Instruction
}

fn derive_severity(error: &str) -> u8 {
    if error.contains("provider_error") {
        3
    } else if error.contains("unknown variant") || error.contains("missing field") {
        2
    } else {
        1
    }
}

fn extract_quoted_after(text: &str, prefix: &str) -> Option<String> {
    let start = text.find(prefix)? + prefix.len();
    let rest = &text[start..];
    let first_quote = rest.find('`').or_else(|| rest.find('\''))?;
    let quote = rest.as_bytes()[first_quote] as char;
    let after = &rest[first_quote + 1..];
    let end = after.find(quote)?;
    Some(after[..end].to_string())
}

fn slugify(value: &str) -> String {
    let mut slug = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if matches!(ch, ':' | ' ' | '-' | '_' | '.') && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

async fn derive_sleep_artifacts(
    context: &mut Context,
    patterns: &[SleepArtifactFailurePattern],
) -> Result<DerivedSleepArtifacts> {
    if patterns.is_empty() {
        return Ok(DerivedSleepArtifacts {
            bootstrap_demos: Vec::new(),
            stress_cases: Vec::new(),
            instruction_hypotheses: Vec::new(),
        });
    }

    let renderer = OpenAIToolRenderer;
    let program = SleepArtifactBuilderProgram;
    let mut bootstrap_demos = Vec::new();
    let mut stress_cases = Vec::new();
    let mut instruction_hypotheses = Vec::new();

    for pattern in patterns {
        let related_memories = context.memory.search_mem(&pattern.description, 3).await;
        let evidence_summary = render_related_memories(&related_memories);
        let available_canonical_cases = suite_reference_case_names(&pattern.suite);
        let outcome = execute_program_with_ir_report(
            context.judge_llm.as_ref(),
            context,
            &renderer,
            &program,
            program.dataset_ir(
                pattern.suite.clone(),
                pattern.pattern_id.clone(),
                pattern.description.clone(),
                pattern.frequency,
                pattern.severity,
                format!("{:?}", pattern.suggested_fix_kind),
                pattern.supporting_trace_ids.join("\n"),
                evidence_summary.clone().unwrap_or_else(|| "无".to_string()),
                available_canonical_cases.join("\n"),
            ),
            &program.default_tuning(),
            TraceOrigin::Sleep,
        )
        .await?;

        if let Some(artifact) = to_instruction_hypothesis(pattern, &outcome.output) {
            instruction_hypotheses.push(artifact);
        }
        if let Some(artifact) = to_bootstrap_demo(
            pattern,
            &related_memories,
            evidence_summary.as_deref(),
            &outcome.output,
        ) {
            bootstrap_demos.push(artifact);
        }
        if let Some(artifact) = to_stress_case(pattern, &related_memories, &outcome.output) {
            stress_cases.push(artifact);
        }
    }

    Ok(DerivedSleepArtifacts {
        bootstrap_demos,
        stress_cases,
        instruction_hypotheses,
    })
}

fn render_related_memories(related_memories: &[String]) -> Option<String> {
    if related_memories.is_empty() {
        return None;
    }
    Some(
        related_memories
            .iter()
            .take(3)
            .enumerate()
            .map(|(index, memory)| format!("{}. {}", index + 1, memory.trim()))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn render_input_summary(
    pattern: &SleepArtifactFailurePattern,
    evidence_summary: Option<&str>,
) -> String {
    match evidence_summary {
        Some(evidence) => format!(
            "failure pattern: {}\n相关 L2 记忆：\n{}",
            pattern.description, evidence
        ),
        None => format!("failure pattern: {}", pattern.description),
    }
}

fn suite_reference_case_names(suite: &str) -> Vec<String> {
    match suite {
        "resolve_telegram_chat" => crate::reasoning::datasets::resolve_telegram::all_case_names(),
        "action_phase.attend_notifications"
        | "action_phase.execute_task"
        | "action_phase.plan_from_project"
        | "action_phase.explore_new_tasks" => {
            crate::reasoning::datasets::action_phase::all_case_names_for_suite(suite)
        }
        _ => Vec::new(),
    }
}

fn to_instruction_hypothesis(
    pattern: &SleepArtifactFailurePattern,
    output: &SleepArtifactBuilderOutput,
) -> Option<SleepArtifactInstructionHypothesis> {
    if !output.create_instruction_hypothesis || output.instruction_text.trim().is_empty() {
        return None;
    }
    Some(SleepArtifactInstructionHypothesis {
        suite: pattern.suite.clone(),
        text: output.instruction_text.trim().to_string(),
        justification: output.reason.trim().to_string(),
        source_pattern_ids: vec![pattern.pattern_id.clone()],
    })
}

fn to_bootstrap_demo(
    pattern: &SleepArtifactFailurePattern,
    related_memories: &[String],
    evidence_summary: Option<&str>,
    output: &SleepArtifactBuilderOutput,
) -> Option<SleepArtifactBootstrapDemo> {
    if !output.create_bootstrap_demo
        || output.bootstrap_demo_title.trim().is_empty()
        || output.reference_case_names.is_empty()
    {
        return None;
    }
    Some(SleepArtifactBootstrapDemo {
        suite: pattern.suite.clone(),
        title: output.bootstrap_demo_title.trim().to_string(),
        input_summary: render_input_summary(pattern, evidence_summary),
        inputs: vec![ExampleField {
            name: "sleep artifact summary".to_string(),
            value: render_input_summary(pattern, evidence_summary),
        }],
        expected_output: json!({
            "suite": pattern.suite,
            "pattern_id": pattern.pattern_id,
            "target": "avoid_failure_pattern",
            "summary": output.bootstrap_demo_summary.trim(),
            "related_memories": related_memories,
        }),
        reference_case_names: output.reference_case_names.clone(),
        source_trace_ids: pattern.supporting_trace_ids.clone(),
        confidence: output.confidence.clamp(0.0, 1.0) as f32,
    })
}

fn to_stress_case(
    pattern: &SleepArtifactFailurePattern,
    related_memories: &[String],
    output: &SleepArtifactBuilderOutput,
) -> Option<SleepArtifactStressCase> {
    if !output.create_stress_case
        || output.stress_case_name.trim().is_empty()
        || output.reference_case_names.is_empty()
    {
        return None;
    }
    Some(SleepArtifactStressCase {
        suite: pattern.suite.clone(),
        name: output.stress_case_name.trim().to_string(),
        input_ir: json!({
            "suite": pattern.suite,
            "pattern_id": pattern.pattern_id,
            "description": pattern.description,
            "related_memories": related_memories,
        }),
        expected_constraints: output.stress_constraints.clone(),
        reference_case_names: output.reference_case_names.clone(),
        source_pattern_id: pattern.pattern_id.clone(),
        repeat: pattern.frequency.max(2),
        weight: usize::from(pattern.severity.max(1)),
    })
}

fn derive_success_bootstrap_demos(
    records: &[ProgramTraceRecord],
) -> Vec<SleepArtifactBootstrapDemo> {
    let mut per_suite = std::collections::HashMap::<String, usize>::new();
    let mut demos = Vec::new();

    for record in records {
        if record.deserialization_error.is_some() || record.attempt != 1 {
            continue;
        }
        let Some(parsed_output) = record.parsed_output.clone() else {
            continue;
        };
        let Some(suite) = infer_runtime_suite(record) else {
            continue;
        };
        let inputs = extract_inputs_from_request(&record.request);
        if inputs.is_empty() {
            continue;
        }
        let count = per_suite.entry(suite.clone()).or_insert(0);
        if *count >= 3 {
            continue;
        }
        *count += 1;
        demos.push(SleepArtifactBootstrapDemo {
            suite,
            title: format!("Sleep success trace {} #{}", record.program_name, count),
            input_summary: inputs
                .iter()
                .map(|field| format!("{}: {}", field.name, field.value))
                .collect::<Vec<_>>()
                .join("\n"),
            inputs,
            expected_output: parsed_output,
            reference_case_names: Vec::new(),
            source_trace_ids: vec![format!(
                "{}:{}:{}",
                record.program_name, record.timestamp_ms, record.attempt
            )],
            confidence: 0.8,
        });
    }

    demos
}

fn infer_runtime_suite(record: &ProgramTraceRecord) -> Option<String> {
    if record.program_name == "resolve_telegram_chat" {
        return Some("resolve_telegram_chat".to_string());
    }
    if record.program_name != "decide_next_action" {
        return None;
    }
    let inputs = extract_inputs_from_request(&record.request);
    let phase = inputs
        .iter()
        .find(|field| field.name.trim() == "阶段")
        .map(|field| field.value.trim());
    match phase {
        Some("处理提醒") => Some("action_phase.attend_notifications".to_string()),
        Some("执行下一步动作") => Some("action_phase.execute_task".to_string()),
        Some("为项目规划下一步") => Some("action_phase.plan_from_project".to_string()),
        Some("探索与规划新任务") => Some("action_phase.explore_new_tasks".to_string()),
        _ => None,
    }
}

fn extract_inputs_from_request(request: &PromptRequest) -> Vec<ExampleField> {
    let mut inputs = Vec::new();
    for message in &request.messages {
        if !matches!(message.role, PromptRole::User) {
            continue;
        }
        inputs.extend(parse_user_sections(&message.content));
    }
    inputs
}

fn parse_user_sections(content: &str) -> Vec<ExampleField> {
    let mut fields = Vec::new();
    let mut current_title: Option<String> = None;
    let mut current_body = String::new();

    for line in content.lines() {
        if let Some(title) = line.strip_prefix("## ") {
            flush_section(&mut fields, &mut current_title, &mut current_body);
            current_title = Some(title.trim().to_string());
        } else {
            if !current_body.is_empty() {
                current_body.push('\n');
            }
            current_body.push_str(line);
        }
    }
    flush_section(&mut fields, &mut current_title, &mut current_body);

    fields
}

fn flush_section(
    fields: &mut Vec<ExampleField>,
    current_title: &mut Option<String>,
    current_body: &mut String,
) {
    let Some(title) = current_title.take() else {
        current_body.clear();
        return;
    };
    let trimmed = current_body.trim();
    if trimmed.is_empty() {
        current_body.clear();
        return;
    }
    if matches!(title.as_str(), "程序签名" | "示例") {
        current_body.clear();
        return;
    }
    fields.push(ExampleField {
        name: title,
        value: trimmed.to_string(),
    });
    current_body.clear();
}

async fn promote_failure_patterns_to_l3(
    context: &Context,
    patterns: &[SleepArtifactFailurePattern],
) -> Result<Vec<L3EntryDraft>> {
    if patterns.is_empty() {
        return Ok(Vec::new());
    }

    let renderer = OpenAIToolRenderer;
    let program = SleepL3PromoterProgram;
    let mut entries = Vec::new();

    for pattern in patterns {
        let ir = program.dataset_ir(
            pattern.suite.clone(),
            pattern.pattern_id.clone(),
            pattern.description.clone(),
            pattern.frequency,
            pattern.severity,
            format!("{:?}", pattern.suggested_fix_kind),
            pattern.supporting_trace_ids.join("\n"),
        );
        let outcome = execute_program_with_ir_report(
            context.judge_llm.as_ref(),
            context,
            &renderer,
            &program,
            ir,
            &program.default_tuning(),
            TraceOrigin::Sleep,
        )
        .await?;

        if let Some(entry) = to_l3_entry(pattern, &outcome.output) {
            entries.push(entry);
        }
    }

    Ok(entries)
}

async fn promote_success_patterns_to_l3(
    context: &mut Context,
    records: &[ProgramTraceRecord],
) -> Result<Vec<L3EntryDraft>> {
    let renderer = OpenAIToolRenderer;
    let program = SleepSuccessL3PromoterProgram;
    let mut entries = Vec::new();
    let mut per_suite = std::collections::HashMap::<String, usize>::new();

    for record in records {
        if record.deserialization_error.is_some() || record.attempt != 1 {
            continue;
        }
        let Some(suite) = infer_runtime_suite(record) else {
            continue;
        };
        let count = per_suite.entry(suite.clone()).or_insert(0);
        if *count >= 2 {
            continue;
        }
        let inputs = extract_inputs_from_request(&record.request);
        if inputs.is_empty() {
            continue;
        }
        let input_summary = inputs
            .iter()
            .map(|field| format!("{}: {}", field.name, field.value))
            .collect::<Vec<_>>()
            .join("\n");
        let output_summary = record
            .parsed_output
            .as_ref()
            .map(serde_json::to_string_pretty)
            .transpose()
            .map_err(|err| miette!("failed to format parsed success output: {err}"))?
            .unwrap_or_else(|| "无".to_string());
        let related_memories = context.memory.search_mem(&input_summary, 3).await;
        let outcome = execute_program_with_ir_report(
            context.judge_llm.as_ref(),
            context,
            &renderer,
            &program,
            program.dataset_ir(
                suite.clone(),
                format!(
                    "{}:{}:{}",
                    record.program_name, record.timestamp_ms, record.attempt
                ),
                input_summary,
                output_summary,
                render_related_memories(&related_memories).unwrap_or_else(|| "无".to_string()),
            ),
            &program.default_tuning(),
            TraceOrigin::Sleep,
        )
        .await?;
        if let Some(entry) = to_success_l3_entry(record, &outcome.output) {
            *count += 1;
            entries.push(entry);
        }
    }

    Ok(entries)
}

async fn promote_episode_success_patterns_to_l3(
    context: &mut Context,
    outcomes: &[EpisodeOutcome],
) -> Result<Vec<L3EntryDraft>> {
    let renderer = OpenAIToolRenderer;
    let program = SleepSuccessL3PromoterProgram;
    let mut entries = Vec::new();
    let mut per_suite = std::collections::HashMap::<String, usize>::new();

    for outcome in outcomes {
        if outcome.status != EpisodeStatus::Succeeded {
            continue;
        }
        let Some(step) = outcome
            .steps
            .iter()
            .rev()
            .find(|step| infer_episode_suite_from_step(step).is_some())
        else {
            continue;
        };
        let Some(suite) = infer_episode_suite_from_step(step) else {
            continue;
        };
        let count = per_suite.entry(suite.to_string()).or_insert(0);
        if *count >= 2 {
            continue;
        }
        let input_summary = render_episode_input_summary(outcome, step);
        let output_summary = serde_json::to_string_pretty(&step_to_output(step))
            .map_err(|err| miette!("failed to format parsed episode success output: {err}"))?;
        let related_memories = context.memory.search_mem(&input_summary, 3).await;
        let trace_id = format!("episode:{}", outcome.task.id);
        let outcome = execute_program_with_ir_report(
            context.judge_llm.as_ref(),
            context,
            &renderer,
            &program,
            program.dataset_ir(
                suite.to_string(),
                trace_id.clone(),
                input_summary,
                output_summary,
                render_related_memories(&related_memories).unwrap_or_else(|| "无".to_string()),
            ),
            &program.default_tuning(),
            TraceOrigin::Sleep,
        )
        .await?;
        if let Some(entry) = to_episode_success_l3_entry(&trace_id, &outcome.output) {
            *count += 1;
            entries.push(entry);
        }
    }

    Ok(entries)
}

fn to_l3_entry(
    pattern: &SleepArtifactFailurePattern,
    output: &SleepL3PromoterOutput,
) -> Option<L3EntryDraft> {
    if !output.promote {
        return None;
    }
    if output.lesson.trim().is_empty() || output.retrieval_text.trim().is_empty() {
        return None;
    }
    Some(L3EntryDraft {
        kind: output.kind.clone(),
        lesson: output.lesson.trim().to_string(),
        evidence_summary: if output.evidence_summary.trim().is_empty() {
            pattern.description.clone()
        } else {
            output.evidence_summary.trim().to_string()
        },
        retrieval_text: output.retrieval_text.trim().to_string(),
        confidence: output.confidence.clamp(0.0, 1.0) as f32,
        stability: output.stability.clone(),
        source_trace_ids: pattern.supporting_trace_ids.clone(),
    })
}

fn to_success_l3_entry(
    record: &ProgramTraceRecord,
    output: &SleepSuccessL3PromoterOutput,
) -> Option<L3EntryDraft> {
    if !output.promote {
        return None;
    }
    if output.lesson.trim().is_empty() || output.retrieval_text.trim().is_empty() {
        return None;
    }
    Some(L3EntryDraft {
        kind: output.kind.clone(),
        lesson: output.lesson.trim().to_string(),
        evidence_summary: output.evidence_summary.trim().to_string(),
        retrieval_text: output.retrieval_text.trim().to_string(),
        confidence: output.confidence.clamp(0.0, 1.0) as f32,
        stability: output.stability.clone(),
        source_trace_ids: vec![format!(
            "{}:{}:{}",
            record.program_name, record.timestamp_ms, record.attempt
        )],
    })
}

fn to_episode_success_l3_entry(
    trace_id: &str,
    output: &SleepSuccessL3PromoterOutput,
) -> Option<L3EntryDraft> {
    if !output.promote {
        return None;
    }
    if output.lesson.trim().is_empty() || output.retrieval_text.trim().is_empty() {
        return None;
    }
    Some(L3EntryDraft {
        kind: output.kind.clone(),
        lesson: output.lesson.trim().to_string(),
        evidence_summary: output.evidence_summary.trim().to_string(),
        retrieval_text: output.retrieval_text.trim().to_string(),
        confidence: output.confidence.clamp(0.0, 1.0) as f32,
        stability: output.stability.clone(),
        source_trace_ids: vec![trace_id.to_string()],
    })
}
