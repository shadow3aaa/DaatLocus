//! Bounded tool output with a re-readable spill file.
//!
//! A tool result that exceeds its context budget is cut down for the model and
//! the full text is written to a shared temporary pool. The truncation marker
//! tells the model where the remainder went so it can pull back just the range
//! it actually needs instead of guessing.
//!
//! Ownership split:
//!
//! - Session processes write spills and never collect them.
//! - The Manager daemon owns age-based collection of the pool.
//!
//! Sessions are runtime workers with no global view, and the pool is shared by
//! every session, so letting each writer also collect would make every session
//! responsible for files it knows nothing about. The Manager already owns
//! global housekeeping (config recovery, registry pruning) and outlives all
//! sessions.
//!
//! The pool lives under the OS temp directory rather than `DAAT_LOCUS_HOME`
//! because the Daat home is a sandbox deny-listed root that protects API keys,
//! the daemon token, and session state. A spill written there could not be
//! `read_file` at all. The OS temp dir is already readable, and the pool is
//! added to `deny_write_paths` so the model can read back its own overflow but
//! cannot overwrite a spill that another pending tool result still points at.
//!
//! Collection is a stat sweep, not per-file timers. The deadline lives in each
//! file's mtime rather than in memory, so it survives a Manager restart, and a
//! file that is due but momentarily undeletable (a Session still holding it
//! open on Windows) is simply retried by the next sweep.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::context_budget::APPROX_BYTES_PER_TOKEN;

/// How long a spill stays readable.
///
/// This must comfortably exceed the longest plausible single turn. A read that
/// lands after expiry fails as an ordinary file-not-found tool error, which the
/// model can recover from, so expiry is a cost, not a correctness break.
pub const SPILL_TTL: Duration = Duration::from_secs(60 * 60);

/// How often the Manager sweeps the pool.
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
    pub session_hint: &'a str,
    pub tool_name: &'a str,
    pub call_id: &'a str,
}

impl<'a> SpillContext<'a> {
    pub fn new(session_hint: &'a str, tool_name: &'a str, call_id: &'a str) -> Self {
        Self {
            session_hint,
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

/// Create the pool if needed. Idempotent; the Manager calls it at boot and
/// every spill write calls it before writing.
pub fn ensure_pool_dir() -> std::io::Result<PathBuf> {
    let dir = pool_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Applies the tool-output budget to already-rendered model content.
///
/// This is unconditional, including for content a tool supplied as an explicit
/// `model_content_override`. Call it where a tool result becomes an
/// `AgentMessage::tool`, after source-line elision.
///
/// When the content does not fit and a spill context is available, the full
/// text is written to the pool and the returned marker points at it. Without a
/// spill context, or when the write fails, this degrades to a plain prefix
/// truncation.
pub fn bound_tool_model_content(
    text: &str,
    max_tokens: usize,
    spill: Option<SpillContext<'_>>,
) -> String {
    bound_tool_model_content_in(text, max_tokens, spill, None)
}

fn bound_tool_model_content_in(
    text: &str,
    max_tokens: usize,
    spill: Option<SpillContext<'_>>,
    dir: Option<&Path>,
) -> String {
    let max_chars = max_tokens.max(1).saturating_mul(APPROX_BYTES_PER_TOKEN);
    let total_chars = text.chars().count();
    if total_chars <= max_chars {
        return text.to_string();
    }

    match spill.and_then(|ctx| write_spill(text, &ctx, dir)) {
        Some(written) => {
            let marker = spill_marker(&written.path, total_chars, written.line_count);
            // Reserve the marker's own length so the result stays inside the
            // budget instead of overshooting it by the size of the notice.
            let head_chars = max_chars.saturating_sub(marker.chars().count()).max(1);
            let kept: String = text.chars().take(head_chars).collect();
            format!("{kept}\n{marker}")
        }
        None => {
            let kept: String = text.chars().take(max_chars).collect();
            format!(
                "{kept}\n{PLAIN_TRUNCATION_NOTICE} ({} chars omitted)",
                total_chars.saturating_sub(max_chars)
            )
        }
    }
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
         Re-read only the part you need with read_file({{ \"path\": <path above>, \"start_line\": 1, \"line_count\": 80 }}).\n\
         It is temporary and is removed after {}; re-read it during this turn, not later.",
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
        sanitize_component(ctx.session_hint, 8),
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
    /// Session still holds one open. The next sweep retries them.
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
        // The Manager is the only deleter, so a failure here is never a race
        // with another collector. The file is due either way, so a failure is
        // counted and retried by the next pass rather than surfaced.
        match std::fs::remove_file(entry.path()) {
            Ok(()) => report.removed += 1,
            Err(_) => report.deferred += 1,
        }
    }

    report
}

/// Manager-side collection: one pass at boot, then one per interval.
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
    fn sanitize_component_is_platform_safe() {
        assert_eq!(
            sanitize_component("browser__browser_snapshot", 40),
            "browser__browser_snapshot"
        );
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
        assert!(bounded.contains("chars omitted"));
        assert!(bounded.chars().count() < text.chars().count());
    }

    #[test]
    fn truncation_reserves_room_for_its_own_marker() {
        let text = "a".repeat(50_000);
        let bounded = bound_tool_model_content(&text, 100, None);
        let max_chars = 100 * APPROX_BYTES_PER_TOKEN;

        assert!(
            bounded.chars().count() <= max_chars + 200,
            "result must stay near the budget rather than overshoot by the notice"
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
            Some(SpillContext::new(
                "sess-1234",
                "terminal__terminal_exec",
                "call-abcdef",
            )),
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
        assert!(
            bounded.contains("It is temporary"),
            "marker states the lifetime"
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
        crate::runtime::bootstrap::deny_tool_output_spill_writes_for_test(&mut policy, &pool);

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
