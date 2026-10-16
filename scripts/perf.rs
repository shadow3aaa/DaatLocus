#!/usr/bin/env rust-script
//! Isolated DL backend memory footprint/CPU stress test: `rust-script scripts/perf.rs`.
//!
//! ```cargo
//! [package]
//! edition = "2024"
//! [profile.dev]
//! debug = 0
//! incremental = false
//! [profile.release]
//! debug = 0
//! incremental = false
//! [dependencies]
//! daat-locus = { path = "..", default-features = false }
//! daat-locus-auth = { path = "../crates/daat-locus-auth" }
//! axum = "0.8"
//! base64 = "0.22"
//! clap = { version = "4", features = ["derive"] }
//! futures-util = "0.3"
//! hex = "0.4"
//! miette = { version = "7", features = ["fancy"] }
//! plotters = { version = "0.3", default-features = false, features = ["svg_backend", "line_series"] }
//! reqwest = { version = "0.13", features = ["json", "stream"] }
//! serde = { version = "1", features = ["derive"] }
//! serde_json = "1"
//! sha2 = "0.11"
//! sysinfo = "0.39"
//! tempfile = "3"
//! tokio = { version = "1", features = ["full"] }
//! url = "2"
//! uuid = { version = "1", features = ["v4"] }
//! [target.'cfg(windows)'.dependencies]
//! windows-sys = { version = "0.61", features = ["Win32_Foundation", "Win32_System_Memory", "Win32_System_ProcessStatus", "Win32_System_Threading"] }
//! [target.'cfg(target_os = "macos")'.dependencies]
//! libc = "0.2"
//! ```
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    response::sse::{Event, Sse},
    routing::post,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use clap::{Parser, builder::TypedValueParser};
use daat_locus::{
    actors::{ActorConfig, ActorPermissions, ActorSystem},
    config::{Config, ModelConfig, ProviderConfig},
};
use futures_util::{StreamExt, stream};
use miette::{IntoDiagnostic, Result, miette};
use plotters::{coord::Shift, prelude::*};
use reqwest::{Client, Method};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    convert::Infallible,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

#[derive(Clone, Debug, Parser, Serialize)]
#[command(
    name = "perf.rs",
    about = "Measure DL backend memory footprint/CPU with temporary data and a loopback model mock"
)]
struct Args {
    /// Measure a debug CLI instead of the default release binary.
    #[arg(long)]
    debug_build: bool,
    /// Reuse the selected CLI binary without rebuilding it.
    #[arg(long)]
    skip_build: bool,
    #[arg(long, hide = true, default_value = "", value_parser = clap::builder::OsStringValueParser::new().map(PathBuf::from))]
    binary: PathBuf,
    #[arg(long, hide = true, default_value = "", value_parser = clap::builder::OsStringValueParser::new().map(PathBuf::from))]
    scratch: PathBuf,
    #[arg(long, hide = true, default_value = "", value_parser = clap::builder::OsStringValueParser::new().map(PathBuf::from))]
    output: PathBuf,
    /// Concurrent Actors (each Actor processes its own messages serially).
    #[arg(long, default_value_t = 64, value_parser = clap::value_parser!(u32).range(1..=256))]
    actors: u32,
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=100000))]
    rounds: u32,
    #[arg(long, default_value_t = 16384, value_parser = clap::value_parser!(u32).range(1..=1048576))]
    response_bytes: u32,
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u64).range(0..=60000))]
    model_delay_ms: u64,
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..=3600))]
    idle_seconds: u64,
    #[arg(long, default_value_t = 90, value_parser = clap::value_parser!(u64).range(1..=3600))]
    cooldown_seconds: u64,
    #[arg(long, default_value_t = 250, value_parser = clap::value_parser!(u64).range(100..=10000))]
    sample_ms: u64,
    #[arg(long, default_value_t = 1800, value_parser = clap::value_parser!(u64).range(30..=86400))]
    timeout_seconds: u64,
    #[arg(long, hide = true)]
    seed_model_url: Option<String>,
}

// Only the seed subprocess initializes library-global paths; the driver never does.
async fn seed(args: &Args, url: &str) -> Result<()> {
    let url = url::Url::parse(url).into_diagnostic()?;
    if url.scheme() != "http" || url.host_str() != Some("127.0.0.1") {
        return Err(miette!("Mock provider must use IPv4 loopback"));
    }
    let root = args.scratch.join("profile");
    daat_locus::daat_locus_paths::initialize_native_root(root.clone())
        .map_err(|_| miette!("Paths already initialized"))?;
    let workspace = args.scratch.join("workspace");
    std::fs::create_dir_all(&workspace).into_diagnostic()?;
    let mut config = Config {
        main_model: "perf-mock".into(),
        ..Default::default()
    };
    config.providers.insert(
        "perf-mock".into(),
        ProviderConfig::OpenaiCompatible {
            base_url: url.to_string(),
            api_key: "disposable-perf-key".into(),
        },
    );
    config.models.insert(
        "perf-mock".into(),
        ModelConfig {
            provider: "perf-mock".into(),
            model_id: "perf-mock".into(),
            context_window_tokens: 1_000_000,
            max_completion_tokens: 262144,
            ..Default::default()
        },
    );
    daat_locus::config::write_config(&config)
        .await
        .into_diagnostic()?;
    let system =
        ActorSystem::open_account_gated(root.join("actors"), workspace.clone(), config).await?;
    let mut ids = Vec::new();
    for index in 0..args.actors {
        let path = workspace.join(format!("actor-{index}"));
        std::fs::create_dir_all(&path).into_diagnostic()?;
        let actor = system.create_owned(
            "root",
            ActorConfig {
                name: format!("perf-{index}"),
                workspace: path.clone(),
                apps: vec!["files".into()],
                model: None,
                instructions: String::new(),
                permissions: ActorPermissions::workspace(path, true),
            },
        )?;
        ids.push(actor.id);
    }
    system.shutdown().await;
    std::fs::write(
        args.scratch.join("actors.json"),
        serde_json::to_vec(&ids).into_diagnostic()?,
    )
    .into_diagnostic()?;
    Ok(())
}

#[derive(Clone)]
struct Mock {
    bytes: usize,
    delay: Duration,
    calls: Arc<AtomicUsize>,
}

async fn completion(
    State(mock): State<Mock>,
    Json(request): Json<Value>,
) -> Sse<impl futures_util::Stream<Item = std::result::Result<Event, Infallible>>> {
    mock.calls.fetch_add(1, Ordering::Relaxed);
    // Match production streaming, including incremental content and usage frames.
    let prompt_tokens = request.to_string().len().div_ceil(4);
    let chunks = mock.bytes.div_ceil(1024);
    Sse::new(stream::unfold((0, mock), move |(index, mock)| async move {
        if index > chunks + 1 {
            return None;
        }
        let event = if index < chunks {
            tokio::time::sleep(mock.delay / chunks as u32).await;
            let count = (mock.bytes - index * 1024).min(1024);
            Event::default().data(json!({"choices":[{"index":0,"delta":{"content":"x".repeat(count)},"finish_reason":null}]}).to_string())
        } else if index == chunks {
            Event::default().data(json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":prompt_tokens,"completion_tokens":mock.bytes.div_ceil(4),"total_tokens":prompt_tokens+mock.bytes.div_ceil(4)}}).to_string())
        } else {
            Event::default().data("[DONE]")
        };
        Some((Ok(event), (index + 1, mock)))
    }))
}

fn command(exe: &Path, args: &Args) -> Command {
    let mut command = Command::new(exe);
    // Discard inherited DL settings (including daemonization) and proxy routing.
    for (name, _) in std::env::vars_os() {
        let key = name.to_string_lossy().to_ascii_uppercase();
        if key.starts_with("DAAT_LOCUS_") || key.ends_with("_PROXY") {
            command.env_remove(name);
        }
    }
    command
        .env("DAAT_LOCUS_HOME", args.scratch.join("profile"))
        .env(
            "DAAT_LOCUS_ACCOUNT_STORAGE_ROOT",
            args.scratch.join("profile"),
        )
        .env("NO_PROXY", "*")
        .env("HOME", args.scratch.join("home"))
        .env("USERPROFILE", args.scratch.join("home"))
        .current_dir(args.scratch.join("workspace"));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    command
}

struct Service(Child);
impl Drop for Service {
    fn drop(&mut self) {
        // Retain the exact child handle: never kill by executable name or a stale PID.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn checked_child(command: Command, input: Option<&[u8]>) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut command = tokio::process::Command::from(command);
    let mut child = command
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .into_diagnostic()?;
    if let Some(input) = input {
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(input)
            .await
            .into_diagnostic()?;
    }
    drop(child.stdin.take());
    let output = tokio::select! {
        output = tokio::time::timeout(Duration::from_secs(60), child.wait_with_output()) => output.into_diagnostic()?.into_diagnostic()?,
        _ = tokio::signal::ctrl_c() => return Err(miette!("Setup/control command interrupted")),
    };
    if !output.status.success() {
        return Err(miette!(
            "Setup/control command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

#[derive(Clone)]
struct Api {
    client: Client,
    url: String,
    cookie: String,
}
impl Api {
    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.url))
            .header("origin", &self.url)
            .header("x-daat-request", "1")
            .header("cookie", &self.cookie)
    }
    async fn login(url: String, password: &[u8]) -> Result<Self> {
        let mut api = Self {
            client: Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(30))
                .build()
                .into_diagnostic()?,
            url,
            cookie: String::new(),
        };
        let client = daat_locus_auth::LoginClient::start(password.to_vec()).into_diagnostic()?;
        let reply: Value = api
            .request(Method::POST, "/v1/account/login/start")
            .json(&json!({"name":"perf","request":B64.encode(client.request())}))
            .send()
            .await
            .into_diagnostic()?
            .error_for_status()
            .into_diagnostic()?
            .json()
            .await
            .into_diagnostic()?;
        let proof = client
            .finish(
                &B64.decode(string(&reply, "response")?).into_diagnostic()?,
                &B64.decode(string(&reply, "context")?).into_diagnostic()?,
            )
            .into_diagnostic()?;
        let reply = api
            .request(Method::POST, "/v1/account/login/finish")
            .json(&json!({"id":reply["id"],"message":B64.encode(proof)}))
            .send()
            .await
            .into_diagnostic()?
            .error_for_status()
            .into_diagnostic()?;
        api.cookie = reply
            .headers()
            .get("set-cookie")
            .ok_or_else(|| miette!("Missing account cookie"))?
            .to_str()
            .into_diagnostic()?
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        Ok(api)
    }
    async fn turn(&self, actor: &str, round: u32, expected_bytes: usize) -> Result<f64> {
        let start = Instant::now();
        let message: Value = self
            .request(Method::POST, &format!("/v1/actors/{actor}/messages"))
            .json(&json!({"content":format!("Synthetic resource load {round}"),"attachments":[]}))
            .send()
            .await
            .into_diagnostic()?
            .error_for_status()
            .into_diagnostic()?
            .json()
            .await
            .into_diagnostic()?;
        let id = string(&message, "id")?;
        loop {
            let status: Value = self
                .request(Method::GET, &format!("/v1/actors/{actor}/messages/{id}"))
                .send()
                .await
                .into_diagnostic()?
                .error_for_status()
                .into_diagnostic()?
                .json()
                .await
                .into_diagnostic()?;
            match status["status"].as_str() {
                Some("completed") => {
                    if status["reply"].as_str().map(str::len) != Some(expected_bytes) {
                        return Err(miette!(
                            "Completed message did not preserve the mock response"
                        ));
                    }
                    return Ok(start.elapsed().as_secs_f64());
                }
                Some("failed" | "cancelled" | "interrupted") => {
                    return Err(miette!("Synthetic message ended with {}", status["status"]));
                }
                _ => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| miette!("Missing response field {key}"))
}

// These are explicitly different OS accounting metrics, not interchangeable
// physical-memory measurements. Never substitute RSS or virtual address size.
fn footprint_metric() -> &'static str {
    #[cfg(windows)]
    {
        "Windows Private Bytes (private commit, including paged-out memory)"
    }
    #[cfg(target_os = "macos")]
    {
        "macOS phys_footprint (includes compressed memory)"
    }
    #[cfg(target_os = "linux")]
    {
        "Linux PSS + SwapPss (proportional resident + swapped memory)"
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        "unsupported platform"
    }
}

struct FootprintProbe {
    #[cfg(windows)]
    handle: std::os::windows::io::OwnedHandle,
    #[cfg(not(windows))]
    pid: u32,
}

impl FootprintProbe {
    fn new(pid: u32) -> Result<Self> {
        #[cfg(windows)]
        {
            use std::os::windows::io::FromRawHandle;
            use windows_sys::Win32::System::Threading::{
                OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            };
            // Keep the exact process handle for the entire run, including failure cleanup.
            let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
            if handle.is_null() {
                return Err(std::io::Error::last_os_error()).into_diagnostic();
            }
            Ok(Self {
                handle: unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle) },
            })
        }
        #[cfg(not(windows))]
        {
            Ok(Self { pid })
        }
    }

    fn bytes(&self) -> Result<u64> {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::System::ProcessStatus::{
                GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
            };
            let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
            let size = std::mem::size_of_val(&counters) as u32;
            counters.cb = size;
            // The extended layout returns PrivateUsage, not WorkingSetSize or
            // reserved address space. Propagate query failures instead of stale data.
            let ok = unsafe {
                GetProcessMemoryInfo(
                    self.handle.as_raw_handle(),
                    (&raw mut counters).cast(),
                    size,
                )
            };
            if ok == 0 {
                return Err(std::io::Error::last_os_error()).into_diagnostic();
            }
            Ok(counters.PrivateUsage as u64)
        }
        #[cfg(target_os = "macos")]
        {
            // RUSAGE_INFO_V0 already contains phys_footprint; no task port or
            // debugger entitlement is needed to query our own child with libproc.
            let mut usage: libc::rusage_info_v0 = unsafe { std::mem::zeroed() };
            let ok = unsafe {
                libc::proc_pid_rusage(
                    self.pid.try_into().into_diagnostic()?,
                    libc::RUSAGE_INFO_V0,
                    (&raw mut usage).cast(),
                )
            };
            if ok != 0 {
                return Err(std::io::Error::last_os_error()).into_diagnostic();
            }
            Ok(usage.ri_phys_footprint)
        }
        #[cfg(target_os = "linux")]
        {
            linux_footprint(
                &std::fs::read_to_string(format!("/proc/{}/smaps_rollup", self.pid))
                    .into_diagnostic()?,
            )
        }
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        {
            Err(miette!(
                "Memory footprint sampling is unsupported on this platform (PID {})",
                self.pid
            ))
        }
    }
}

#[cfg(any(target_os = "linux", test))]
fn linux_footprint(rollup: &str) -> Result<u64> {
    let kb = |key: &str| -> Result<u64> {
        let line = rollup
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .ok_or_else(|| miette!("smaps_rollup is missing {key}"))?;
        let mut fields = line.split_whitespace();
        let value = fields
            .next()
            .ok_or_else(|| miette!("Missing {key} value"))?
            .parse::<u64>()
            .into_diagnostic()?;
        if fields.next() != Some("kB") || fields.next().is_some() {
            return Err(miette!("Unexpected smaps_rollup units for {key}"));
        }
        Ok(value)
    };
    kb("Pss:")?
        .checked_add(kb("SwapPss:")?)
        .and_then(|kb| kb.checked_mul(1024))
        .ok_or_else(|| miette!("smaps_rollup footprint overflow"))
}

#[derive(Clone, Serialize)]
struct Sample {
    phase: String,
    elapsed_s: f64,
    footprint_bytes: u64,
    rss_bytes: u64,
    cpu_ms: u64,
}
#[derive(Serialize)]
struct Phase {
    name: String,
    seconds: f64,
    samples: usize,
    footprint_start_bytes: u64,
    footprint_end_bytes: u64,
    footprint_peak_bytes: u64,
    footprint_p95_bytes: u64,
    rss_start_bytes: u64,
    rss_end_bytes: u64,
    rss_peak_bytes: u64,
    rss_p95_bytes: u64,
    cpu_seconds: f64,
    cpu_percent_one_core: f64,
}
struct Sampler {
    system: System,
    pid: Pid,
    start: Instant,
    csv: File,
    interval: Duration,
    phases: Vec<Phase>,
    history: Vec<Sample>,
    footprint: FootprintProbe,
}
impl Sampler {
    fn take(&mut self, phase: &str) -> Result<Sample> {
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[self.pid]),
            true,
            ProcessRefreshKind::nothing().with_memory().with_cpu(),
        );
        let process = self
            .system
            .process(self.pid)
            .ok_or_else(|| miette!("DL exited during {phase}"))?;
        let sample = Sample {
            phase: phase.into(),
            elapsed_s: self.start.elapsed().as_secs_f64(),
            footprint_bytes: self.footprint.bytes()?,
            rss_bytes: process.memory(),
            cpu_ms: process.accumulated_cpu_time(),
        };
        writeln!(
            self.csv,
            "{phase},{:.6},{},{},{}",
            sample.elapsed_s, sample.footprint_bytes, sample.rss_bytes, sample.cpu_ms
        )
        .into_diagnostic()?;
        self.csv.flush().into_diagnostic()?;
        // Retain every sample even if a phase is cancelled by the outer deadline.
        self.history.push(sample.clone());
        Ok(sample)
    }
    async fn phase<T>(&mut self, name: &str, work: impl Future<Output = Result<T>>) -> Result<T> {
        println!("Phase: {name}");
        let mut samples = vec![self.take(name)?];
        let mut tick = tokio::time::interval(self.interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        tick.tick().await;
        tokio::pin!(work);
        let result = loop {
            tokio::select! {
                result = &mut work => break result,
                _ = tick.tick() => samples.push(self.take(name)?),
            }
        };
        samples.push(self.take(name)?);
        let first = &samples[0];
        let last = samples.last().unwrap();
        let seconds = last.elapsed_s - first.elapsed_s;
        let cpu_seconds = last.cpu_ms.saturating_sub(first.cpu_ms) as f64 / 1000.0;
        let mut rss: Vec<_> = samples.iter().map(|s| s.rss_bytes).collect();
        rss.sort_unstable();
        let mut footprint: Vec<_> = samples.iter().map(|s| s.footprint_bytes).collect();
        footprint.sort_unstable();
        self.phases.push(Phase {
            name: name.into(),
            seconds,
            samples: samples.len(),
            footprint_start_bytes: first.footprint_bytes,
            footprint_end_bytes: last.footprint_bytes,
            footprint_peak_bytes: *footprint.last().unwrap(),
            footprint_p95_bytes: footprint[(footprint.len() * 95).div_ceil(100) - 1],
            rss_start_bytes: first.rss_bytes,
            rss_end_bytes: last.rss_bytes,
            rss_peak_bytes: *rss.last().unwrap(),
            rss_p95_bytes: rss[(rss.len() * 95).div_ceil(100) - 1],
            cpu_seconds,
            cpu_percent_one_core: 100.0 * cpu_seconds / seconds,
        });
        result
    }
}

// Merge short phase-boundary intervals before differentiating millisecond CPU
// counters. Attribute each resulting average to the entire observed interval.
fn cpu_intervals(samples: &[Sample], minimum_seconds: f64) -> Vec<(f64, f64, f64)> {
    let Some(mut previous) = samples.first() else {
        return Vec::new();
    };
    let mut intervals = Vec::new();
    for sample in &samples[1..] {
        let seconds = sample.elapsed_s - previous.elapsed_s;
        if seconds < minimum_seconds {
            continue;
        }
        let percent = sample.cpu_ms.saturating_sub(previous.cpu_ms) as f64 / (10.0 * seconds);
        intervals.push((previous.elapsed_s, sample.elapsed_s, percent));
        previous = sample;
    }
    // Fold a short trailing interval into the previous one; do not discard CPU time.
    if let (Some(last), Some(&(start, _, _))) = (samples.last(), intervals.last())
        && last.elapsed_s > previous.elapsed_s
    {
        let anchor = samples.iter().find(|s| s.elapsed_s == start).unwrap();
        *intervals.last_mut().unwrap() = (
            start,
            last.elapsed_s,
            last.cpu_ms.saturating_sub(anchor.cpu_ms) as f64 / (10.0 * (last.elapsed_s - start)),
        );
    }
    intervals
}

fn phase_color(name: &str) -> RGBColor {
    match name {
        "startup" => RGBColor(148, 163, 184),
        "login" => RGBColor(168, 85, 247),
        "idle" => RGBColor(14, 165, 233),
        "load" => RGBColor(249, 115, 22),
        _ => RGBColor(16, 185, 129),
    }
}

fn time_chart(
    area: &DrawingArea<SVGBackend<'_>, Shift>,
    samples: &[Sample],
    points: Vec<(f64, f64)>,
    caption: &str,
    unit: &str,
    color: RGBColor,
) -> Result<()> {
    let xmax = samples.last().map_or(1.0, |s| s.elapsed_s.max(1.0));
    let ymax = points.iter().map(|p| p.1).fold(1.0, f64::max) * 1.12;
    let mut chart = ChartBuilder::on(area)
        .caption(caption, ("sans-serif", 22))
        .margin(12)
        .x_label_area_size(38)
        .y_label_area_size(72)
        .build_cartesian_2d(0.0..xmax, 0.0..ymax)
        .into_diagnostic()?;
    let mut start = 0;
    while start < samples.len() {
        let end = (start + 1..samples.len())
            .find(|&i| samples[i].phase != samples[start].phase)
            .unwrap_or(samples.len());
        let right = samples.get(end).map_or(xmax, |s| s.elapsed_s);
        chart
            .draw_series(std::iter::once(Rectangle::new(
                [(samples[start].elapsed_s, 0.0), (right, ymax)],
                phase_color(&samples[start].phase).mix(0.10).filled(),
            )))
            .into_diagnostic()?;
        start = end;
    }
    chart
        .configure_mesh()
        .x_desc("Elapsed time (s)")
        .y_desc(unit)
        .max_light_lines(0)
        .draw()
        .into_diagnostic()?;
    chart
        .draw_series(LineSeries::new(points, color.stroke_width(2)))
        .into_diagnostic()?;
    Ok(())
}

fn write_charts(args: &Args, samples: &[Sample], latencies: &[f64], success: bool) -> Result<()> {
    let path = args.output.join("charts.svg");
    let root = SVGBackend::new(&path, (1200, 1080)).into_drawing_area();
    root.fill(&WHITE).into_diagnostic()?;
    let status = if success { "PASS" } else { "FAILED / PARTIAL" };
    let root = root
        .titled(
            &format!(
                "DL backend | {status} | {} Actors x {} rounds | {} / {} completed | {} KiB/reply",
                args.actors,
                args.rounds,
                latencies.len(),
                args.actors * args.rounds,
                args.response_bytes as f64 / 1024.0,
            ),
            ("sans-serif", 23),
        )
        .into_diagnostic()?;
    let (legend, plots) = root.split_vertically(45);
    for (i, name) in ["startup", "login", "idle", "load", "cooldown"]
        .iter()
        .enumerate()
    {
        let x = 90 + i as i32 * 210;
        legend
            .draw(&Rectangle::new(
                [(x, 13), (x + 18, 31)],
                phase_color(name).filled(),
            ))
            .into_diagnostic()?;
        legend
            .draw(&Text::new(*name, (x + 28, 28), ("sans-serif", 18)))
            .into_diagnostic()?;
    }
    let areas = plots.split_evenly((3, 1));
    time_chart(
        &areas[0],
        samples,
        samples
            .iter()
            .map(|s| (s.elapsed_s, s.footprint_bytes as f64 / 1048576.0))
            .collect(),
        &format!("Memory footprint | {} | sampled peaks", footprint_metric()),
        "Footprint (MiB)",
        RGBColor(37, 99, 235),
    )?;
    let cpu = cpu_intervals(samples, args.sample_ms as f64 / 2000.0);
    time_chart(
        &areas[1],
        samples,
        cpu.into_iter()
            .flat_map(|(start, end, value)| [(start, value), (end, value)])
            .collect(),
        "DL process CPU (interval averages; 100% = one logical CPU)",
        "CPU (%)",
        RGBColor(217, 119, 6),
    )?;
    let max_latency = latencies.last().copied().unwrap_or(1.0).max(0.001);
    let width = max_latency * 1.001 / 30.0;
    let mut bins = [0u32; 30];
    for &latency in latencies {
        bins[((latency / width) as usize).min(29)] += 1;
    }
    let p95 = latencies.get((latencies.len() * 95).div_ceil(100).saturating_sub(1));
    let caption = p95.map_or_else(
        || "Completed-turn latency: no completed messages".to_owned(),
        |p95| format!("Completed-turn latency | p95 {p95:.3} s | includes 100 ms status polling"),
    );
    let mut chart = ChartBuilder::on(&areas[2])
        .caption(caption, ("sans-serif", 22))
        .margin(12)
        .x_label_area_size(38)
        .y_label_area_size(72)
        .build_cartesian_2d(
            0.0..width * 30.0,
            0.0..(*bins.iter().max().unwrap()).max(1) as f64 + 1.0,
        )
        .into_diagnostic()?;
    chart
        .configure_mesh()
        .x_desc("Turn latency (s)")
        .y_desc("Completed messages")
        .max_light_lines(0)
        .draw()
        .into_diagnostic()?;
    chart
        .draw_series(
            bins.iter()
                .enumerate()
                .filter(|(_, count)| **count > 0)
                .map(|(i, &count)| {
                    Rectangle::new(
                        [
                            (i as f64 * width, 0.0),
                            ((i + 1) as f64 * width, count as f64),
                        ],
                        RGBColor(13, 148, 136).filled(),
                    )
                }),
        )
        .into_diagnostic()?;
    if let Some(&p95) = p95 {
        chart
            .draw_series(std::iter::once(PathElement::new(
                vec![(p95, 0.0), (p95, *bins.iter().max().unwrap() as f64)],
                RED.stroke_width(2),
            )))
            .into_diagnostic()?;
    }
    root.present().into_diagnostic()?;
    std::fs::write(
        args.output.join("latencies.csv"),
        format!(
            "completed_turn_seconds\n{}",
            latencies
                .iter()
                .map(|s| format!("{s:.6}\n"))
                .collect::<String>(),
        ),
    )
    .into_diagnostic()?;
    println!("Charts: {}", path.display());
    Ok(())
}

async fn ready(args: &Args) -> Result<String> {
    loop {
        let text = std::fs::read_to_string(args.scratch.join("service.stdout")).unwrap_or_default();
        if let Some(line) = text
            .lines()
            .find_map(|line| line.strip_prefix("Daat Locus API listening on "))
        {
            return Ok(line.split(';').next().unwrap().to_owned());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn run(args: &Args) -> Result<()> {
    if std::fs::read_dir(&args.scratch)
        .into_diagnostic()?
        .next()
        .is_some()
    {
        return Err(miette!("Scratch directory must be empty"));
    }
    std::fs::create_dir_all(&args.output).into_diagnostic()?;
    std::fs::create_dir_all(args.scratch.join("workspace")).into_diagnostic()?;
    std::fs::create_dir_all(args.scratch.join("home")).into_diagnostic()?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .into_diagnostic()?;
    let mock_url = format!("http://{}/v1", listener.local_addr().into_diagnostic()?);
    let calls = Arc::new(AtomicUsize::new(0));
    let mock = Mock {
        bytes: args.response_bytes as usize,
        delay: Duration::from_millis(args.model_delay_ms),
        calls: calls.clone(),
    };
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/v1/chat/completions", post(completion))
                .layer(DefaultBodyLimit::max(16 * 1024 * 1024))
                .with_state(mock),
        )
        .await
    });
    // Kill-on-drop on each setup subprocess also covers cancellation and timeout.
    let mut seed_command = command(&std::env::current_exe().into_diagnostic()?, args);
    seed_command
        .args(std::env::args_os().skip(1))
        .arg("--binary")
        .arg(&args.binary)
        .arg("--scratch")
        .arg(&args.scratch)
        .arg("--output")
        .arg(&args.output)
        .arg("--seed-model-url")
        .arg(mock_url);
    checked_child(seed_command, None).await?;
    let password = uuid::Uuid::new_v4().to_string();
    let mut account = command(&args.binary, args);
    account
        .args([
            "account",
            "create",
            "perf",
            "--password-stdin",
            "--workspace",
        ])
        .arg(args.scratch.join("workspace"));
    checked_child(account, Some(password.as_bytes())).await?;
    let stdout = File::create(args.scratch.join("service.stdout")).into_diagnostic()?;
    let stderr = File::create(args.scratch.join("service.stderr")).into_diagnostic()?;
    let mut service_command = command(&args.binary, args);
    service_command
        .args(["serve", "--listen", "127.0.0.1:0", "--workspace"])
        .arg(args.scratch.join("workspace"))
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr);
    let mut service = Service(service_command.spawn().into_diagnostic()?);
    let mut csv = File::create(args.output.join("samples.csv")).into_diagnostic()?;
    writeln!(
        csv,
        "phase,elapsed_s,footprint_bytes,rss_bytes,cpu_total_ms"
    )
    .into_diagnostic()?;
    let mut sampler = Sampler {
        system: System::new(),
        pid: Pid::from_u32(service.0.id()),
        start: Instant::now(),
        csv,
        interval: Duration::from_millis(args.sample_ms),
        phases: Vec::new(),
        history: Vec::new(),
        footprint: FootprintProbe::new(service.0.id())?,
    };
    let latencies = Arc::new(Mutex::new(Vec::new()));
    let work = async {
        let url = sampler.phase("startup", ready(args)).await?;
        let api = sampler
            .phase("login", Api::login(url, password.as_bytes()))
            .await?;
        sampler
            .phase("idle", async {
                tokio::time::sleep(Duration::from_secs(args.idle_seconds)).await;
                Ok(())
            })
            .await?;
        let actors: Vec<String> = serde_json::from_slice(
            &std::fs::read(args.scratch.join("actors.json")).into_diagnostic()?,
        )
        .into_diagnostic()?;
        sampler
            .phase("load", async {
                let mut tasks = stream::iter(actors.into_iter().map(|actor| {
                    let api = api.clone();
                    let latencies = latencies.clone();
                    async move {
                        for round in 0..args.rounds {
                            let elapsed = api
                                .turn(&actor, round, args.response_bytes as usize)
                                .await?;
                            latencies.lock().unwrap().push(elapsed);
                        }
                        Ok::<_, miette::Report>(())
                    }
                }))
                .buffer_unordered(args.actors as usize);
                let mut progress = tokio::time::interval(Duration::from_secs(10));
                progress.tick().await;
                loop {
                    tokio::select! {
                        result = tasks.next() => match result {
                            Some(result) => result?,
                            None => break,
                        },
                        _ = progress.tick() => println!("Load: {}/{} completed", latencies.lock().unwrap().len(), args.actors * args.rounds),
                    }
                }
                Ok(())
            })
            .await?;
        sampler
            .phase("cooldown", async {
                tokio::time::sleep(Duration::from_secs(args.cooldown_seconds)).await;
                Ok(())
            })
            .await?;
        if calls.load(Ordering::Relaxed) != (args.actors * args.rounds) as usize {
            return Err(miette!(
                "Unexpected model request count; retry or extra inference occurred"
            ));
        }
        Ok(())
    };
    let result: Result<()> = tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(args.timeout_seconds), work) => result.unwrap_or_else(|_| Err(miette!("Run timed out"))),
        _ = tokio::signal::ctrl_c() => Err(miette!("Run interrupted")),
    };
    let mut stop = command(&args.binary, args);
    stop.arg("stop");
    let stop_result =
        tokio::time::timeout(Duration::from_secs(15), checked_child(stop, None)).await;
    let graceful = matches!(stop_result, Ok(Ok(())))
        && tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if service.0.try_wait().into_diagnostic()?.is_some() {
                    return Ok::<_, miette::Report>(());
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .is_ok_and(|r| r.is_ok());
    drop(service);
    server.abort();
    let _ = server.await;
    let result = result.and_then(|()| {
        if graceful {
            Ok(())
        } else {
            Err(miette!("Graceful stop failed; child was forcibly reaped"))
        }
    });
    let mut latencies = latencies.lock().unwrap();
    latencies.sort_by(f64::total_cmp);
    let p95 = latencies
        .get((latencies.len() * 95).div_ceil(100).saturating_sub(1))
        .copied();
    let load_seconds = sampler
        .phases
        .iter()
        .find(|p| p.name == "load")
        .map(|p| p.seconds);
    let revision = Command::new("git")
        .current_dir(repository()?)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
    let dirty = Command::new("git")
        .current_dir(repository()?)
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| !output.stdout.is_empty());
    let report = json!({"schema_version":2,"success":result.is_ok(),"error":result.as_ref().err().map(|e|format!("{e:?}")),
        "scope":"actual CLI backend PID only; excludes driver, mock, build, account seeding and desktop WebView", "footprint_metric":footprint_metric(), "rss_metric":"auxiliary resident bytes; Windows Working Set", "cpu_metric":"process user+kernel CPU seconds; 100% = one logical CPU",
        "pid":sampler.pid.as_u32(),"graceful_shutdown":graceful,"args":args,"os":std::env::consts::OS,"arch":std::env::consts::ARCH,
        "logical_cpus":std::thread::available_parallelism().map(|n|n.get()).ok(),"git_revision":revision,"git_dirty":dirty,
        "binary_sha256":hex::encode(Sha256::digest(std::fs::read(&args.binary).into_diagnostic()?)),
        "driver_debug_assertions":cfg!(debug_assertions),"mock_requests":calls.load(Ordering::Relaxed),"completed_messages":latencies.len(),"latency_p95_seconds":p95,
        "messages_per_second":load_seconds.map(|s|latencies.len() as f64/s),"phases":sampler.phases});
    std::fs::write(
        args.output.join("summary.json"),
        serde_json::to_vec_pretty(&report).into_diagnostic()?,
    )
    .into_diagnostic()?;
    for name in ["service.stdout", "service.stderr"] {
        std::fs::copy(args.scratch.join(name), args.output.join(name)).into_diagnostic()?;
    }
    println!("Report: {}", args.output.display());
    write_charts(args, &sampler.history, &latencies, result.is_ok())?;
    result
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = Args::parse();
    if let Some(url) = &args.seed_model_url {
        return seed(&args, url).await;
    }
    if !args.binary.as_os_str().is_empty()
        || !args.scratch.as_os_str().is_empty()
        || !args.output.as_os_str().is_empty()
    {
        return Err(miette!(
            "Internal paths are reserved for the seed subprocess"
        ));
    }
    let repo = repository()?;
    if !args.skip_build {
        let mut build = tokio::process::Command::new("cargo");
        build
            .current_dir(&repo)
            .env("DAAT_LOCUS_BUILD_WEBUI", "0")
            .args([
                "build",
                "--locked",
                "--no-default-features",
                "--bin",
                "daat-locus",
            ])
            .kill_on_drop(true);
        if !args.debug_build {
            build.arg("--release");
        }
        let status = tokio::select! {
            status = build.status() => status.into_diagnostic()?,
            _ = tokio::signal::ctrl_c() => return Err(miette!("Build interrupted")),
        };
        if !status.success() {
            return Err(miette!("DL binary failed to build"));
        }
    }
    let profile = if args.debug_build { "debug" } else { "release" };
    args.binary = repo
        .join("target")
        .join(profile)
        .join(format!("daat-locus{}", std::env::consts::EXE_SUFFIX))
        .canonicalize()
        .into_diagnostic()?;
    let temp_root = std::env::temp_dir().canonicalize().into_diagnostic()?;
    let temp = tempfile::Builder::new()
        .prefix("daat-locus-perf-")
        .tempdir_in(&temp_root)
        .into_diagnostic()?;
    args.scratch = temp.path().canonicalize().into_diagnostic()?;
    if args.scratch.parent() != Some(temp_root.as_path()) {
        let path = temp.keep();
        return Err(miette!(
            "Temporary directory is outside expected parent: {}",
            path.display()
        ));
    }
    args.output = repo.join("target/perf").join(
        args.scratch
            .file_name()
            .ok_or_else(|| miette!("Missing run directory name"))?,
    );
    let result = run(&args).await;
    // Validate immediately before recursive removal; never accept caller-supplied paths.
    if temp.path().canonicalize().ok().as_ref() != Some(&args.scratch) {
        let path = temp.keep();
        return Err(miette!(
            "Refusing cleanup of changed temporary path: {}",
            path.display()
        ));
    }
    temp.close().into_diagnostic()?;
    result
}

fn repository() -> Result<PathBuf> {
    // rust-script's generated Cargo project lives in its cache, not in this checkout.
    Path::new(env!("RUST_SCRIPT_BASE_PATH"))
        .join("..")
        .canonicalize()
        .into_diagnostic()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_footprint_accounts_for_swap_and_requires_both_fields() -> Result<()> {
        assert_eq!(
            linux_footprint("Rss: 900 kB\nPss: 100 kB\nPss_Anon: 90 kB\nSwapPss: 25 kB\n")?,
            125 * 1024
        );
        assert!(linux_footprint("Pss: 100 kB\n").is_err());
        assert!(linux_footprint("Pss: 100 MB\nSwapPss: 0 kB\n").is_err());
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn windows_footprint_counts_committed_pages_without_touching_them() -> Result<()> {
        use windows_sys::Win32::System::Memory::{
            MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree,
        };
        struct Allocation(*mut std::ffi::c_void);
        impl Drop for Allocation {
            fn drop(&mut self) {
                unsafe {
                    VirtualFree(self.0, 0, MEM_RELEASE);
                }
            }
        }
        let probe = FootprintProbe::new(std::process::id())?;
        let before = probe.bytes()?;
        let size = 32 * 1024 * 1024;
        let pointer = unsafe {
            VirtualAlloc(
                std::ptr::null(),
                size,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            )
        };
        if pointer.is_null() {
            return Err(std::io::Error::last_os_error()).into_diagnostic();
        }
        let _allocation = Allocation(pointer);
        // Pages have not been touched: RSS would not account for this allocation.
        // Allow small unrelated allocator changes from parallel tests.
        assert!(probe.bytes()? >= before + size as u64 - 1024 * 1024);
        Ok(())
    }

    #[test]
    fn cpu_boundary_merging_preserves_accumulated_time() {
        let samples: Vec<_> = [
            (0.0, 100),
            (0.25, 350),
            (0.251, 350),
            (0.5, 850),
            (0.501, 850),
        ]
        .into_iter()
        .map(|(elapsed_s, cpu_ms)| Sample {
            phase: "load".into(),
            elapsed_s,
            footprint_bytes: 0,
            rss_bytes: 0,
            cpu_ms,
        })
        .collect();
        let intervals = cpu_intervals(&samples, 0.125);
        assert_eq!(intervals.len(), 2);
        assert!((intervals[0].2 - 100.0).abs() < 1e-9);
        assert!(intervals[1].2 > 100.0); // Multiple cores must not be capped.
        let cpu_seconds: f64 = intervals.iter().map(|(a, b, p)| (b - a) * p / 100.0).sum();
        assert!((cpu_seconds - 0.75).abs() < 1e-9);
        assert_eq!(intervals.last().unwrap().1, 0.501);
    }
}
