//! Process event source.
//!
//! Design constraint from Phase 0: the event source is *eBPF only* — a single
//! `sched_process_exec` tracepoint, nothing else. Fork/exit are deliberately
//! not subscribed (ananicy-cpp receives ~3 netlink messages per spawn and pays
//! for all of them; see `docs/BENCHMARKS.md`). Events must be decoded with no
//! heap allocation in the steady state.

/// One exec event: the pid that executed, plus the best-effort name hint from
/// the kernel (truncated comm). The authoritative match name is resolved by
/// the daemon (`basename(argv[0])`, exe fallback) only when the hint cannot be
/// ruled out by the matcher's prescreen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecEvent {
    pub pid: u32,
    /// Up to 15 usable bytes from the kernel (TASK_COMM_LEN-limited).
    pub comm: [u8; 16],
    pub comm_len: u8,
}

impl ExecEvent {
    pub fn comm_bytes(&self) -> &[u8] {
        &self.comm[..self.comm_len as usize]
    }
}

/// Abstraction over the kernel event stream so the daemon can be tested
/// without eBPF and so a future source can be swapped in without touching
/// the worker.
pub trait ExecSource: Send {
    /// Blocks until the next exec event or the source is shut down.
    /// Returns `Ok(None)` on clean shutdown.
    fn next_event(&self) -> std::io::Result<Option<ExecEvent>>;

    /// Wake a blocked `next_event` from another thread (shutdown path).
    fn wake(&self);
}

/// Errors that must be surfaced loudly at startup: the daemon is eBPF-only by
/// decision, so a missing BTF/tracepoint capability is a hard failure with a
/// clear message — never a silent fallback.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("kernel BTF not available at /sys/kernel/btf/vmlinux; aranicy requires a BTF-enabled kernel (>= 5.8)")]
    NoBtf,
    #[error("failed to load sched_process_exec eBPF program: {0}")]
    LoadFailed(String),
    #[error("failed to attach sched_process_exec tracepoint: {0}")]
    AttachFailed(String),
}

pub fn btf_available() -> bool {
    std::path::Path::new("/sys/kernel/btf/vmlinux").exists()
}

mod raw;
mod source;

pub use source::BpfExecSource;
