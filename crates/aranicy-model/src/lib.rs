//! Rule policy model.
//!
//! [`Policy`] is *flattened at load time*: type inheritance and `null`
//! suppression are resolved once by the rules loader, so the per-event hot
//! path only performs a hash lookup and reads small fields — no merges, no
//! JSON, no allocations. (Phase 0 root-cause analysis: see
//! `docs/BENCHMARKS.md`.)

use std::sync::Arc;

/// CPU scheduling policy (`sched` rule field).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedPolicy {
    Fifo,
    Rr,
    Normal,
    Batch,
    Idle,
}

/// I/O scheduling class (`ioclass` rule field).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoClass {
    BestEffort,
    Realtime,
    Idle,
    None,
}

/// CPU affinity (`cpuset` rule field): an explicit list/range string
/// (`0-3,8-11`) or a named alias (`big-cores`, `llc-N`, ...). Alias name
/// tables live in `aranicy-apply`, not here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CpuSet {
    Explicit(Arc<str>),
    Alias(Arc<str>),
}

/// Fully resolved policy for one process name. Built once at rules load.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Policy {
    pub nice: Option<i8>,
    /// Falls back to `nice` at apply time when absent (ananicy-cpp parity).
    pub latency_nice: Option<i8>,
    pub sched: Option<SchedPolicy>,
    /// RT priority for `Fifo`/`Rr`; defaults to 1 at apply time.
    pub rtprio: Option<u8>,
    pub ioclass: Option<IoClass>,
    /// Priority level 0..=7; meaningful for `BestEffort`/`Realtime`.
    pub ionice: Option<u8>,
    pub oom_score_adj: Option<i16>,
    pub cgroup: Option<Arc<str>>,
    pub cpuset: Option<CpuSet>,
    /// Autogroup nice (superset field; ananicy-cpp only reports it).
    pub autogroup: Option<i8>,
}

/// Definition of a cgroup created at startup (`*.cgroups` file entry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CgroupDef {
    pub name: Arc<str>,
    /// Percent of *all* CPUs (`CPUQuota` semantics), not of one core.
    pub cpu_quota_pct: Option<u8>,
}
