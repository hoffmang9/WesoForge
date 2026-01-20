use std::borrow::Cow;
use std::io::Write;
use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use clap::Parser;
use reqwest::Url;
use serde::{Deserialize, Serialize};

use bbr_client_chiavdf_fast::{prove_one_weso_fast, prove_one_weso_fast_with_progress};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    /// Interactive TUI-like output (single line updated in place).
    Tui,
    /// Newline-based progress logs (no ANSI cursor control).
    Plain,
    /// Minimal output (final line per job).
    Quiet,
}

#[derive(Debug, Clone, Parser)]
#[command(name = "bbr-client", version, about = "BBR compact proof worker")]
struct Cli {
    #[arg(long, env = "BBR_BACKEND_URL", default_value = "http://127.0.0.1:8080")]
    backend_url: Url,

    #[arg(long, env = "BBR_WORKER_ID")]
    worker_id: Option<String>,

    #[arg(long, env = "BBR_IDLE_SLEEP_SECONDS", default_value_t = 10)]
    idle_sleep_seconds: u64,

    #[arg(long, env = "BBR_DISCRIMINANT_BITS", default_value_t = 1024)]
    discriminant_bits: usize,

    #[arg(long, env = "BBR_NO_TUI", default_value_t = false)]
    no_tui: bool,

    /// Run a local benchmark (e.g. `--bench 0`) and exit.
    #[arg(long, value_name = "ALGO")]
    bench: Option<u32>,
}

#[derive(Debug, Serialize)]
struct LeaseRequest {
    count: u32,
    worker_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LeaseResponse {
    lease_id: String,
    lease_expires_at: i64,
    jobs: Vec<JobDto>,
}

#[derive(Debug, Deserialize)]
struct JobDto {
    job_id: u64,
    height: u32,
    field_vdf: i32,
    challenge_b64: String,
    number_of_iterations: u64,
    output_b64: String,
}

#[derive(Debug, Serialize)]
struct SubmitRequest {
    lease_id: String,
    witness_b64: String,
}

#[derive(Debug, Deserialize)]
struct SubmitResponse {
    reason: String,
    detail: String,
    accepted_event_id: Option<u64>,
}

fn field_vdf_label(field_vdf: i32) -> Cow<'static, str> {
    match field_vdf {
        1 => Cow::Borrowed("CC_EOS_VDF"),
        2 => Cow::Borrowed("ICC_EOS_VDF"),
        3 => Cow::Borrowed("CC_SP_VDF"),
        4 => Cow::Borrowed("CC_IP_VDF"),
        other => Cow::Owned(format!("UNKNOWN_VDF({other})")),
    }
}

fn default_worker_id() -> String {
    std::env::var("HOSTNAME").unwrap_or_else(|_| "bbr-client".to_string())
}

fn print_job_line(line: &str, mode: OutputMode) {
    match mode {
        OutputMode::Tui => {
            print!("\r\x1b[2K{line}");
            let _ = std::io::stdout().flush();
        }
        OutputMode::Plain => {
            println!("{line}");
            let _ = std::io::stdout().flush();
        }
        OutputMode::Quiet => {}
    }
}

fn complete_job_line(line: &str, mode: OutputMode) {
    match mode {
        OutputMode::Tui => {
            print!("\r\x1b[2K{line}\n");
            let _ = std::io::stdout().flush();
        }
        OutputMode::Plain | OutputMode::Quiet => {
            println!("{line}");
        }
    }
}

fn print_error_line(line: &str, mode: OutputMode, restore_status_line: Option<&str>) {
    match mode {
        OutputMode::Tui => {
            eprint!("\r\x1b[2K{line}\n");
            let _ = std::io::stderr().flush();
            if let Some(status_line) = restore_status_line {
                print_job_line(status_line, OutputMode::Tui);
            }
        }
        OutputMode::Plain | OutputMode::Quiet => {
            eprintln!("{line}");
        }
    }
}

fn default_classgroup_element() -> [u8; 100] {
    let mut el = [0u8; 100];
    el[0] = 0x08;
    el
}

fn format_number(n: u64) -> String {
    let s = n.to_string();
    let len = s.len();
    if len <= 3 {
        return s;
    }

    let mut out = String::with_capacity(len + (len - 1) / 3);
    for (i, ch) in s.chars().enumerate() {
        if i != 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn format_duration(d: Duration) -> String {
    let ms = d.as_millis() as u64;
    if ms < 60_000 {
        let secs = ms / 1000;
        let tenths = (ms % 1000) / 100;
        return format!("{secs}.{tenths}s");
    }

    if ms < 3_600_000 {
        let minutes = ms / 60_000;
        let seconds = (ms % 60_000) / 1000;
        return format!("{minutes}m{seconds:02}s");
    }

    if ms < 86_400_000 {
        let hours = ms / 3_600_000;
        let minutes = (ms % 3_600_000) / 60_000;
        let seconds = (ms % 60_000) / 1000;
        return format!("{hours}h{minutes:02}m{seconds:02}s");
    }

    let days = ms / 86_400_000;
    let hours = (ms % 86_400_000) / 3_600_000;
    let minutes = (ms % 3_600_000) / 60_000;
    let seconds = (ms % 60_000) / 1000;
    format!("{days}d{hours:02}h{minutes:02}m{seconds:02}s")
}

fn format_job_status_line(
    height: u32,
    field_vdf: i32,
    status: &str,
    progress: Option<(u64, u64)>,
    elapsed: Duration,
    is_final: bool,
) -> String {
    let field = field_vdf_label(field_vdf);
    let mut line = format!("Block: {height} ({field}), Status: {status}");
    if let Some((done, total)) = progress {
        line.push_str(&format!(
            " ({}/{})",
            format_number(done),
            format_number(total)
        ));
    }
    let label = if is_final { "Duration" } else { "Elapsed" };
    line.push_str(&format!(", {label}: {}", format_duration(elapsed)));
    line
}

fn run_benchmark(algo: u32) -> anyhow::Result<()> {
    const BENCH_DISCRIMINANT_BITS: usize = 1024;
    const BENCH_ITERS: u64 = 14_576_841;
    const WARMUP_ITERS: u64 = 10_000;
    const BENCH_CHALLENGE: [u8; 32] = [
        0x62, 0x62, 0x72, 0x2d, 0x63, 0x6c, 0x69, 0x65, 0x6e, 0x74, 0x2d, 0x62, 0x65, 0x6e,
        0x63, 0x68, 0x2d, 0x76, 0x31, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
        0x09, 0x0a, 0x0b, 0x0c,
    ];

    let x = default_classgroup_element();

    match algo {
        0 => {
            let _ = prove_one_weso_fast(
                &BENCH_CHALLENGE,
                &x,
                BENCH_DISCRIMINANT_BITS,
                WARMUP_ITERS,
            )
            .context("warmup prove_one_weso_fast")?;

            let started_at = Instant::now();
            let out = prove_one_weso_fast(&BENCH_CHALLENGE, &x, BENCH_DISCRIMINANT_BITS, BENCH_ITERS)
                .context("bench prove_one_weso_fast")?;
            let duration = started_at.elapsed();

            let half = out.len() / 2;
            let y = &out[..half];
            let witness = &out[half..];

            println!("Benchmark algo: {algo}");
            println!("Discriminant bits: {BENCH_DISCRIMINANT_BITS}");
            println!("Challenge (b64): {}", B64.encode(BENCH_CHALLENGE));
            println!("Iterations: {}", format_number(BENCH_ITERS));
            println!("Y (b64): {}", B64.encode(y));
            println!("Witness (b64): {}", B64.encode(witness));
            println!("Duration: {}", format_duration(duration));
            Ok(())
        }
        _ => anyhow::bail!("unknown --bench algo {algo} (supported: 0)"),
    }
}

fn humanize_submit_reason(reason: &str) -> String {
    let s = reason.trim();
    if s.is_empty() {
        return "Unknown".to_string();
    }

    let lower = s.to_ascii_lowercase();
    match lower.as_str() {
        "accepted" => return "Accepted".to_string(),
        "already_compact" => return "Already compact".to_string(),
        _ => {}
    }

    let mut out = String::with_capacity(lower.len());
    let mut capitalize_next = true;
    for ch in lower.chars() {
        if ch == '_' || ch == '-' {
            out.push(' ');
            capitalize_next = true;
            continue;
        }
        if capitalize_next {
            out.extend(ch.to_uppercase());
            capitalize_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    if let Some(algo) = cli.bench {
        run_benchmark(algo)?;
        return Ok(());
    }

    let worker_id = cli.worker_id.clone().unwrap_or_else(default_worker_id);
    let is_tty = std::io::stdout().is_terminal();
    let output_mode = if cli.no_tui {
        OutputMode::Plain
    } else if is_tty {
        OutputMode::Tui
    } else {
        OutputMode::Quiet
    };

    println!(
        "bbr-client {} backend={} worker_id={}",
        env!("CARGO_PKG_VERSION"),
        cli.backend_url,
        worker_id
    );

    let shutdown = Arc::new(ShutdownController::new());
    spawn_ctrl_c_handler(shutdown.clone());

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .context("build http client")?;

    loop {
        if shutdown.should_exit_graceful() {
            break;
        }

        let lease = match lease_one_job(&http, &cli.backend_url, &worker_id).await
        {
            Ok(v) => v,
            Err(err) => {
                eprintln!("lease error: {err:?}");
                tokio::time::sleep(Duration::from_secs(cli.idle_sleep_seconds)).await;
                continue;
            }
        };

        let Some(job) = lease.jobs.into_iter().next() else {
            tokio::time::sleep(Duration::from_secs(cli.idle_sleep_seconds)).await;
            continue;
        };

        let height = job.height;
        let field_vdf = job.field_vdf;
        let total_iters = job.number_of_iterations;
        let started_at = Instant::now();

        let started_line = format_job_status_line(
            height,
            field_vdf,
            "In progress...",
            Some((0, total_iters)),
            started_at.elapsed(),
            false,
        );
        print_job_line(&started_line, output_mode);

        let submit_res = run_job_until_submitted(
            shutdown.clone(),
            &http,
            &cli.backend_url,
            lease.lease_id,
            lease.lease_expires_at,
            job,
            cli.discriminant_bits,
            output_mode,
            started_at,
        )
        .await;

        match submit_res {
            Ok(Some(done_line)) => {
                complete_job_line(&done_line, output_mode);
            }
            Ok(None) => {
                let done_line = format_job_status_line(
                    height,
                    field_vdf,
                    "Canceled",
                    None,
                    started_at.elapsed(),
                    true,
                );
                complete_job_line(&done_line, output_mode);
                break;
            }
            Err(err) => {
                let field = field_vdf_label(field_vdf);
                print_error_line(
                    &format!(
                        "Block {height} ({field}) error: {err:#} (after {})",
                        format_duration(started_at.elapsed())
                    ),
                    output_mode,
                    None,
                );
                let done_line = format_job_status_line(
                    height,
                    field_vdf,
                    "Error",
                    None,
                    started_at.elapsed(),
                    true,
                );
                complete_job_line(&done_line, output_mode);
            }
        }

        if shutdown.should_exit_graceful() {
            break;
        }
    }

    Ok(())
}

#[derive(Debug)]
struct ShutdownController {
    graceful: AtomicBool,
    forced: AtomicU8,
}

impl ShutdownController {
    fn new() -> Self {
        Self {
            graceful: AtomicBool::new(false),
            forced: AtomicU8::new(0),
        }
    }

    fn request_graceful(&self) {
        self.graceful.store(true, Ordering::SeqCst);
    }

    fn should_exit_graceful(&self) -> bool {
        self.graceful.load(Ordering::SeqCst)
    }

    fn bump_forced(&self) -> u8 {
        self.forced.fetch_add(1, Ordering::SeqCst) + 1
    }
}

fn spawn_ctrl_c_handler(shutdown: Arc<ShutdownController>) {
    tokio::spawn(async move {
        loop {
            if tokio::signal::ctrl_c().await.is_err() {
                return;
            }
            let n = shutdown.bump_forced();
            if n == 1 {
                shutdown.request_graceful();
                eprintln!(
                    "CTRL+C: finishing current job then exiting. Press CTRL+C again to exit immediately."
                );
            } else {
                eprintln!("CTRL+C: exiting immediately.");
                std::process::exit(130);
            }
        }
    });
}

async fn lease_one_job(
    http: &reqwest::Client,
    backend: &Url,
    worker_id: &str,
) -> anyhow::Result<LeaseResponse> {
    let url = backend.join("api/jobs/lease")?;
    let res = http
        .post(url)
        .json(&LeaseRequest {
            count: 1,
            worker_id: Some(worker_id.to_string()),
        })
        .send()
        .await?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        anyhow::bail!("http {status}: {body}");
    }
    Ok(res.json().await?)
}

async fn submit_job(
    http: &reqwest::Client,
    backend: &Url,
    job_id: u64,
    lease_id: &str,
    witness: &[u8],
) -> anyhow::Result<SubmitResponse> {
    let url = backend.join(&format!("api/jobs/{job_id}/submit"))?;
    let res = http
        .post(url)
        .json(&SubmitRequest {
            lease_id: lease_id.to_string(),
            witness_b64: B64.encode(witness),
        })
        .send()
        .await?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        anyhow::bail!("http {status}: {body}");
    }
    Ok(res.json().await?)
}

async fn run_job_until_submitted(
    shutdown: Arc<ShutdownController>,
    http: &reqwest::Client,
    backend: &Url,
    lease_id: String,
    lease_expires_at: i64,
    job: JobDto,
    discriminant_bits: usize,
    output_mode: OutputMode,
    started_at: Instant,
) -> anyhow::Result<Option<String>> {
    const PROGRESS_STEP_ITERS: u64 = 1_000_000;

    let output = B64
        .decode(job.output_b64.as_bytes())
        .context("decode output_b64")?;
    let challenge = B64
        .decode(job.challenge_b64.as_bytes())
        .context("decode challenge_b64")?;

    let mut last_compute_err: Option<String> = None;
    let (witness, output_mismatch) = loop {
        if shutdown.should_exit_graceful() {
            // graceful exit: still complete this job.
        }

        let now = chrono::Utc::now().timestamp();
        if now >= lease_expires_at {
            anyhow::bail!("lease expired before completion");
        }

        let compute = tokio::task::spawn_blocking({
            let challenge = challenge.clone();
            let output = output.clone();
            let height = job.height;
            let field_vdf = job.field_vdf;
            let total_iters = job.number_of_iterations;
            let started_at = started_at;
            let output_mode = output_mode;
            move || -> anyhow::Result<(Vec<u8>, bool)> {
                let x = default_classgroup_element();
                let out = prove_one_weso_fast_with_progress(
                    &challenge,
                    &x,
                    discriminant_bits,
                    total_iters,
                    PROGRESS_STEP_ITERS,
                    move |iters_done| {
                        if output_mode != OutputMode::Quiet {
                            let line = format_job_status_line(
                                height,
                                field_vdf,
                                "In progress...",
                                Some((iters_done, total_iters)),
                                started_at.elapsed(),
                                false,
                            );
                            print_job_line(&line, output_mode);
                        }
                    },
                )
                .context("chiavdf prove_one_weso_fast_with_progress")?;
                let half = out.len() / 2;
                let y = &out[..half];
                let witness = out[half..].to_vec();
                Ok((witness, y != output))
            }
        })
        .await
        .context("join compute task")?;

        let (witness, output_mismatch) = match compute {
            Ok(v) => v,
            Err(err) => {
                let status_line = format_job_status_line(
                    job.height,
                    job.field_vdf,
                    "Compute failed, retrying...",
                    None,
                    started_at.elapsed(),
                    false,
                );

                print_job_line(&status_line, output_mode);

                let err_msg = format!("{err:#}");
                if last_compute_err.as_deref() != Some(&err_msg) {
                    last_compute_err = Some(err_msg.clone());
                    let field = field_vdf_label(job.field_vdf);
                    print_error_line(
                        &format!(
                            "Block {} ({field}) compute error: {err_msg} (after {})",
                            job.height,
                            format_duration(started_at.elapsed())
                        ),
                        output_mode,
                        Some(&status_line),
                    );
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };

        break (witness, output_mismatch);
    };

    let mut last_submit_err: Option<String> = None;
    loop {
        let now = chrono::Utc::now().timestamp();
        if now >= lease_expires_at {
            anyhow::bail!("lease expired before completion");
        }

        if output_mode != OutputMode::Quiet {
            let status = if output_mismatch {
                "Submitting... (output mismatch)"
            } else {
                "Submitting..."
            };
            let line = format_job_status_line(
                job.height,
                job.field_vdf,
                status,
                Some((job.number_of_iterations, job.number_of_iterations)),
                started_at.elapsed(),
                false,
            );
            print_job_line(&line, output_mode);
        }

        match submit_job(http, backend, job.job_id, &lease_id, &witness).await {
            Ok(res) => {
                let mut status = humanize_submit_reason(&res.reason);
                if let Some(id) = res.accepted_event_id {
                    status.push_str(&format!(" (event {id})"));
                }
                if !res.detail.is_empty() && res.detail != res.reason {
                    status.push_str(&format!(" ({})", res.detail));
                }
                let line =
                    format_job_status_line(job.height, job.field_vdf, &status, None, started_at.elapsed(), true);
                return Ok(Some(line));
            }
            Err(err) => {
                let status_line = format_job_status_line(
                    job.height,
                    job.field_vdf,
                    "Submit failed, retrying...",
                    Some((job.number_of_iterations, job.number_of_iterations)),
                    started_at.elapsed(),
                    false,
                );

                print_job_line(&status_line, output_mode);

                let err_msg = format!("{err:#}");
                if last_submit_err.as_deref() != Some(&err_msg) {
                    last_submit_err = Some(err_msg.clone());
                    let field = field_vdf_label(job.field_vdf);
                    print_error_line(
                        &format!(
                            "Block {} ({field}) submit error: {err_msg} (after {})",
                            job.height,
                            format_duration(started_at.elapsed())
                        ),
                        output_mode,
                        Some(&status_line),
                    );
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        }
    }
}
