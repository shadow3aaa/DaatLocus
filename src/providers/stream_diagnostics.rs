//! Response metadata only: never retain response bodies, credentials or cookies.
use crate::model_protocol::AgentTurnStreamResult;
use miette::Result;
use serde_json::{Value, json};
use std::time::Instant;

pub(super) struct StreamDiagnostics {
    protocol: &'static str,
    model: String,
    actor: Option<String>,
    status: u16,
    content_type: Option<String>,
    request_id: Option<String>,
    upstream_id: Option<String>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    content_blocks: u64,
    received_bytes: u64,
    events: u64,
    invalid_json_events: u64,
    last_event: Option<String>,
    terminal_event: Option<String>,
    stop_reason: Option<String>,
    started: Instant,
    outcome: &'static str,
}

fn label(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(256)
        .collect()
}

impl StreamDiagnostics {
    pub(super) fn new(
        response: &reqwest::Response,
        protocol: &'static str,
        model: &str,
        actor: Option<&str>,
    ) -> Self {
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(label)
        };
        Self {
            protocol,
            model: model.into(),
            actor: actor.map(str::to_owned),
            status: response.status().as_u16(),
            content_type: header("content-type"),
            request_id: header("request-id")
                .or_else(|| header("x-request-id"))
                .or_else(|| header("cf-ray")),
            upstream_id: None,
            input_tokens: None,
            output_tokens: None,
            content_blocks: 0,
            received_bytes: 0,
            events: 0,
            invalid_json_events: 0,
            last_event: None,
            terminal_event: None,
            stop_reason: None,
            started: Instant::now(),
            outcome: "interrupted",
        }
    }

    pub(super) fn received(&mut self, bytes: usize) {
        self.received_bytes = self.received_bytes.saturating_add(bytes as u64);
    }

    pub(super) fn invalid_json(&mut self) {
        self.invalid_json_events += 1;
    }

    pub(super) fn done_marker(&mut self) {
        self.last_event = Some("[DONE]".into());
    }

    pub(super) fn event(&mut self, value: &Value) {
        self.events += 1;
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        self.last_event = Some(label(kind));
        if kind == "content_block_start" {
            self.content_blocks += 1;
        }
        if let Some(usage) = value
            .pointer("/message/usage")
            .or_else(|| value.pointer("/response/usage"))
            .or_else(|| value.get("usage"))
        {
            self.input_tokens = usage
                .get("input_tokens")
                .and_then(Value::as_i64)
                .or(self.input_tokens);
            self.output_tokens = usage
                .get("output_tokens")
                .and_then(Value::as_i64)
                .or(self.output_tokens);
        }
        if matches!(
            kind,
            "message_stop"
                | "response.completed"
                | "response.failed"
                | "response.incomplete"
                | "error"
        ) {
            self.terminal_event = Some(label(kind));
        }
        if let Some(id) = value
            .pointer("/message/id")
            .or_else(|| value.pointer("/response/id"))
            .and_then(Value::as_str)
        {
            self.upstream_id = Some(label(id));
        }
        if let Some(reason) = value
            .pointer("/delta/stop_reason")
            .or_else(|| value.pointer("/response/status"))
            .and_then(Value::as_str)
        {
            self.stop_reason = Some(label(reason));
        }
    }

    fn summary(&self) -> String {
        json!({
            "protocol": self.protocol, "model": self.model, "actor": self.actor,
            "http_status": self.status, "content_type": self.content_type,
            "request_id": self.request_id, "upstream_id": self.upstream_id,
            "input_tokens": self.input_tokens, "output_tokens": self.output_tokens,
            "content_blocks": self.content_blocks,
            "received_bytes": self.received_bytes, "events": self.events,
            "invalid_json_events": self.invalid_json_events,
            "last_event": self.last_event, "terminal_event": self.terminal_event,
            "stop_reason": self.stop_reason, "elapsed_ms": self.started.elapsed().as_millis(),
            "outcome": self.outcome
        })
        .to_string()
    }

    pub(super) fn finish(
        &mut self,
        result: Result<AgentTurnStreamResult>,
    ) -> Result<AgentTurnStreamResult> {
        let result = result.and_then(|response| {
            response.ensure_model_output()?;
            Ok(response)
        });
        self.outcome = if result.is_ok() { "ok" } else { "error" };
        result.map_err(|error| {
            let detail = error.to_string();
            error.wrap_err(format!(
                "{detail}\nModel stream diagnostics: {}",
                self.summary()
            ))
        })
    }
}

impl Drop for StreamDiagnostics {
    fn drop(&mut self) {
        tracing::info!(diagnostics = %self.summary(), "Model stream finished");
    }
}
