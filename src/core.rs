use std::{borrow::Cow, time::Duration};

use async_trait::async_trait;
use chrono::Local;
use daat_locus_macros::model_schema;
use miette::{Result, miette};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

use crate::{
    context_budget::{
        ContextBudgetExceededError, RequestBudgetBreakdown, RequestBudgetLimits,
        estimate_agent_turn_request, estimate_prompt_request,
    },
    events::EventDisposition,
    live_progress::LiveProgressEvent,
    plan::PlanStatus,
    reasoning::runtime::{AgentTurnRequest, AgentTurnStreamResult, PromptRequest},
};

const MAX_DAILY_TOKEN_USAGE_DAYS: usize = 30;

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TerminalExecArgs {
    pub command: String,
    /// Existing session id to reuse; null or empty creates a new session. Never invent a session id.
    pub session_id: Option<String>,
    pub workdir: Option<String>,
    pub yield_time_ms: Option<u64>,
    pub max_chars: Option<usize>,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TerminalWriteStdinArgs {
    pub session_id: String,
    pub text: String,
    /// Defaults to `any_output`. Use `timeout` for a pure wait that suppresses intermediate progress updates.
    pub wait_mode: Option<TerminalWaitMode>,
    pub yield_time_ms: Option<u64>,
    pub max_chars: Option<usize>,
}

#[model_schema]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalWaitMode {
    /// Return after new output arrives, the process exits, or the yield window expires.
    AnyOutput,
    /// Wait until the yield window expires or the process exits; do not stream intermediate output updates.
    Timeout,
}

impl JsonSchema for TerminalWaitMode {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        "TerminalWaitMode".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "enum": ["any_output", "timeout"],
        })
    }
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TerminalTerminateArgs {
    pub session_id: String,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct BrowserOpenArgs {
    pub url: String,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct BrowserSnapshotArgs {
    pub page_id: String,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct BrowserWaitArgs {
    pub page_id: String,
    /// `dom` waits for any parsed DOM, `load` waits for complete readyState.
    pub state: Option<String>,
    pub timeout_ms: Option<u64>,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct BrowserClickArgs {
    pub page_id: String,
    pub element_ref: String,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct BrowserFillArgs {
    pub page_id: String,
    pub element_ref: String,
    pub value: String,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct BrowserBackArgs {
    pub page_id: String,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct BrowserForwardArgs {
    pub page_id: String,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct BrowserReloadArgs {
    pub page_id: String,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct BrowserClosePageArgs {
    pub page_id: String,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct EventResolveArgs {
    pub disposition: EventDisposition,
    pub reply_message: Option<String>,
    pub note: Option<String>,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct UpdatePlanStepArgs {
    pub step: String,
    pub status: PlanStatus,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct UpdatePlanArgs {
    pub explanation: Option<String>,
    pub plan: Vec<UpdatePlanStepArgs>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct TokenUsage {
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_output_tokens: i64,
    pub total_tokens: i64,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct DailyTokenUsage {
    pub date: String,
    pub usage: TokenUsage,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct TokenUsageInfo {
    pub total_token_usage: TokenUsage,
    pub last_token_usage: TokenUsage,
    pub model_context_window: Option<i64>,
    #[serde(default)]
    pub daily_token_usage: Vec<DailyTokenUsage>,
}

impl TokenUsage {
    pub const fn is_zero(&self) -> bool {
        self.total_tokens == 0
            && self.input_tokens == 0
            && self.cached_input_tokens == 0
            && self.output_tokens == 0
            && self.reasoning_output_tokens == 0
    }

    pub const fn add_assign(&mut self, other: &Self) {
        self.input_tokens += other.input_tokens;
        self.cached_input_tokens += other.cached_input_tokens;
        self.output_tokens += other.output_tokens;
        self.reasoning_output_tokens += other.reasoning_output_tokens;
        self.total_tokens += other.total_tokens;
    }
}

impl TokenUsageInfo {
    pub fn append_last_usage(&mut self, last: TokenUsage) {
        self.total_token_usage.add_assign(&last);
        self.append_daily_usage(&last);
        self.last_token_usage = last;
    }

    pub fn merged_with_process_usage(&self, process: &Self) -> Self {
        let mut merged = self.clone();
        merged
            .total_token_usage
            .add_assign(&process.total_token_usage);
        if let Some(window) = process.model_context_window {
            merged.model_context_window = Some(window);
        }
        if !process.last_token_usage.is_zero() {
            merged.last_token_usage = process.last_token_usage.clone();
        }
        for day in &process.daily_token_usage {
            merged.append_daily_usage_for_date(&day.date, &day.usage);
        }
        merged
    }

    fn append_daily_usage(&mut self, usage: &TokenUsage) {
        let date = Local::now().date_naive().to_string();
        self.append_daily_usage_for_date(&date, usage);
    }

    fn append_daily_usage_for_date(&mut self, date: &str, usage: &TokenUsage) {
        if usage.is_zero() {
            return;
        }

        if let Some(day) = self
            .daily_token_usage
            .iter_mut()
            .find(|day| day.date == date)
        {
            day.usage.add_assign(usage);
        } else {
            self.daily_token_usage.push(DailyTokenUsage {
                date: date.to_string(),
                usage: usage.clone(),
            });
        }

        self.trim_daily_usage();
    }

    fn trim_daily_usage(&mut self) {
        if self.daily_token_usage.len() > MAX_DAILY_TOKEN_USAGE_DAYS {
            let excess = self.daily_token_usage.len() - MAX_DAILY_TOKEN_USAGE_DAYS;
            self.daily_token_usage.drain(0..excess);
        }
    }
}
/// An explicit, runtime-owned sink for streaming model progress.
///
/// Model providers may report model output through this sink, but they never
/// receive the session's mutable runtime state.
#[derive(Clone)]
pub struct ModelProgressSink {
    sender: tokio::sync::mpsc::UnboundedSender<LiveProgressEvent>,
}

impl ModelProgressSink {
    pub const fn new(sender: tokio::sync::mpsc::UnboundedSender<LiveProgressEvent>) -> Self {
        Self { sender }
    }

    pub fn emit_assistant_content(&self, content: impl Into<String>) {
        let _ = self.sender.send(LiveProgressEvent::AssistantContent {
            content: content.into(),
        });
    }

    pub fn emit_reasoning_content(&self, content: impl Into<String>) {
        let _ = self.sender.send(LiveProgressEvent::ReasoningContent {
            content: content.into(),
        });
    }
}

/// Explicit metadata and runtime policy for one provider call.
///
/// This contains only values a provider is allowed to observe. It deliberately
/// excludes [`crate::context::Context`], which remains owned by the runtime.
#[derive(Clone)]
pub struct ModelRequestOptions {
    pub conversation_id: Option<String>,
    pub progress: Option<ModelProgressSink>,
    pub budget: RequestBudgetBreakdown,
}

impl ModelRequestOptions {
    pub fn for_prompt(
        provider: &(dyn ModelProvider + Send + Sync),
        request: &PromptRequest,
        conversation_id: Option<String>,
    ) -> Result<Self> {
        let budget = estimate_prompt_request(request, provider.request_budget_limits());
        Self::from_budget("prompt request", provider, budget, conversation_id)
    }

    pub fn for_agent_turn(
        provider: &(dyn ModelProvider + Send + Sync),
        request: &AgentTurnRequest,
        conversation_id: Option<String>,
    ) -> Result<Self> {
        let budget = estimate_agent_turn_request(
            &request.messages,
            &request.tools,
            provider.request_budget_limits(),
        );
        Self::from_budget("agent turn", provider, budget, conversation_id)
    }

    fn from_budget(
        request_kind: &str,
        provider: &(dyn ModelProvider + Send + Sync),
        budget: RequestBudgetBreakdown,
        conversation_id: Option<String>,
    ) -> Result<Self> {
        if !budget.within_context_window() {
            return Err(ContextBudgetExceededError::for_request(
                request_kind,
                &provider.model_name(),
                &budget,
                None,
            )
            .into());
        }
        Ok(Self {
            conversation_id,
            progress: None,
            budget,
        })
    }

    pub fn with_progress(mut self, progress: Option<ModelProgressSink>) -> Self {
        self.progress = progress;
        self
    }
}

/// Model protocol provider abstraction.
///
/// Providers translate explicit model requests to an upstream protocol. They
/// do not receive runtime state, own request budgets, or construct call
/// metadata; the runtime supplies fully-validated [`ModelRequestOptions`].
#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// Complete a structured request and return the raw JSON argument object.
    async fn complete_json(
        &self,
        request: PromptRequest,
        options: ModelRequestOptions,
    ) -> Result<serde_json::Value>;

    /// Complete one tool-driven agent turn.
    async fn complete_agent_turn(
        &self,
        request: AgentTurnRequest,
        options: ModelRequestOptions,
    ) -> Result<AgentTurnStreamResult>;

    /// Static request budget limits advertised by this model configuration.
    fn request_budget_limits(&self) -> RequestBudgetLimits;

    /// Current process-local token accounting for this provider.
    fn token_usage_info(&self) -> TokenUsageInfo;

    /// The model identifier sent to the upstream provider.
    fn model_name(&self) -> String;
}

/// Shared retry policy for all tool-driven agent-turn model requests.
#[derive(Clone, Copy, Debug)]
pub struct AgentTurnRetryPolicy {
    pub max_attempts: usize,
    pub base_request_timeout: Duration,
    pub max_request_timeout: Duration,
    pub base_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for AgentTurnRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 6,
            base_request_timeout: Duration::from_mins(5),
            max_request_timeout: Duration::from_mins(20),
            base_backoff: Duration::from_millis(300),
            max_backoff: Duration::from_secs(30),
        }
    }
}

impl AgentTurnRetryPolicy {
    fn request_timeout_for_attempt(self, attempt: usize) -> Duration {
        exponential_retry_duration(self.base_request_timeout, self.max_request_timeout, attempt)
    }

    fn backoff_for_attempt(self, attempt: usize) -> Duration {
        exponential_retry_duration(self.base_backoff, self.max_backoff, attempt)
    }
}

fn exponential_retry_duration(base: Duration, maximum: Duration, attempt: usize) -> Duration {
    let shift = u32::try_from((attempt.saturating_sub(1)).min(6))
        .expect("retry backoff shift is capped at six");
    base.checked_mul(1u32 << shift)
        .unwrap_or(maximum)
        .min(maximum)
}

pub struct AgentTurnRetrySuccess {
    pub response: AgentTurnStreamResult,
    pub attempts: usize,
}

pub struct AgentTurnRetryFailure {
    pub error: miette::Report,
}

pub type AgentTurnAttemptStartedObserver<'a> = dyn Fn(usize) + Send + Sync + 'a;
pub type AgentTurnAttemptFailedObserver<'a> =
    dyn Fn(&str, usize, Option<Duration>) + Send + Sync + 'a;

#[derive(Clone, Copy, Default)]
pub struct AgentTurnRetryObserver<'a> {
    pub on_attempt_started: Option<&'a AgentTurnAttemptStartedObserver<'a>>,
    pub on_attempt_failed: Option<&'a AgentTurnAttemptFailedObserver<'a>>,
}

pub async fn complete_agent_turn_with_retry_with_observer(
    provider: &(dyn ModelProvider + Send + Sync),
    request: AgentTurnRequest,
    options: ModelRequestOptions,
    observer: AgentTurnRetryObserver<'_>,
) -> std::result::Result<AgentTurnRetrySuccess, AgentTurnRetryFailure> {
    complete_agent_turn_with_retry_using_policy_cancellable_with_observer(
        provider,
        request,
        options,
        AgentTurnRetryPolicy::default(),
        || false,
        observer,
    )
    .await
}

pub async fn complete_agent_turn_with_retry_using_policy_cancellable_with_observer<F>(
    provider: &(dyn ModelProvider + Send + Sync),
    request: AgentTurnRequest,
    options: ModelRequestOptions,
    policy: AgentTurnRetryPolicy,
    is_cancelled: F,
    observer: AgentTurnRetryObserver<'_>,
) -> std::result::Result<AgentTurnRetrySuccess, AgentTurnRetryFailure>
where
    F: Fn() -> bool + Sync,
{
    let max_attempts = policy.max_attempts.max(1);
    let model_name = provider.model_name();
    let estimated_input_tokens = options.budget.total_input_tokens;
    let mut attempt = 1usize;
    loop {
        if is_cancelled() {
            return Err(AgentTurnRetryFailure {
                error: agent_turn_retry_cancelled_error(),
            });
        }

        let request_timeout = policy.request_timeout_for_attempt(attempt);
        if let Some(on_attempt_started) = observer.on_attempt_started {
            on_attempt_started(attempt);
        }
        let Some(turn_result) = complete_agent_turn_attempt_cancellable(
            provider,
            request.clone(),
            options.clone(),
            request_timeout,
            &is_cancelled,
        )
        .await
        else {
            return Err(AgentTurnRetryFailure {
                error: agent_turn_retry_cancelled_error(),
            });
        };

        match turn_result {
            Ok(response) => {
                return Ok(AgentTurnRetrySuccess {
                    response,
                    attempts: attempt,
                });
            }
            Err(error) => {
                let retry_backoff = (should_retry_agent_turn_error(&error)
                    && attempt < max_attempts)
                    .then(|| policy.backoff_for_attempt(attempt));
                let error_detail = model_request_error_detail(&error);
                if let Some(on_attempt_failed) = observer.on_attempt_failed {
                    on_attempt_failed(&error_detail, attempt, retry_backoff);
                }
                let Some(backoff) = retry_backoff else {
                    return Err(AgentTurnRetryFailure { error });
                };

                tracing::warn!(
                    "complete_agent_turn retry #{attempt} after {}ms (model={}, messages={}, tools={}, estimated_input_tokens={}): {}",
                    backoff.as_millis(),
                    model_name,
                    request.messages.len(),
                    request.tools.len(),
                    estimated_input_tokens,
                    error_detail
                );
                if wait_for_agent_turn_retry_backoff(backoff, &is_cancelled).await {
                    return Err(AgentTurnRetryFailure {
                        error: agent_turn_retry_cancelled_error(),
                    });
                }
                attempt += 1;
            }
        }
    }
}

async fn complete_agent_turn_attempt_cancellable<F>(
    provider: &(dyn ModelProvider + Send + Sync),
    request: AgentTurnRequest,
    options: ModelRequestOptions,
    request_timeout: Duration,
    is_cancelled: &F,
) -> Option<Result<AgentTurnStreamResult>>
where
    F: Fn() -> bool + Sync,
{
    const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(25);

    let request = tokio::time::timeout(
        request_timeout,
        provider.complete_agent_turn(request, options),
    );
    tokio::pin!(request);
    loop {
        tokio::select! {
            result = &mut request => {
                return Some(match result {
                    Ok(result) => result,
                    Err(_elapsed) => Err(miette!(
                        "model request timed out after {}s",
                        request_timeout.as_secs()
                    )),
                });
            }
            () = tokio::time::sleep(CANCELLATION_POLL_INTERVAL) => {
                if is_cancelled() {
                    return None;
                }
            }
        }
    }
}

async fn wait_for_agent_turn_retry_backoff<F>(backoff: Duration, is_cancelled: &F) -> bool
where
    F: Fn() -> bool + Sync,
{
    const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(25);

    let deadline = tokio::time::Instant::now() + backoff;
    while tokio::time::Instant::now() < deadline {
        if is_cancelled() {
            return true;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        tokio::time::sleep(remaining.min(CANCELLATION_POLL_INTERVAL)).await;
    }
    is_cancelled()
}

fn agent_turn_retry_cancelled_error() -> miette::Report {
    miette!("agent turn retry cancelled")
}

pub fn model_request_error_detail(err: &miette::Report) -> String {
    let mut lines = vec![err.to_string()];
    let mut causes = Vec::new();
    let mut current = err.source();
    while let Some(source) = current {
        let cause = source.to_string();
        if !cause.trim().is_empty() {
            causes.push(cause);
        }
        current = source.source();
    }
    if !causes.is_empty() {
        lines.push("causes:".to_string());
        lines.extend(causes.into_iter().map(|cause| format!("- {cause}")));
    }
    lines.join("\n")
}

pub fn should_retry_agent_turn_error(error: &miette::Report) -> bool {
    !is_context_budget_error(error) && !is_permanent_model_request_error(&error.to_string())
}

fn is_context_budget_error(error: &miette::Report) -> bool {
    error.downcast_ref::<ContextBudgetExceededError>().is_some()
}

pub fn is_permanent_model_request_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("http 400 bad request")
        || lower.contains("invalid_request_error")
        || lower.contains("invalid_value")
}
#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct RetryTestProvider {
        outcomes: std::sync::Mutex<Vec<Result<AgentTurnStreamResult>>>,
        calls: AtomicUsize,
    }

    impl RetryTestProvider {
        fn new(outcomes: Vec<Result<AgentTurnStreamResult>>) -> Self {
            Self {
                outcomes: std::sync::Mutex::new(outcomes),
                calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl ModelProvider for RetryTestProvider {
        async fn complete_json(
            &self,
            _request: PromptRequest,
            _options: ModelRequestOptions,
        ) -> Result<serde_json::Value> {
            Err(miette!("unused retry test prompt request"))
        }

        async fn complete_agent_turn(
            &self,
            _request: AgentTurnRequest,
            _options: ModelRequestOptions,
        ) -> Result<AgentTurnStreamResult> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.outcomes
                .lock()
                .expect("retry test outcomes lock")
                .remove(0)
        }

        fn request_budget_limits(&self) -> RequestBudgetLimits {
            RequestBudgetLimits {
                context_window_tokens: 128_000,
                auto_compact_threshold_tokens: 120_000,
                reserved_output_tokens: 4_000,
            }
        }

        fn token_usage_info(&self) -> TokenUsageInfo {
            TokenUsageInfo::default()
        }

        fn model_name(&self) -> String {
            "retry-test-provider".to_string()
        }
    }

    fn retry_test_request() -> AgentTurnRequest {
        AgentTurnRequest {
            messages: Vec::new(),
            tools: Vec::new(),
        }
    }

    fn retry_test_response() -> AgentTurnStreamResult {
        AgentTurnStreamResult {
            items: Vec::new(),
            raw_stream_follow_up: false,
            last_assistant_message: None,
            last_reasoning_content: None,
        }
    }

    fn retry_test_policy() -> AgentTurnRetryPolicy {
        AgentTurnRetryPolicy {
            max_attempts: 3,
            base_request_timeout: Duration::from_secs(1),
            max_request_timeout: Duration::from_secs(1),
            base_backoff: Duration::ZERO,
            max_backoff: Duration::ZERO,
        }
    }

    #[tokio::test]
    async fn retries_transient_agent_turn_failures() {
        let provider = RetryTestProvider::new(vec![
            Err(miette!("connection reset")),
            Ok(retry_test_response()),
        ]);

        let result = match complete_agent_turn_with_retry_using_policy_cancellable_with_observer(
            &provider,
            retry_test_request(),
            ModelRequestOptions::for_agent_turn(&provider, &retry_test_request(), None)
                .expect("retry test request options"),
            retry_test_policy(),
            || false,
            AgentTurnRetryObserver::default(),
        )
        .await
        {
            Ok(success) => success,
            Err(failure) => panic!("transient request should retry: {}", failure.error),
        };

        assert_eq!(result.attempts, 2);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    }
    #[tokio::test]
    async fn does_not_retry_permanent_or_context_budget_agent_turn_failures() {
        let permanent_error =
            miette!("model provider returned HTTP 400 Bad Request: invalid_request_error");
        assert_non_retryable_agent_turn_failure(permanent_error).await;

        let provider = RetryTestProvider::new(vec![Err(miette!("unused provider error"))]);
        let request = retry_test_request();
        let budget_error = ContextBudgetExceededError::for_request(
            "agent turn",
            "retry-test-provider",
            &ModelRequestOptions::for_agent_turn(&provider, &request, None)
                .expect("retry test request options")
                .budget,
            None,
        )
        .into();
        assert_non_retryable_agent_turn_failure(budget_error).await;
    }

    async fn assert_non_retryable_agent_turn_failure(error: miette::Report) {
        let provider = RetryTestProvider::new(vec![Err(error)]);
        let request = retry_test_request();
        let result = complete_agent_turn_with_retry_using_policy_cancellable_with_observer(
            &provider,
            request.clone(),
            ModelRequestOptions::for_agent_turn(&provider, &request, None)
                .expect("retry test request options"),
            retry_test_policy(),
            || false,
            AgentTurnRetryObserver::default(),
        )
        .await;
        let Err(_) = result else {
            panic!("permanent and context-budget errors must not retry");
        };

        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cancellation_stops_retry_backoff() {
        let provider = RetryTestProvider::new(vec![Err(miette!("connection reset"))]);
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancelled_for_observer = cancelled.clone();
        let on_attempt_failed = move |_: &str, _: usize, _: Option<Duration>| {
            cancelled_for_observer.store(true, Ordering::SeqCst);
        };
        let request = retry_test_request();
        let result = complete_agent_turn_with_retry_using_policy_cancellable_with_observer(
            &provider,
            request.clone(),
            ModelRequestOptions::for_agent_turn(&provider, &request, None)
                .expect("retry test request options"),
            AgentTurnRetryPolicy {
                base_backoff: Duration::from_mins(1),
                max_backoff: Duration::from_mins(1),
                ..retry_test_policy()
            },
            || cancelled.load(Ordering::SeqCst),
            AgentTurnRetryObserver {
                on_attempt_started: None,
                on_attempt_failed: Some(&on_attempt_failed),
            },
        )
        .await;
        let Err(_) = result else {
            panic!("cancellation should stop the retry backoff");
        };

        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }
}
