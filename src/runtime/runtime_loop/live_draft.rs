use std::time::{Duration, Instant};

use super::{Context, EventPayload, EventView, Result};
use crate::{
    dashboard::{
        DashboardActivityEvent, DashboardState, apply_activity_event, assistant_activity_cell,
        thinking_activity_cell,
    },
    live_progress::{LiveProgressEvent, TelegramLiveStatus},
    telegram_transport::state::TelegramTransportStateHandle,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time::MissedTickBehavior,
};

const TELEGRAM_MESSAGE_LIMIT: usize = 4096;
const MAX_LIVE_DRAFT_STATUSES: usize = 5;
const MAX_RECENT_STATUSES_WITH_STICKY_WORKFLOW: usize = 4;
const MARKDOWN_V2_ELLIPSIS: &str = "\\.\\.\\.";
const TELEGRAM_DRAFT_FLUSH_INTERVAL: Duration = Duration::from_millis(900);
const DASHBOARD_DRAFT_FLUSH_INTERVAL: Duration = Duration::from_millis(200);
const DASHBOARD_THINKING_DRAFT_KEY: &str = "thinking-draft";
const DASHBOARD_ASSISTANT_DRAFT_KEY: &str = "assistant-draft";

pub(super) struct LiveProgressSession {
    join: JoinHandle<()>,
}

impl LiveProgressSession {
    pub(super) async fn shutdown(self, context: &Context) {
        context.install_live_progress(None);
        let _ = tokio::time::timeout(Duration::from_secs(2), self.join).await;
    }
}

struct TelegramDraftTarget {
    chat_id: i64,
    draft_id: i64,
    event_id: String,
    previous_sent_text: Option<String>,
}

/// Start the per-turn live progress session.
///
/// Dashboard draft cells are produced for every turn. Telegram live drafts are
/// produced only for private-chat Telegram events.
pub(super) fn maybe_start_live_progress_session(
    context: &Context,
    claimed_event_views: &[EventView],
) -> Option<LiveProgressSession> {
    let telegram_target = telegram_draft_target(context, claimed_event_views);
    let dashboard_tx = context.dashboard_tx.clone();
    if telegram_target.is_none() && dashboard_tx.is_none() {
        return None;
    }
    let (tx, mut rx) = mpsc::unbounded_channel::<LiveProgressEvent>();
    context.install_live_progress(Some(tx));
    let mut telegram = telegram_target.map(|target| TelegramDraftProcessor::start(context, target));
    let join = tokio::spawn(async move {
        let mut dashboard = DashboardLiveDraftState::default();
        let mut dashboard_interval = tokio::time::interval(DASHBOARD_DRAFT_FLUSH_INTERVAL);
        dashboard_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut telegram_interval = tokio::time::interval(TELEGRAM_DRAFT_FLUSH_INTERVAL);
        telegram_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                maybe_event = rx.recv() => {
                    match maybe_event {
                        Some(event) => {
                            dashboard.apply(&event, dashboard_tx.as_ref());
                            if let Some(processor) = telegram.as_mut() {
                                processor.apply(event);
                            }
                        }
                        None => break,
                    }
                }
                _ = dashboard_interval.tick() => dashboard.flush(dashboard_tx.as_ref()),
                _ = telegram_interval.tick() => {
                    if let Some(processor) = telegram.as_mut() {
                        processor.flush();
                    }
                }
            }
        }
        dashboard.end_cells(dashboard_tx.as_ref());
        if let Some(processor) = telegram.as_mut() {
            processor.flush();
        }
    });
    Some(LiveProgressSession { join })
}

fn telegram_draft_target(
    context: &Context,
    claimed_event_views: &[EventView],
) -> Option<TelegramDraftTarget> {
    if claimed_event_views.len() != 1 {
        return None;
    }
    let event = claimed_event_views.first()?;
    let EventPayload::TelegramIncoming(payload) = &event.payload else {
        return None;
    };
    if payload.chat_kind != "private" {
        return None;
    }
    if !context.config.telegram.enabled || !context.config.telegram.has_real_credentials() {
        return None;
    }
    let chat_id = payload.chat_id.parse::<i64>().ok()?;
    let event_id = event.event_id.to_string();
    let (draft_id, previous_sent_text) = context
        .get_or_create_telegram_live_draft(event_id.clone(), stable_live_draft_id(event.event_id));
    Some(TelegramDraftTarget {
        chat_id,
        draft_id,
        event_id,
        previous_sent_text,
    })
}

struct TelegramDraftProcessor {
    telegram: TelegramTransportStateHandle,
    live_drafts: crate::context::TelegramLiveDraftRegistry,
    event_id: String,
    chat_id: i64,
    draft_id: i64,
    last_sent: String,
    state: TelegramLiveDraftState,
    dirty: bool,
}

impl TelegramDraftProcessor {
    fn start(context: &Context, target: TelegramDraftTarget) -> Self {
        let mut processor = Self {
            telegram: context.telegram.clone(),
            live_drafts: context.telegram_live_drafts.clone(),
            event_id: target.event_id,
            chat_id: target.chat_id,
            draft_id: target.draft_id,
            last_sent: target.previous_sent_text.clone().unwrap_or_default(),
            state: TelegramLiveDraftState::from_previous_sent(
                target.previous_sent_text.as_deref().unwrap_or_default(),
            ),
            dirty: false,
        };
        let initial_draft_text = processor.state.render_markdown_v2();
        if should_send_initial_live_draft(&processor.last_sent) {
            if let Err(err) = enqueue_live_draft(
                &processor.telegram,
                processor.chat_id,
                processor.draft_id,
                &initial_draft_text,
            ) {
                tracing::warn!("telegram initial live draft enqueue failed: {err:?}");
            } else {
                record_live_draft_sent(
                    &processor.live_drafts,
                    &processor.event_id,
                    &initial_draft_text,
                );
                processor.last_sent = initial_draft_text;
            }
        }
        processor
    }

    fn apply(&mut self, event: LiveProgressEvent) {
        apply_live_progress_event(&mut self.state, &mut self.dirty, event);
    }

    fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        let draft_text = self.state.render_markdown_v2();
        if draft_text != self.last_sent {
            if let Err(err) =
                enqueue_live_draft(&self.telegram, self.chat_id, self.draft_id, &draft_text)
            {
                tracing::warn!("telegram live draft enqueue failed: {err:?}");
            } else {
                record_live_draft_sent(&self.live_drafts, &self.event_id, &draft_text);
                self.last_sent = draft_text;
            }
        }
        self.dirty = false;
    }
}

#[derive(Default)]
struct DashboardLiveDraftState {
    thinking: String,
    assistant: String,
    has_drafts: bool,
    dirty: bool,
    last_flush_at: Option<Instant>,
}

impl DashboardLiveDraftState {
    fn apply(&mut self, event: &LiveProgressEvent, tx: Option<&watch::Sender<DashboardState>>) {
        match event {
            LiveProgressEvent::GenerationStarted | LiveProgressEvent::DraftReset => self.reset(tx),
            LiveProgressEvent::AssistantContent { content } => {
                self.assistant.clone_from(content);
                self.mark_dirty(tx);
            }
            LiveProgressEvent::ReasoningContent { content } => {
                self.thinking.clone_from(content);
                self.mark_dirty(tx);
            }
            LiveProgressEvent::TelegramStatus(_) => {}
        }
    }

    fn mark_dirty(&mut self, tx: Option<&watch::Sender<DashboardState>>) {
        self.dirty = true;
        let due = self
            .last_flush_at
            .is_none_or(|at| at.elapsed() >= DASHBOARD_DRAFT_FLUSH_INTERVAL);
        if due {
            self.flush(tx);
        }
    }

    fn flush(&mut self, tx: Option<&watch::Sender<DashboardState>>) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        self.last_flush_at = Some(Instant::now());
        let Some(tx) = tx else {
            return;
        };
        let thinking = thinking_activity_cell(&self.thinking);
        let assistant = assistant_activity_cell(&self.assistant);
        if thinking.is_none() && assistant.is_none() {
            return;
        }
        self.has_drafts = true;
        tx.send_modify(|state| {
            if let Some(event) = thinking {
                apply_activity_event(
                    state,
                    DashboardActivityEvent::LiveCellUpsert {
                        key: DASHBOARD_THINKING_DRAFT_KEY.to_string(),
                        event: Box::new(event),
                    },
                );
            }
            if let Some(event) = assistant {
                apply_activity_event(
                    state,
                    DashboardActivityEvent::LiveCellUpsert {
                        key: DASHBOARD_ASSISTANT_DRAFT_KEY.to_string(),
                        event: Box::new(event),
                    },
                );
            }
        });
    }

    fn reset(&mut self, tx: Option<&watch::Sender<DashboardState>>) {
        self.thinking.clear();
        self.assistant.clear();
        self.dirty = false;
        self.last_flush_at = None;
        self.end_cells(tx);
    }

    fn end_cells(&mut self, tx: Option<&watch::Sender<DashboardState>>) {
        if !self.has_drafts {
            return;
        }
        self.has_drafts = false;
        let Some(tx) = tx else {
            return;
        };
        tx.send_modify(|state| {
            apply_activity_event(
                state,
                DashboardActivityEvent::LiveCellEnd {
                    key: DASHBOARD_THINKING_DRAFT_KEY.to_string(),
                },
            );
            apply_activity_event(
                state,
                DashboardActivityEvent::LiveCellEnd {
                    key: DASHBOARD_ASSISTANT_DRAFT_KEY.to_string(),
                },
            );
        });
    }
}

fn enqueue_live_draft(
    telegram: &TelegramTransportStateHandle,
    chat_id: i64,
    draft_id: i64,
    text: &str,
) -> Result<()> {
    telegram.enqueue_outgoing_draft(chat_id.to_string(), draft_id, text.to_string())
}

fn stable_live_draft_id(event_id: uuid::Uuid) -> i64 {
    let bounded = event_id.as_u128() % (i64::MAX as u128);
    i64::try_from(bounded).expect("live-draft ID is reduced below i64::MAX") + 1
}

const fn should_send_initial_live_draft(last_sent: &str) -> bool {
    last_sent.is_empty()
}

fn record_live_draft_sent(
    live_drafts: &crate::context::TelegramLiveDraftRegistry,
    event_id: &str,
    text: &str,
) {
    if let Some(record) = live_drafts.lock().get_mut(event_id) {
        record.last_sent_text = Some(text.to_string());
    }
}

fn apply_live_progress_event(
    state: &mut TelegramLiveDraftState,
    dirty: &mut bool,
    event: LiveProgressEvent,
) {
    match event {
        LiveProgressEvent::GenerationStarted => {
            state.apply(LiveProgressEvent::GenerationStarted);
        }
        event => {
            if state.apply(event) {
                *dirty = true;
            }
        }
    }
}

#[derive(Default)]
struct TelegramLiveDraftState {
    previous_markdown_v2: Option<String>,
    sticky_workflow_status: Option<TelegramLiveStatus>,
    recent_statuses: Vec<TelegramLiveStatus>,
}

impl TelegramLiveDraftState {
    fn working() -> Self {
        Self::default()
    }

    fn from_previous_sent(previous_sent_text: &str) -> Self {
        let mut state = Self::working();
        if previous_sent_text.trim().is_empty() || previous_sent_text == "Working\\.\\.\\." {
            return state;
        }
        if let Some(statuses) = parse_statuses_markdown_v2(previous_sent_text) {
            state.restore_statuses(statuses);
        } else {
            state.previous_markdown_v2 = Some(previous_sent_text.to_string());
        }
        state
    }

    fn apply(&mut self, event: LiveProgressEvent) -> bool {
        match event {
            LiveProgressEvent::GenerationStarted
            | LiveProgressEvent::DraftReset
            | LiveProgressEvent::AssistantContent { .. }
            | LiveProgressEvent::ReasoningContent { .. } => false,
            LiveProgressEvent::TelegramStatus(status) => {
                let icon = status.icon.trim();
                let text = status.text.trim();
                if icon.is_empty() || text.is_empty() {
                    return false;
                }
                let status = TelegramLiveStatus {
                    icon: icon.to_string(),
                    text: text.to_string(),
                };
                let changed = self.apply_status(status);
                if changed {
                    self.previous_markdown_v2 = None;
                }
                changed
            }
        }
    }

    fn restore_statuses(&mut self, statuses: Vec<TelegramLiveStatus>) {
        for status in statuses {
            self.apply_status(status);
        }
        self.previous_markdown_v2 = None;
    }

    fn apply_status(&mut self, status: TelegramLiveStatus) -> bool {
        if is_sticky_workflow_status(&status) {
            let removed_created_status = self.consume_workflow_created_statuses();
            let changed = self.sticky_workflow_status.as_ref() != Some(&status)
                || self.previous_markdown_v2.is_some()
                || removed_created_status;
            self.sticky_workflow_status = Some(status);
            self.trim_recent_statuses();
            return changed;
        }

        let changed =
            self.recent_statuses.last() != Some(&status) || self.previous_markdown_v2.is_some();
        if !changed {
            return false;
        }
        self.recent_statuses.push(status);
        self.trim_recent_statuses();
        true
    }

    fn consume_workflow_created_statuses(&mut self) -> bool {
        let original_len = self.recent_statuses.len();
        self.recent_statuses
            .retain(|status| !is_workflow_created_status(status));
        self.recent_statuses.len() != original_len
    }

    fn trim_recent_statuses(&mut self) {
        let max_recent = if self.sticky_workflow_status.is_some() {
            MAX_RECENT_STATUSES_WITH_STICKY_WORKFLOW
        } else {
            MAX_LIVE_DRAFT_STATUSES
        };
        if self.recent_statuses.len() > max_recent {
            let remove_count = self.recent_statuses.len() - max_recent;
            self.recent_statuses.drain(0..remove_count);
        }
    }

    fn render_markdown_v2(&self) -> String {
        let statuses = self.render_statuses();
        if !statuses.is_empty() {
            return truncate_markdown_v2(render_statuses_markdown_v2(&statuses));
        }
        if let Some(previous) = &self.previous_markdown_v2 {
            return truncate_markdown_v2(previous.clone());
        }
        "Working\\.\\.\\.".to_string()
    }

    fn render_statuses(&self) -> Vec<&TelegramLiveStatus> {
        self.sticky_workflow_status
            .iter()
            .chain(self.recent_statuses.iter())
            .collect()
    }
}

fn is_sticky_workflow_status(status: &TelegramLiveStatus) -> bool {
    status.icon == crate::activity_event::glyph::WORKFLOW
        && status.text.starts_with("Workflow Active:")
}

fn is_workflow_created_status(status: &TelegramLiveStatus) -> bool {
    status.icon == crate::activity_event::glyph::WORKFLOW
        && status.text.starts_with("Workflow Created:")
}

fn render_statuses_markdown_v2(statuses: &[&TelegramLiveStatus]) -> String {
    statuses
        .iter()
        .map(|status| render_status_markdown_v2(status))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_status_markdown_v2(status: &TelegramLiveStatus) -> String {
    format!(
        "{} {}",
        escape_markdown_v2(status.icon.trim()),
        escape_markdown_v2(status.text.trim())
    )
}

fn parse_statuses_markdown_v2(text: &str) -> Option<Vec<TelegramLiveStatus>> {
    let statuses = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(parse_status_markdown_v2)
        .collect::<Option<Vec<_>>>()?;
    (!statuses.is_empty()).then_some(statuses)
}

fn parse_status_markdown_v2(line: &str) -> Option<TelegramLiveStatus> {
    let (icon, text) = line.split_once(' ')?;
    let icon = unescape_markdown_v2(icon.trim());
    let text = unescape_markdown_v2(text.trim());
    if icon.is_empty() || text.is_empty() {
        return None;
    }
    Some(TelegramLiveStatus { icon, text })
}

fn escape_markdown_v2(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '_' | '*' | '[' | ']' | '(' | ')' | '~' | '`' | '>' | '#' | '+' | '-' | '=' | '|'
            | '{' | '}' | '.' | '!' | '\\' => {
                escaped.push('\\');
                escaped.push(ch);
            }
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn unescape_markdown_v2(text: &str) -> String {
    let mut unescaped = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                unescaped.push(next);
            }
        } else {
            unescaped.push(ch);
        }
    }
    unescaped
}

fn truncate_markdown_v2(text: String) -> String {
    if text.chars().count() <= TELEGRAM_MESSAGE_LIMIT {
        return text;
    }
    let max_prefix_len = TELEGRAM_MESSAGE_LIMIT - MARKDOWN_V2_ELLIPSIS.chars().count();
    let mut truncated = text.chars().take(max_prefix_len).collect::<String>();
    while truncated.ends_with('\\') {
        truncated.pop();
    }
    truncated.push_str(MARKDOWN_V2_ELLIPSIS);
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(icon: &str, text: &str) -> TelegramLiveStatus {
        TelegramLiveStatus {
            icon: icon.to_string(),
            text: text.to_string(),
        }
    }

    #[test]
    fn live_draft_replaces_working_when_status_arrives() {
        let mut state = TelegramLiveDraftState::working();
        assert_eq!(state.render_markdown_v2(), "Working\\.\\.\\.");

        state.apply(LiveProgressEvent::TelegramStatus(status(
            crate::activity_event::glyph::PLAN,
            "Plan Updated",
        )));

        assert_eq!(state.render_markdown_v2(), "∷ Plan Updated");
    }

    #[test]
    fn live_draft_id_is_stable_for_event() {
        let event_id = uuid::Uuid::parse_str("65d9e9f8-ae4f-455c-af4f-1cc6d7d0368c").unwrap();

        assert_eq!(
            stable_live_draft_id(event_id),
            stable_live_draft_id(event_id)
        );
        assert!(stable_live_draft_id(event_id) > 0);
    }

    #[test]
    fn live_draft_initial_working_is_sent_only_without_prior_text() {
        assert!(should_send_initial_live_draft(""));
        assert!(!should_send_initial_live_draft("Working\\.\\.\\."));
        assert!(!should_send_initial_live_draft("∷ Plan Updated"));
    }

    #[test]
    fn live_draft_ignores_reasoning_and_assistant_content() {
        let mut state = TelegramLiveDraftState::working();
        assert!(!state.apply(LiveProgressEvent::ReasoningContent {
            content: "checking options".to_string(),
        }));
        assert!(!state.apply(LiveProgressEvent::AssistantContent {
            content: "Current answer.".to_string(),
        }));

        assert_eq!(state.render_markdown_v2(), "Working\\.\\.\\.");
    }

    #[test]
    fn live_draft_generation_started_preserves_last_status() {
        let mut state = TelegramLiveDraftState::working();
        state.apply(LiveProgressEvent::TelegramStatus(status(
            crate::activity_event::glyph::EXEC,
            "Command Ran",
        )));
        assert!(!state.apply(LiveProgressEvent::GenerationStarted));

        assert_eq!(state.render_markdown_v2(), "• Command Ran");
    }

    #[test]
    fn live_draft_generation_started_keeps_unflushed_status_dirty() {
        let mut state = TelegramLiveDraftState::working();
        let mut dirty = false;

        apply_live_progress_event(
            &mut state,
            &mut dirty,
            LiveProgressEvent::TelegramStatus(status(
                crate::activity_event::glyph::WORKFLOW,
                "Workflow Active: repo-analysis",
            )),
        );
        assert!(dirty);

        apply_live_progress_event(&mut state, &mut dirty, LiveProgressEvent::GenerationStarted);

        assert!(dirty);
        assert_eq!(
            state.render_markdown_v2(),
            "⌘ Workflow Active: repo\\-analysis"
        );
    }

    #[test]
    fn live_draft_fast_tool_status_survives_next_model_request() {
        let mut state = TelegramLiveDraftState::from_previous_sent("⌘ Workflow Active: simple");
        let mut dirty = false;

        apply_live_progress_event(
            &mut state,
            &mut dirty,
            LiveProgressEvent::TelegramStatus(status(
                crate::activity_event::glyph::PLAN,
                "Plan Updated",
            )),
        );
        apply_live_progress_event(&mut state, &mut dirty, LiveProgressEvent::GenerationStarted);

        assert!(dirty);
        assert_eq!(
            state.render_markdown_v2(),
            "⌘ Workflow Active: simple\n∷ Plan Updated"
        );
    }

    #[test]
    fn live_draft_restores_previous_sent_text_for_next_session() {
        let mut state = TelegramLiveDraftState::from_previous_sent("∷ Plan Updated");

        assert_eq!(state.render_markdown_v2(), "∷ Plan Updated");

        state.apply(LiveProgressEvent::TelegramStatus(status(
            crate::activity_event::glyph::EXEC,
            "Command Ran",
        )));
        assert_eq!(state.render_markdown_v2(), "∷ Plan Updated\n• Command Ran");
    }

    #[test]
    fn live_draft_keeps_only_last_five_statuses() {
        let mut state = TelegramLiveDraftState::working();
        for index in 1..=6 {
            state.apply(LiveProgressEvent::TelegramStatus(status(
                crate::activity_event::glyph::EXEC,
                &format!("Step {index}"),
            )));
        }

        assert_eq!(
            state.render_markdown_v2(),
            "• Step 2\n• Step 3\n• Step 4\n• Step 5\n• Step 6"
        );
    }

    #[test]
    fn live_draft_keeps_workflow_active_sticky_above_four_recent_statuses() {
        let mut state = TelegramLiveDraftState::working();
        state.apply(LiveProgressEvent::TelegramStatus(status(
            crate::activity_event::glyph::WORKFLOW,
            "Workflow Active: repo-analysis",
        )));
        for index in 1..=5 {
            state.apply(LiveProgressEvent::TelegramStatus(status(
                crate::activity_event::glyph::EXEC,
                &format!("Step {index}"),
            )));
        }

        assert_eq!(
            state.render_markdown_v2(),
            "⌘ Workflow Active: repo\\-analysis\n• Step 2\n• Step 3\n• Step 4\n• Step 5"
        );
    }

    #[test]
    fn live_draft_consumes_created_workflow_when_workflow_becomes_active() {
        let mut state = TelegramLiveDraftState::working();
        state.apply(LiveProgressEvent::TelegramStatus(status(
            crate::activity_event::glyph::WORKFLOW,
            "Workflow Created: repo-analysis",
        )));
        state.apply(LiveProgressEvent::TelegramStatus(status(
            crate::activity_event::glyph::PLAN,
            "Plan Updated",
        )));
        state.apply(LiveProgressEvent::TelegramStatus(status(
            crate::activity_event::glyph::WORKFLOW,
            "Workflow Active: repo-analysis",
        )));

        assert_eq!(
            state.render_markdown_v2(),
            "⌘ Workflow Active: repo\\-analysis\n∷ Plan Updated"
        );
    }

    #[test]
    fn live_draft_escapes_markdown_v2_dynamic_content() {
        assert_eq!(
            escape_markdown_v2("_*[]()~`>#+-=|{}.!\\"),
            "\\_\\*\\[\\]\\(\\)\\~\\`\\>\\#\\+\\-\\=\\|\\{\\}\\.\\!\\\\"
        );
    }

    #[test]
    fn dashboard_draft_upserts_assistant_and_thinking_cells() {
        let (tx, rx) = tokio::sync::watch::channel(crate::dashboard::DashboardState::default());
        let mut state = DashboardLiveDraftState::default();

        state.apply(
            &LiveProgressEvent::ReasoningContent {
                content: "considering options".to_string(),
            },
            Some(&tx),
        );
        state.apply(
            &LiveProgressEvent::AssistantContent {
                content: "Here is the answer.".to_string(),
            },
            Some(&tx),
        );
        state.flush(Some(&tx));

        let live = rx.borrow().live_activity_events.clone();
        assert_eq!(live.len(), 2);
        assert!(live.iter().any(|cell| {
            cell.key == DASHBOARD_THINKING_DRAFT_KEY
                && matches!(
                    &cell.event,
                    crate::dashboard::SessionActivityEvent::Thinking(thinking)
                        if thinking.content == "considering options"
                )
        }));
        assert!(live.iter().any(|cell| {
            cell.key == DASHBOARD_ASSISTANT_DRAFT_KEY
                && matches!(
                    &cell.event,
                    crate::dashboard::SessionActivityEvent::Assistant(assistant)
                        if assistant.content == "Here is the answer."
                )
        }));
    }

    #[test]
    fn dashboard_draft_reset_removes_cells_and_buffers() {
        let (tx, rx) = tokio::sync::watch::channel(crate::dashboard::DashboardState::default());
        let mut state = DashboardLiveDraftState::default();
        state.apply(
            &LiveProgressEvent::AssistantContent {
                content: "draft".to_string(),
            },
            Some(&tx),
        );
        assert_eq!(rx.borrow().live_activity_events.len(), 1);

        state.apply(&LiveProgressEvent::DraftReset, Some(&tx));
        assert!(rx.borrow().live_activity_events.is_empty());

        state.flush(Some(&tx));
        assert!(
            rx.borrow().live_activity_events.is_empty(),
            "a late flush must not resurrect a committed draft"
        );
    }

    #[test]
    fn dashboard_draft_throttles_refreshes_until_flush() {
        let (tx, rx) = tokio::sync::watch::channel(crate::dashboard::DashboardState::default());
        let mut state = DashboardLiveDraftState::default();
        state.apply(
            &LiveProgressEvent::AssistantContent {
                content: "first".to_string(),
            },
            Some(&tx),
        );
        state.apply(
            &LiveProgressEvent::AssistantContent {
                content: "first second".to_string(),
            },
            Some(&tx),
        );

        assert!(matches!(
            rx.borrow().live_activity_events.first(),
            Some(cell) if matches!(
                &cell.event,
                crate::dashboard::SessionActivityEvent::Assistant(assistant)
                    if assistant.content == "first"
            )
        ));

        state.flush(Some(&tx));
        assert!(matches!(
            rx.borrow().live_activity_events.first(),
            Some(cell) if matches!(
                &cell.event,
                crate::dashboard::SessionActivityEvent::Assistant(assistant)
                    if assistant.content == "first second"
            )
        ));
    }
}
