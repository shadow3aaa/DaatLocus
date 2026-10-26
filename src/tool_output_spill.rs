//! Bound model-visible tool output and store readable overflow in a temporary pool.
//! Actor execution supplies continuations backed by its immutable resource ledger.
//! Standalone temporary-pool writers and collection are separate helpers.
//! The pool must remain readable and protected against model writes by access policy.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::context_budget::APPROX_BYTES_PER_TOKEN;

/// How long a spill stays readable.
///
/// This must comfortably exceed the longest plausible single turn. A read that
/// lands after expiry fails as an ordinary file-not-found tool error, which the
/// model can recover from, so expiry is a cost, not a correctness break.
pub const SPILL_TTL: Duration = Duration::from_secs(60 * 60);

/// How often the service sweeps the pool.
///
/// Bounds worst-case overshoot at roughly a tenth of the TTL. This is the only
/// knob controlling how promptly expired spills are reclaimed, and a longer
/// period is strictly fine: nothing depends on exact expiry.
pub const SPILL_SWEEP_INTERVAL: Duration = Duration::from_secs(6 * 60);

const POOL_DIR_NAME: &str = "daat-locus";
const POOL_SUBDIR_NAME: &str = "tool-output";
const STAGING_SUFFIX: &str = ".partial";
const PLAIN_TRUNCATION_NOTICE: &str = "[tool output too long; model content truncated]";

/// Identity used to name one spill file.
#[derive(Clone, Copy, Debug)]
pub struct SpillContext<'a> {
    pub source_hint: &'a str,
    pub tool_name: &'a str,
    pub call_id: &'a str,
}

impl<'a> SpillContext<'a> {
    pub fn new(source_hint: &'a str, tool_name: &'a str, call_id: &'a str) -> Self {
        Self {
            source_hint,
            tool_name,
            call_id,
        }
    }
}

/// Root of the shared spill pool.
pub fn pool_dir() -> PathBuf {
    std::env::temp_dir()
        .join(POOL_DIR_NAME)
        .join(POOL_SUBDIR_NAME)
}

/// Create the pool if needed. Idempotent; the service calls it at boot and
/// every spill write calls it before writing.
pub fn ensure_pool_dir() -> std::io::Result<PathBuf> {
    let dir = pool_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Where the model should go when a tool result does not fit the budget.
#[derive(Clone, Debug)]
pub enum OverflowTarget<'a> {
    /// The full text cannot be re-derived from anywhere, so it is written to
    /// the shared spill pool and the marker names the file.
    Spill(SpillContext<'a>),
    /// The caller can read the source again, so the marker says how to
    /// continue instead.
    ///
    /// Copying content that already exists on disk into a temp file would only
    /// hand the model a worse copy of what it just asked for: the copy has no
    /// line anchors the model can edit against, and its line numbering does not
    /// match the real file unless the read happened to start at line one.
    Continue { instruction: String },
}

/// Applies the tool-output budget to already-rendered model content.
///
/// This is unconditional, including for content a tool supplied as an explicit
/// `model_content_override`. Call it where a tool result becomes an
/// `AgentMessage::tool`, after source-line elision.
///
/// When the content does not fit, the target decides what the model is told.
/// A tool that can be re-read supplies `Continue`; anything else spills to the
/// pool. With no target at all this degrades to a plain prefix truncation.
pub fn bound_tool_model_content(
    text: &str,
    max_tokens: usize,
    target: Option<OverflowTarget<'_>>,
) -> String {
    bound_tool_model_content_in(text, max_tokens, target, None)
}

fn bound_tool_model_content_in(
    text: &str,
    max_tokens: usize,
    target: Option<OverflowTarget<'_>>,
    dir: Option<&Path>,
) -> String {
    let max_chars = max_tokens.max(1).saturating_mul(APPROX_BYTES_PER_TOKEN);
    let prefix = take_chars_bounded(text, max_chars);
    if prefix.exhausted {
        return prefix.text;
    }

    let bounded = match target {
        None => append_plain_truncation(prefix.text, max_chars, prefix.total_chars),
        Some(OverflowTarget::Continue { instruction }) => {
            let marker = format!(
                "[tool output truncated: showing part of {} chars]\n{instruction}",
                prefix.total_chars
            );
            let kept = keep_head_from_prefix(&prefix, max_chars, &marker);
            format!("{kept}\n{marker}")
        }
        Some(OverflowTarget::Spill(ctx)) => match write_spill(text, &ctx, dir) {
            Some(written) => {
                let marker = spill_marker(&written.path, prefix.total_chars, written.line_count);
                let kept = keep_head_from_prefix(&prefix, max_chars, &marker);
                format!("{kept}\n{marker}")
            }
            None => append_plain_truncation(prefix.text, max_chars, prefix.total_chars),
        },
    };
    crate::context_budget::utf8_prefix(&bounded, max_chars).to_owned()
}

struct CharPrefix {
    text: String,
    total_chars: usize,
    /// True when `text` already contains every char of the source.
    exhausted: bool,
}

/// One forward scan. Stops copying once `limit` UTF-8 bytes are kept, but keeps
/// counting so callers still know the full length.
fn take_chars_bounded(text: &str, limit: usize) -> CharPrefix {
    let out = crate::context_budget::utf8_prefix(text, limit).to_owned();
    CharPrefix {
        total_chars: text.chars().count(),
        exhausted: out.len() == text.len(),
        text: out,
    }
}

/// Keep as much of the already-scanned head as fits once the marker has been
/// accounted for, so the result stays inside the budget instead of overshooting it.
fn keep_head_from_prefix(prefix: &CharPrefix, max_chars: usize, marker: &str) -> String {
    let head_bytes = max_chars.saturating_sub(marker.len() + 1);
    crate::context_budget::utf8_prefix(&prefix.text, head_bytes).to_owned()
}

fn append_plain_truncation(kept: String, max_chars: usize, total_chars: usize) -> String {
    let marker = format!(
        "{PLAIN_TRUNCATION_NOTICE} ({} chars omitted)",
        total_chars.saturating_sub(kept.chars().count())
    );
    let head =
        crate::context_budget::utf8_prefix(&kept, max_chars.saturating_sub(marker.len() + 1));
    let marker = format!(
        "{PLAIN_TRUNCATION_NOTICE} ({} chars omitted)",
        total_chars.saturating_sub(head.chars().count())
    );
    crate::context_budget::utf8_prefix(&format!("{head}\n{marker}"), max_chars).to_owned()
}

struct WrittenSpill {
    path: PathBuf,
    line_count: usize,
}

fn write_spill(text: &str, ctx: &SpillContext<'_>, dir: Option<&Path>) -> Option<WrittenSpill> {
    let owned_dir;
    let dir = match dir {
        Some(dir) => dir,
        None => {
            owned_dir = ensure_pool_dir().ok()?;
            &owned_dir
        }
    };
    let created = SystemTime::now();
    let path = dir.join(spill_file_name(created, ctx));
    // Stage then rename so a reader never observes a half-written file at the
    // final path. An abandoned staging file has a fresh mtime and is collected
    // by the normal age sweep like any other pool entry.
    let staging = dir.join(format!(
        "{}{STAGING_SUFFIX}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("spill")
    ));
    std::fs::write(&staging, text.as_bytes()).ok()?;
    if std::fs::rename(&staging, &path).is_err() {
        let _ = std::fs::remove_file(&staging);
        return None;
    }
    set_creation_times(&path, created);
    Some(WrittenSpill {
        line_count: text.lines().count(),
        path,
    })
}

fn set_creation_times(path: &Path, created: SystemTime) {
    let Ok(file) = std::fs::File::options().write(true).open(path) else {
        return;
    };
    let times = std::fs::FileTimes::new()
        .set_accessed(created)
        .set_modified(created);
    let _ = file.set_times(times);
}

fn spill_marker(path: &Path, total_chars: usize, line_count: usize) -> String {
    let path = path.display();
    format!(
        "[tool output truncated: showing part of {total_chars} chars]\n\
         Full output ({line_count} lines) is at:\n\
         {path}\n\
         Temporary file; retention: {} minutes.",
        SPILL_TTL.as_secs() / 60
    )
}

fn spill_file_name(created: SystemTime, ctx: &SpillContext<'_>) -> String {
    let millis = created
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    format!(
        "{millis}-{}-{}-{}.txt",
        sanitize_component(ctx.source_hint, 8),
        sanitize_component(ctx.tool_name, 40),
        sanitize_component(ctx.call_id, 12),
    )
}

/// Reduce an arbitrary identifier to characters that are safe in a file name on
/// every supported platform, including Windows.
fn sanitize_component(value: &str, max_len: usize) -> String {
    let cleaned = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "x".to_string()
    } else {
        trimmed.chars().take(max_len).collect()
    }
}

/// Counts from one age-based collection pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub removed: usize,
    /// Expired files that could not be removed right now, typically because a
    /// writer still holds one open. The next sweep retries them.
    pub deferred: usize,
    pub retained: usize,
}

/// Removes every pool file older than [`SPILL_TTL`].
pub fn sweep_expired(now: SystemTime) -> SweepReport {
    let Ok(dir) = ensure_pool_dir() else {
        return SweepReport::default();
    };
    sweep_dir(&dir, now)
}

/// The collection pass itself, over an explicit directory.
fn sweep_dir(dir: &Path, now: SystemTime) -> SweepReport {
    let mut report = SweepReport::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return report;
    };

    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        // A file stamped in the future is left alone rather than deleted.
        let Ok(age) = now.duration_since(modified) else {
            report.retained += 1;
            continue;
        };
        if age <= SPILL_TTL {
            report.retained += 1;
            continue;
        }
        // The service is the only deleter, so a failure here is never a race
        // with another collector. The file is due either way, so a failure is
        // counted and retried by the next pass rather than surfaced.
        match std::fs::remove_file(entry.path()) {
            Ok(()) => report.removed += 1,
            Err(_) => report.deferred += 1,
        }
    }

    report
}

/// service-side collection: one pass at boot, then one per interval.
///
/// There is deliberately no filesystem watcher here. The sweep is authoritative
/// and the watcher could only ever shave the sweep interval off a file's
/// expiry, which nothing depends on; a notifier would add a long-lived handle
/// and Windows-specific failure modes in exchange for a delay that is already
/// bounded and harmless.
pub async fn run_gc_loop() {
    sweep_once();
    let mut ticker = tokio::time::interval(SPILL_SWEEP_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        sweep_once();
    }
}

fn sweep_once() {
    let report = sweep_expired(SystemTime::now());
    if report.removed > 0 || report.deferred > 0 {
        tracing::debug!(
            removed = report.removed,
            deferred = report.deferred,
            retained = report.retained,
            "collected expired tool output spills"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multilingual_tool_observations_are_bounded_in_bytes() {
        let text = "中文🙂".repeat(1000);
        let bounded = bound_tool_model_content(&text, 100, None);
        assert!(crate::context_budget::approx_token_count(&bounded) <= 100);
        assert!(bounded.contains(PLAIN_TRUNCATION_NOTICE));
    }

    #[test]
    fn sanitize_component_is_platform_safe() {
        assert_eq!(sanitize_component("web__run", 40), "web__run");
        assert_eq!(sanitize_component("call:1/ab", 12), "call-1-ab");
        assert_eq!(sanitize_component("", 8), "x");
        assert_eq!(sanitize_component("...", 8), "x");
        assert_eq!(sanitize_component("abcdefghijklmnop", 8), "abcdefgh");
    }

    #[test]
    fn content_within_budget_is_untouched() {
        let text = "short output";
        assert_eq!(
            bound_tool_model_content(text, 2_000, None),
            text,
            "content inside the budget must not be rewritten"
        );
    }

    #[test]
    fn oversized_content_without_spill_degrades_to_prefix_truncation() {
        let text = "a".repeat(50_000);
        let bounded = bound_tool_model_content(&text, 100, None);

        assert!(bounded.contains(PLAIN_TRUNCATION_NOTICE));
        assert!(bounded.starts_with('a'));
        assert!(
            bounded.len() <= 400,
            "100-token output must fit its byte budget, including the notice"
        );
    }

    #[test]
    fn sweep_collects_only_expired_files() {
        let temp = tempfile::tempdir().expect("temp dir");
        let pool = temp.path().join("pool");
        std::fs::create_dir_all(&pool).expect("create pool");

        let now = SystemTime::now();
        let fresh = write_stamped(&pool, "fresh.txt", now);
        let expired = write_stamped(
            &pool,
            "expired.txt",
            now - SPILL_TTL - Duration::from_secs(60),
        );
        let future = write_stamped(&pool, "future.txt", now + Duration::from_secs(3_600));
        let staging = write_stamped(
            &pool,
            &format!("abandoned.txt{STAGING_SUFFIX}"),
            now - SPILL_TTL - Duration::from_secs(60),
        );

        let report = sweep_dir(&pool, now);

        assert_eq!(report.removed, 2, "expired spill and staging file both go");
        assert_eq!(report.retained, 2);
        assert!(!expired.exists());
        assert!(!staging.exists());
        assert!(fresh.exists(), "fresh spill is retained");
        assert!(
            future.exists(),
            "a file stamped in the future is never collected"
        );
    }

    #[test]
    fn sweep_on_a_missing_pool_is_a_no_op() {
        let temp = tempfile::tempdir().expect("temp dir");
        let report = sweep_dir(&temp.path().join("absent"), SystemTime::now());

        assert_eq!(report, SweepReport::default());
    }

    #[test]
    fn continue_target_points_back_at_the_source_instead_of_copying_it() {
        let temp = tempfile::tempdir().expect("temp dir");
        let pool = temp.path().join("pool");
        std::fs::create_dir_all(&pool).expect("create pool");

        let body = "42#ab|some source line\n".repeat(4_000);
        let bounded = bound_tool_model_content_in(
            &body,
            200,
            Some(OverflowTarget::Continue {
                instruction: "Source file: src/lib.rs. Start line: 2001.".to_string(),
            }),
            Some(&pool),
        );

        assert!(
            bounded.contains("src/lib.rs") && bounded.contains("2001"),
            "marker must retain the source locator"
        );
        assert!(
            !bounded.contains("tool-output"),
            "a re-readable result must never be copied into the spill pool: {bounded}"
        );
        assert_eq!(
            std::fs::read_dir(&pool).expect("read pool").count(),
            0,
            "no spill file may be written for a redirectable result"
        );
    }

    fn write_stamped(dir: &Path, name: &str, stamp: SystemTime) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, b"payload").expect("write pool file");
        set_creation_times(&path, stamp);
        path
    }

    #[test]
    fn spill_round_trip_lets_the_model_read_back_the_whole_output() {
        let temp = tempfile::tempdir().expect("temp dir");
        let pool = temp.path().join("pool");
        std::fs::create_dir_all(&pool).expect("create pool");

        let body = (0..4_000)
            .map(|index| format!("line-{index:06}"))
            .collect::<Vec<_>>()
            .join("\n");
        let bounded = bound_tool_model_content_in(
            &body,
            200,
            Some(OverflowTarget::Spill(SpillContext::new(
                "sess-1234",
                "terminal__terminal_exec",
                "call-abcdef",
            ))),
            Some(&pool),
        );

        // The marker names a file that really exists and really holds the
        // untruncated text, so the model can page back into what it lost.
        let spilled = pool
            .read_dir()
            .expect("read pool")
            .flatten()
            .map(|entry| entry.path())
            .find(|path| path.extension().is_some_and(|ext| ext == "txt"))
            .expect("a spill file was written");
        assert!(
            bounded.contains(&spilled.display().to_string()),
            "marker must name the spill path so the model can re-read it"
        );
        assert_eq!(
            std::fs::read_to_string(&spilled).expect("read spill"),
            body,
            "spill must hold the full output, not the truncated head"
        );

        // And the spilled text stays line-addressable for read_file paging.
        let spilled_lines = std::fs::read_to_string(&spilled).expect("read spill");
        assert_eq!(spilled_lines.lines().count(), 4_000);
        assert_eq!(spilled_lines.lines().next(), Some("line-000000"));
    }

    #[test]
    fn spill_is_left_readable_but_not_model_writable() {
        let temp = tempfile::tempdir().expect("temp dir");
        let pool = temp.path().join("pool");
        std::fs::create_dir_all(&pool).expect("create pool");

        let mut policy = crate::sandbox::RuntimeSandboxPolicy::disabled();
        policy.filesystem.deny_write_paths.push(pool.clone());

        let inside = pool.join("spill.txt");
        assert!(
            policy.is_path_readable(&inside),
            "the model must be able to read_file its own spilled output"
        );
        assert!(
            !policy.is_path_writable(&inside),
            "the model must not be able to overwrite a spill a pending result points at"
        );
    }
}
