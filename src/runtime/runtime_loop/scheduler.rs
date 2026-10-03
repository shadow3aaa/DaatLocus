use super::sleep_driver::{maybe_start_forced_sleep, maybe_start_idle_sleep};
use super::{
    Context, DashboardState, Duration, EventStatus, PendingWork, RuntimeStatusLevel,
    RuntimeTurnPhase, SleepStatusSnapshot, SleepTaskResult, clear_runtime_status,
    execute_agent_loop_step, requeue_claimed_runtime_events, set_runtime_status,
    set_runtime_status_only, sync_dashboard_state,
};

pub enum RuntimeLoopCycle {
    Idle,
    ProcessedWork,
}

pub async fn daat_locus_loop(
    context: &mut Context,
    tx: &tokio::sync::watch::Sender<DashboardState>,
    sleep_result_tx: &tokio::sync::mpsc::UnboundedSender<SleepTaskResult>,
    session_title_result_tx: &tokio::sync::mpsc::UnboundedSender<
        crate::runtime::session_title::SessionTitleGenerationResult,
    >,
    sleep_running: &mut bool,
    sleep_status: &mut SleepStatusSnapshot,
) -> RuntimeLoopCycle {
    let cycle_started_at = std::time::Instant::now();
    let config_reload = crate::config_hot_reload::maybe_reload_config(context).await;
    if let Some(text) = crate::config_hot_reload::config_reload_status_text(&config_reload) {
        set_runtime_status(Some(tx), RuntimeStatusLevel::Info, text);
        sync_dashboard_state(context, tx, sleep_status, None);
    }

    let forced_sleep_status =
        maybe_start_forced_sleep(context, tx, sleep_result_tx, sleep_running, sleep_status).await;
    refresh_cached_sleep_status_queues(sleep_status).await;
    sync_driver_frontier_from_sources(context);
    if context.active_runtime_turn {
        tracing::warn!(
            elapsed_secs = context
                .runtime_turn_started_at
                .map_or(0, |t| t.elapsed().as_secs()),
            phase = context
                .active_runtime_phase
                .map_or("running", crate::context::RuntimeTurnPhase::label),
            "stale active_runtime_turn detected at loop entry; resetting cancelled turn"
        );
        reset_cancelled_runtime_turn(context, "stale active_runtime_turn at loop entry");
    }
    let pending_work_count = context.pending_work.pending_count();
    if pending_work_count == 0 {
        if context.idle_since.is_none() {
            context.idle_since = Some(std::time::Instant::now());
        }
        crate::runtime::session_title::spawn_session_title_generation(
            context,
            session_title_result_tx,
        );
        if let Some(status) =
            maybe_start_idle_sleep(context, tx, sleep_result_tx, sleep_running, sleep_status).await
        {
            set_runtime_status_only(Some(tx), status);
        } else if let Some(status) = forced_sleep_status {
            set_runtime_status_only(Some(tx), status);
        } else {
            clear_runtime_status(Some(tx));
        }
        sync_dashboard_state(
            context,
            tx,
            sleep_status,
            Some(cycle_started_at.elapsed().as_millis()),
        );
        return RuntimeLoopCycle::Idle;
    }
    context.idle_since = None;
    let mut status = format!("processing: {pending_work_count} pending work item(s)");
    if let Some(forced_sleep_status) = forced_sleep_status.as_deref() {
        status.push_str(" | ");
        status.push_str(forced_sleep_status);
    }
    set_runtime_status_only(Some(tx), status);
    context
        .apps
        .wait_until_settled(Duration::from_secs(1), Duration::from_secs(3))
        .await;
    let runtime_turn_started_at = std::time::Instant::now();
    context.active_runtime_turn = true;
    context.runtime_turn_epoch = context.runtime_turn_epoch.wrapping_add(1);
    context.runtime_turn_started_at = Some(runtime_turn_started_at);
    context.runtime_turn_started_at_ms = Some(chrono::Utc::now().timestamp_millis());
    context.set_runtime_phase(Some(RuntimeTurnPhase::PreflightPreTurnContext));
    sync_dashboard_state(
        context,
        tx,
        sleep_status,
        Some(cycle_started_at.elapsed().as_millis()),
    );
    let _ = execute_agent_loop_step(context, Some(tx)).await;
    context.active_runtime_turn = false;
    context.runtime_turn_started_at = None;
    context.runtime_turn_started_at_ms = None;
    crate::runtime::session_title::spawn_session_title_generation(context, session_title_result_tx);
    sync_dashboard_state(
        context,
        tx,
        sleep_status,
        Some(cycle_started_at.elapsed().as_millis()),
    );
    RuntimeLoopCycle::ProcessedWork
}

fn sync_driver_frontier_from_sources(context: &Context) {
    for (event_id, status) in context.events.driver_event_statuses() {
        let work = PendingWork::Event { event_id };
        if matches!(status, crate::events::EventStatus::Pending) {
            if let Err(err) = context.pending_work.enqueue(work) {
                tracing::error!("failed to sync pending event driver {event_id}: {err:?}");
            }
        } else if let Err(err) = context.pending_work.consume(&work) {
            tracing::error!("failed to remove stale event driver {event_id}: {err:?}");
        }
    }
}

fn recover_stale_runtime_turn_claims(context: &mut Context) {
    let mut claimed_event_ids = std::mem::take(&mut context.claimed_event_ids);
    if claimed_event_ids.is_empty() {
        claimed_event_ids = context
            .events
            .driver_event_statuses()
            .into_iter()
            .filter(|(_, status)| matches!(status, EventStatus::Claimed))
            .map(|(event_id, _)| event_id.to_string())
            .collect();
    }
    if !claimed_event_ids.is_empty() {
        requeue_claimed_runtime_events(context, &claimed_event_ids);
    }

    context.install_live_progress(None);
    context.current_work_origin = None;
}

pub fn reset_cancelled_runtime_turn(context: &mut Context, reason: &str) {
    recover_stale_runtime_turn_claims(context);
    tracing::warn!(reason, "reset cancelled active runtime turn");
    context.active_runtime_turn = false;
    context.set_runtime_phase(None);
    context.runtime_turn_started_at = None;
    context.runtime_turn_started_at_ms = None;
    clear_stale_live_activity_cells(context);
}

fn clear_stale_live_activity_cells(context: &Context) {
    if let Some(tx) = &context.dashboard_tx {
        tx.send_modify(crate::dashboard::clear_transient_live_activity_cells);
    }
}

pub fn interrupt_active_runtime_turn(context: &mut Context, reason: &str) -> usize {
    let mut claimed_event_ids = std::mem::take(&mut context.claimed_event_ids);
    if claimed_event_ids.is_empty() {
        claimed_event_ids = context
            .events
            .driver_event_statuses()
            .into_iter()
            .filter(|(_, status)| matches!(status, EventStatus::Claimed))
            .map(|(event_id, _)| event_id.to_string())
            .collect();
    }
    let mut failed_events = 0usize;
    for event_id in claimed_event_ids {
        if let Err(err) = context.events.set_status(
            &event_id,
            EventStatus::Failed,
            Some(format!("runtime turn interrupted by user: {reason}")),
        ) {
            tracing::error!("failed to mark interrupted runtime event {event_id} failed: {err:?}");
        } else {
            failed_events += 1;
        }
        if let Ok(parsed_event_id) = uuid::Uuid::parse_str(&event_id)
            && let Err(err) = context.pending_work.consume(&PendingWork::Event {
                event_id: parsed_event_id,
            })
        {
            tracing::error!(
                "failed to consume interrupted runtime event driver {event_id}: {err:?}"
            );
        }
    }

    if failed_events > 0 {
        tracing::warn!(
            reason,
            failed_events,
            "interrupted active runtime turn and terminated claimed inputs"
        );
    } else {
        tracing::warn!(
            reason,
            "interrupted active runtime turn with no claimed inputs"
        );
    }

    context.install_live_progress(None);
    context.current_work_origin = None;
    context.active_runtime_turn = false;
    context.set_runtime_phase(None);
    context.runtime_turn_started_at = None;
    context.runtime_turn_started_at_ms = None;

    clear_stale_live_activity_cells(context);

    failed_events
}

#[derive(Clone, Copy)]
struct QueueFileStamp {
    modified: Option<std::time::SystemTime>,
    len: u64,
    count: usize,
    present: bool,
}

struct SleepQueueCountCache {
    runtime_error_cases: Option<QueueFileStamp>,
    skill_run_records: Option<QueueFileStamp>,
}

fn sleep_queue_count_cache() -> &'static std::sync::Mutex<SleepQueueCountCache> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<SleepQueueCountCache>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        std::sync::Mutex::new(SleepQueueCountCache {
            runtime_error_cases: None,
            skill_run_records: None,
        })
    })
}

async fn refresh_cached_sleep_status_queues(status: &mut SleepStatusSnapshot) {
    let paths = crate::daat_locus_paths::daat_locus_paths().await;
    let error_cases = paths.journal_file("runtime_error_cases.jsonl");
    let skill_runs = paths.runtime_dir().join("skills").join("run_records.jsonl");
    if let Some(count) =
        cached_nonempty_line_count(error_cases, QueueCacheSlot::RuntimeErrorCases).await
    {
        status.unread_runtime_error_backlog = count;
    }
    if let Some(count) =
        cached_nonempty_line_count(skill_runs, QueueCacheSlot::SkillRunRecords).await
    {
        status.skill_evidence_records = count;
    }
}

#[derive(Clone, Copy)]
enum QueueCacheSlot {
    RuntimeErrorCases,
    SkillRunRecords,
}

async fn cached_nonempty_line_count(
    path: std::path::PathBuf,
    slot: QueueCacheSlot,
) -> Option<usize> {
    let metadata = match tokio::fs::metadata(&path).await {
        Ok(metadata) => Some(metadata),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            tracing::warn!("failed to stat sleep queue file {}: {err}", path.display());
            return None;
        }
    };
    let stamp = metadata
        .as_ref()
        .map(|metadata| (metadata.modified().ok(), metadata.len()));
    if let Ok(cache) = sleep_queue_count_cache().lock()
        && let Some(cached) = cache_slot(&cache, slot)
        && cached.present == metadata.is_some()
        && metadata.as_ref().is_none_or(|metadata| {
            cached.modified == metadata.modified().ok() && cached.len == metadata.len()
        })
    {
        return Some(cached.count);
    }

    let count = match metadata {
        None => 0,
        Some(_) => match tokio::fs::read(&path).await {
            Ok(bytes) => bytes
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
                .count(),
            Err(err) => {
                tracing::warn!("failed to read sleep queue file {}: {err}", path.display());
                return None;
            }
        },
    };
    if let Ok(mut cache) = sleep_queue_count_cache().lock() {
        let stored = QueueFileStamp {
            modified: stamp.and_then(|(modified, _)| modified),
            len: stamp.map_or(0, |(_, len)| len),
            count,
            present: metadata.is_some(),
        };
        match slot {
            QueueCacheSlot::RuntimeErrorCases => cache.runtime_error_cases = Some(stored),
            QueueCacheSlot::SkillRunRecords => cache.skill_run_records = Some(stored),
        }
    }
    Some(count)
}

fn cache_slot(cache: &SleepQueueCountCache, slot: QueueCacheSlot) -> Option<QueueFileStamp> {
    match slot {
        QueueCacheSlot::RuntimeErrorCases => cache.runtime_error_cases,
        QueueCacheSlot::SkillRunRecords => cache.skill_run_records,
    }
}
