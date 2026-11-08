//! Exec-event frame format shared by `aranicy-collectord` and `aranicy`.
//!
//! The collector captures exec events, resolves the match name at capture
//! time (see its `events` module) and pushes fixed-size frames; the daemon
//! decodes them. Fixed width keeps the reader allocation-free and lets a
//! partial frame be detected with `read_exact`.
//!
//! ```text
//! frame (280 bytes, native endian):
//!   u32 pid | u8 comm_len | u8 reserved | u16 name_len
//!   [u8; 16] comm | [u8; 256] name
//! ```
//!
//! `NAME_LEN` mirrors `aranicy_procname::NAME_CAP`: a shorter field would
//! truncate capture-time names (and can cut a UTF-8 sequence, which the
//! daemon would then drop as unresolved), so exec matching would disagree
//! with every other path.

use serde::{Deserialize, Serialize};

/// One full CPU in the normalized utilization scale (scx uses `1 << 10`).
pub const MAX_UTIL: u64 = 1024;

/// `static_prio` is the kernel nice shifted by this offset: `nice + 20`.
pub const NICE_OFFSET: u64 = 20;

/// Observation budget: the maximum number of per-task rows one sample window
/// may carry. Defined here once because both the collector (which cuts) and
/// the daemon (which requests) must agree on it.
/// Rows a single drain window may carry — the observation budget.
///
/// Measured on a 24-thread desktop: an idle window carries ~260 rows (a
/// 512 budget masked nothing), but a full `cargo build` reaches ~5 600
/// short-lived processes, where 512 hid ~91% of the evidence and even 4 096
/// truncated ~27%. The cap bounds a pathological window; it must sit above
/// the working set of the workload the bias exists for. Measured at 8 192
/// rows under a full rebuild: daemon ~0.3 %, collector ~0.6 % of a core, with
/// no truncation (at 4 096 the same workload truncated ~27 %).
pub const SAMPLE_CAP: usize = 8192;

/// One per-task row of a drain window — the single definition of the sample
/// contract, shared by the collector (producer) and the daemon (consumer).
///
/// Parsing is lenient by design: unknown fields are ignored and missing
/// fields default, so additive schema changes stay compatible. The consequence
/// is that a *missing* field is indistinguishable from a meaningful zero (a
/// `wait_freq` of 0 legitimately means "never slept").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sample {
    pub pid: i64,
    /// Kernel task name (16 bytes).
    pub comm: String,
    /// Kernel static priority: nice + 20 (see [`NICE_OFFSET`]).
    pub static_prio: u64,
    /// Sleeps per second over the window (EWMA). 0 is a real measurement.
    pub wait_freq: u64,
    /// Wakes issued to other tasks per second (EWMA). 0 is a real
    /// measurement: a compiler forks its children, it does not wake them.
    pub wake_freq: u64,
    /// Mean nanoseconds per schedule. 0 means "never ran" (no evidence).
    pub avg_runtime_wall: u64,
    /// DEMAND: running + runnable duty cycle, 0..=[`MAX_UTIL`] (one CPU at
    /// `MAX_UTIL`). Always `>= cpu_util_wall`.
    pub task_util_est: u64,
    /// DELIVERED: running duty cycle on the same 0..=[`MAX_UTIL`] scale.
    pub cpu_util_wall: u64,
}

pub const COMM_LEN: usize = 16;
pub const NAME_LEN: usize = 256;
pub const FRAME_LEN: usize = 4 + 1 + 1 + 2 + COMM_LEN + NAME_LEN;

/// One exec event as delivered to the daemon.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExecFrame {
    pub pid: u32,
    pub comm: [u8; COMM_LEN],
    pub comm_len: u8,
    /// Match name resolved at capture (`basename(argv[0])` → exe → comm).
    /// Empty when resolution failed; the daemon then falls back to comm.
    pub name: Vec<u8>,
}

impl ExecFrame {
    pub fn new(pid: u32, comm: [u8; COMM_LEN], comm_len: u8, name: &[u8]) -> Self {
        let mut n = name.to_vec();
        n.truncate(NAME_LEN);
        Self {
            pid,
            comm,
            comm_len: comm_len.min(COMM_LEN as u8),
            name: n,
        }
    }

    pub fn comm_bytes(&self) -> &[u8] {
        &self.comm[..self.comm_len as usize]
    }

    pub fn encode(&self) -> [u8; FRAME_LEN] {
        let mut out = [0u8; FRAME_LEN];
        out[0..4].copy_from_slice(&self.pid.to_ne_bytes());
        out[4] = self.comm_len;
        out[6..8].copy_from_slice(&(self.name.len() as u16).to_ne_bytes());
        out[8..8 + COMM_LEN].copy_from_slice(&self.comm);
        out[24..24 + self.name.len()].copy_from_slice(&self.name);
        out
    }

    pub fn decode(bytes: &[u8; FRAME_LEN]) -> Self {
        let mut comm = [0u8; COMM_LEN];
        comm.copy_from_slice(&bytes[8..8 + COMM_LEN]);
        let name_len =
            (u16::from_ne_bytes(bytes[6..8].try_into().expect("2 bytes")) as usize).min(NAME_LEN);
        Self {
            pid: u32::from_ne_bytes(bytes[0..4].try_into().expect("4 bytes")),
            comm,
            comm_len: bytes[4].min(COMM_LEN as u8),
            name: bytes[24..24 + name_len].to_vec(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trips() {
        let mut comm = [0u8; COMM_LEN];
        comm[..5].copy_from_slice(b"cc1pl");
        let f = ExecFrame::new(4242, comm, 5, b"cc1plus");
        let back = ExecFrame::decode(&f.encode());
        assert_eq!(back, f);
        assert_eq!(back.comm_bytes(), b"cc1pl");
        assert_eq!(back.name, b"cc1plus");
    }

    #[test]
    fn frame_truncates_long_names() {
        let long = [b'x'; NAME_LEN + 10];
        let f = ExecFrame::new(1, [0; COMM_LEN], 0, &long);
        assert_eq!(f.name.len(), NAME_LEN);
        assert_eq!(ExecFrame::decode(&f.encode()).name.len(), NAME_LEN);
    }

    #[test]
    fn frame_length_matches_layout() {
        assert_eq!(FRAME_LEN, 280);
    }

    #[test]
    fn empty_name_survives_the_round_trip() {
        let f = ExecFrame::new(9, [0; COMM_LEN], 0, b"");
        assert!(ExecFrame::decode(&f.encode()).name.is_empty());
    }

    #[test]
    fn sample_round_trips_json() {
        let s = Sample {
            pid: 4242,
            comm: "cc1plus".into(),
            static_prio: 20 + 3,
            wait_freq: 0,
            wake_freq: 0,
            avg_runtime_wall: 3_906_250,
            task_util_est: 900,
            cpu_util_wall: 700,
        };
        let json = serde_json::to_string(&s).expect("serialize");
        let back: Sample = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, s);
        assert_eq!(back.wait_freq, 0, "a meaningful zero survives the wire");
    }

    #[test]
    fn sample_tolerates_missing_and_unknown_fields() {
        let back: Sample =
            serde_json::from_str(r#"{"pid":7,"comm":"x","cpu_heat":19}"#).expect("lenient parse");
        assert_eq!(back.pid, 7);
        assert_eq!(back.comm, "x");
        assert_eq!(back.task_util_est, 0, "missing fields default");
    }
}
