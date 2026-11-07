//! aranicy-collectord — scheduler-agnostic per-task statistics collector.
//!
//! Owns every BPF program aranicy needs and publishes the per-task sample
//! stream over the `scx_stats` protocol, so bias learning no longer depends
//! on a particular sched_ext scheduler (or on one being loaded at all).

mod agg;
mod bpf;
mod events;
mod stats_server;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use agg::Aggregator;
use bpf::StatsCollector;
use events::{EventQueue, bind as bind_events, serve as serve_events};

/// Default socket location, matching the systemd unit's RuntimeDirectory.
pub const DEFAULT_SOCKET: &str = "/run/aranicy/collector-stats.sock";
pub const DEFAULT_CADENCE_MS: u64 = 500;
pub const DEFAULT_SAMPLE_CAP: usize = 512;
/// Exec-event socket, next to the stats socket in the runtime dir.
pub const DEFAULT_EVENTS_SOCKET: &str = "/run/aranicy/collector-events.sock";

#[derive(Parser, Debug)]
#[command(name = "aranicy-collectord", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Serve per-task samples on a UNIX socket until terminated.
    Run {
        /// UNIX socket path (scx_stats protocol).
        #[arg(long, default_value = DEFAULT_SOCKET)]
        socket: PathBuf,
        /// Exec-event socket path (framed push protocol).
        #[arg(long, default_value = DEFAULT_EVENTS_SOCKET)]
        events_socket: PathBuf,
        /// Drain cadence in milliseconds.
        #[arg(long, default_value_t = DEFAULT_CADENCE_MS)]
        cadence_ms: u64,
        /// Maximum rows returned per request.
        #[arg(long, default_value_t = DEFAULT_SAMPLE_CAP)]
        sample_cap: usize,
    },
    /// Attach, print rows to stdout, and exit (diagnostics; needs root).
    Probe {
        #[arg(long, default_value_t = 1000)]
        interval_ms: u64,
        #[arg(long, default_value_t = 3)]
        iterations: u64,
        #[arg(long, default_value_t = 8)]
        rows: usize,
    },
}

fn now_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // CLOCK_MONOTONIC is what bpf_ktime_get_ns() reports.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64)
}

fn init_tracing() {
    let filter = EnvFilter::try_from_env("ARANICY_COLLECTOR_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

fn main() -> Result<()> {
    init_tracing();
    match Cli::parse().cmd {
        Cmd::Run {
            socket,
            events_socket,
            cadence_ms,
            sample_cap,
        } => run(
            &socket,
            &events_socket,
            cadence_ms.max(50),
            sample_cap.max(1),
        ),
        Cmd::Probe {
            interval_ms,
            iterations,
            rows,
        } => probe(interval_ms.max(50), iterations.max(1), rows.max(1)),
    }
}

fn run(
    socket: &std::path::Path,
    events_socket: &std::path::Path,
    cadence_ms: u64,
    sample_cap: usize,
) -> Result<()> {
    // Before ANY thread exists: the mask is inherited, so a SIGTERM goes to
    // `sigwait` instead of hitting a worker and dying with the default
    // action (which would leave the sockets behind).
    block_termination_signals();

    let queue = EventQueue::new();
    let collector =
        StatsCollector::attach(queue.clone()).context("attaching collector BPF programs")?;
    let rows = Arc::new(Mutex::new(Vec::new()));

    let stop = Arc::new(AtomicBool::new(false));

    // One thread owns the BPF object: it polls exec records every 50 ms and,
    // on the drain cadence, snapshots the counters and refreshes the window.
    let worker = {
        let rows = Arc::clone(&rows);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut agg = Aggregator::new();
            let mut last_drain = std::time::Instant::now();
            while !stop.load(Ordering::Acquire) {
                if let Err(e) = collector.poll_events(Duration::from_millis(50)) {
                    tracing::warn!(error = %e, "exec ring buffer poll failed");
                }
                if last_drain.elapsed() < Duration::from_millis(cadence_ms) {
                    continue;
                }
                last_drain = std::time::Instant::now();
                let snap = match collector.snapshot() {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(error = %e, "snapshot failed; keeping the previous window");
                        continue;
                    }
                };
                let mut window = agg.ingest(snap, now_ns());
                Aggregator::order(&mut window, sample_cap);
                *rows.lock().unwrap_or_else(|e| e.into_inner()) = window;
            }
        })
    };

    // Bind before spawning: a collector without an exec-event socket must not
    // report itself healthy while the daemon keeps failing to start.
    let events_listener = bind_events(events_socket)?;
    let events_thread = {
        let stop = Arc::clone(&stop);
        let path = events_socket.to_path_buf();
        std::thread::spawn(move || {
            if let Err(e) = serve_events(events_listener, &path, queue, stop) {
                tracing::error!(error = %e, "exec-event server stopped");
            }
        })
    };

    let _server = stats_server::serve(socket, Arc::clone(&rows), sample_cap)?;
    tracing::info!(
        socket = %socket.display(),
        events_socket = %events_socket.display(),
        cadence_ms,
        "collector serving sched_samples + exec events"
    );

    wait_for_termination();
    stop.store(true, Ordering::Release);
    let _ = worker.join();
    let _ = events_thread.join();
    Ok(())
}

fn probe(interval_ms: u64, iterations: u64, limit: usize) -> Result<()> {
    let collector = StatsCollector::attach(EventQueue::new())?;
    let mut agg = Aggregator::new();
    for _ in 0..iterations {
        std::thread::sleep(Duration::from_millis(interval_ms));
        let snap = collector.snapshot()?;
        let mut window = agg.ingest(snap, now_ns());
        Aggregator::order(&mut window, limit);
        println!("--- {} rows", window.len());
        for s in &window {
            println!(
                "pid={:<7} comm={:<16} prio={:<3} wait={:<6} wake={:<6} rt={:<12} demand={:<5} deliv={:<5}",
                s.pid, s.comm, s.static_prio, s.wait_freq, s.wake_freq, s.avg_runtime_wall,
                s.task_util_est, s.cpu_util_wall
            );
        }
    }
    Ok(())
}

/// Block SIGTERM/SIGINT for the whole process (inherited by every thread).
fn block_termination_signals() {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
}

/// Block until SIGTERM/SIGINT. Signals are blocked process-wide (see
/// [`block_termination_signals`]) and consumed by this thread only.
fn wait_for_termination() {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGINT);
        let mut sig: libc::c_int = 0;
        libc::sigwait(&set, &mut sig);
    }
}
