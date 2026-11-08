//! `scx_stats` server exposing the bias engine's sample rows.
//!
//! The wire contract is the one aranicy already speaks (and sched_ext tools
//! like `scxtop` understand): request `{"req":"stats","args":{"target":
//! "sched_samples","nr_samples":"N"}}`, response
//! `{"errno":0,"args":{"resp":{"samples":[...]}}}`.
//!
//! Using the `scx_stats` crate is deliberate: the collector package is
//! GPL-2.0-only, so the protocol implementation stays byte-identical to the
//! sched_ext ecosystem instead of being reimplemented.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use scx_stats::prelude::*;
use scx_stats_derive::{Stats, stat_doc};
use serde::{Deserialize, Serialize};

use crate::agg::Sample;

/// One per-task row on the `scx_stats` wire. A thin adapter: the sample
/// contract (fields, units, scales) is [`aranicy_wire::Sample`], and this
/// struct exists only because the `Stats` derive must live in the GPL crate.
#[stat_doc]
#[derive(Clone, Debug, Default, Serialize, Deserialize, Stats)]
pub struct SchedSample {
    #[stat(desc = "Process ID")]
    pub pid: i64,
    #[stat(desc = "Task name (kernel comm)")]
    pub comm: String,
    #[stat(desc = "Static priority: nice + 20")]
    pub static_prio: u64,
    #[stat(desc = "Sleeps per second (EWMA; 0 is a real zero)")]
    pub wait_freq: u64,
    #[stat(desc = "Wakes issued per second (EWMA; 0 is a real zero)")]
    pub wake_freq: u64,
    #[stat(desc = "Mean nanoseconds per schedule")]
    pub avg_runtime_wall: u64,
    #[stat(desc = "Demand: running + runnable duty, 0..1024 (1024 == one CPU)")]
    pub task_util_est: u64,
    #[stat(desc = "Delivered: running duty, same 0..1024 scale")]
    pub cpu_util_wall: u64,
}

impl From<Sample> for SchedSample {
    fn from(s: Sample) -> Self {
        Self {
            pid: s.pid,
            comm: s.comm,
            static_prio: s.static_prio,
            wait_freq: s.wait_freq,
            wake_freq: s.wake_freq,
            avg_runtime_wall: s.avg_runtime_wall,
            task_util_est: s.task_util_est,
            cpu_util_wall: s.cpu_util_wall,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, Stats)]
pub struct SchedSamples {
    pub samples: Vec<SchedSample>,
    /// Rows the observation budget dropped (aggregation cut + this request's
    /// `nr_samples`). Additive and defaulted so an old client still parses.
    #[serde(default)]
    pub truncated: u32,
}

/// The latest window shared between the drain thread and the stats server.
#[derive(Clone, Debug, Default)]
pub struct Window {
    /// Already ordered and cut to the collector's sample cap.
    pub rows: Vec<Sample>,
    /// Rows `Aggregator::order` dropped while producing `rows`.
    pub truncated: u32,
}

/// Shared latest-window rows, refreshed by the drain thread.
pub type SharedRows = Arc<Mutex<Window>>;

/// Requested `nr_samples`, clamped to `[1, cap]`; defaults to `cap`.
fn requested_cap(args: &std::collections::BTreeMap<String, String>, cap: usize) -> usize {
    args.get("nr_samples")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(cap)
        .clamp(1, cap)
}

/// Launch the stats server on `path` and return it (keep it alive).
pub fn serve(path: &Path, rows: SharedRows, cap: usize) -> Result<StatsServer<(), ()>> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create socket dir {}", dir.display()))?;
    }
    let data = StatsServerData::<(), ()>::new()
        .add_meta(SchedSample::meta())
        .add_stats(
            "sched_samples",
            Box::new(move |args, _chan| {
                let cap = requested_cap(args, cap);
                let window = rows.lock().unwrap_or_else(|e| e.into_inner());
                let served = cap.min(window.rows.len());
                let samples: Vec<SchedSample> = window
                    .rows
                    .iter()
                    .take(served)
                    .cloned()
                    .map(SchedSample::from)
                    .collect();
                // Both cuts are part of the same budget: rows dropped by the
                // aggregation step plus rows this request's `nr_samples` cut.
                let truncated = window
                    .truncated
                    .saturating_add(u32::try_from(window.rows.len() - served).unwrap_or(u32::MAX));
                SchedSamples { samples, truncated }.to_json()
            }),
        );

    StatsServer::new(data)
        .set_path(path)
        .launch()
        .with_context(|| format!("failed to serve stats on {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requested_cap_parses_and_clamps() {
        use std::collections::BTreeMap;
        let mut args = BTreeMap::new();
        args.insert("target".to_string(), "sched_samples".to_string());
        assert_eq!(requested_cap(&args, 512), 512);

        args.insert("nr_samples".to_string(), "200".to_string());
        assert_eq!(requested_cap(&args, 512), 200);

        args.insert("nr_samples".to_string(), "99999".to_string());
        assert_eq!(requested_cap(&args, 512), 512, "never above the cap");

        args.insert("nr_samples".to_string(), "junk".to_string());
        assert_eq!(requested_cap(&args, 512), 512);
    }

    #[test]
    fn wire_row_keeps_the_contract_fields() {
        let s = Sample {
            pid: 42,
            comm: "build".into(),
            static_prio: 23,
            wait_freq: 10,
            wake_freq: 4,
            avg_runtime_wall: 1_000_000,
            task_util_est: 1024,
            cpu_util_wall: 700,
        };
        let w = SchedSample::from(s);
        assert_eq!(w.pid, 42);
        assert_eq!(w.static_prio, 23);
        assert_eq!(w.task_util_est, 1024);
        assert_eq!(w.cpu_util_wall, 700);
    }

    #[test]
    fn truncated_field_serializes_and_old_payloads_default_to_zero() {
        let payload = SchedSamples {
            samples: Vec::new(),
            truncated: 7,
        };
        let json = payload.to_json().expect("serialize");
        assert_eq!(json["truncated"], 7);

        let old: SchedSamples =
            serde_json::from_str(r#"{"samples":[]}"#).expect("old payload still parses");
        assert_eq!(old.truncated, 0);
    }
}
