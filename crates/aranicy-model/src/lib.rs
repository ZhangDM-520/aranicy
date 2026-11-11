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

/// CPU indices are limited to 0..=1023 (glibc `CPU_SETSIZE`).
pub const MAX_CPUS: usize = 1024;

/// Set `cpu` in a `cpu_set_t`-layout mask, growing it as needed.
pub fn set_cpu(mask: &mut Vec<u64>, cpu: usize) {
    let word = cpu / 64;
    if mask.len() <= word {
        mask.resize(word + 1, 0);
    }
    mask[word] |= 1 << (cpu % 64);
}

/// Parse `"0-3,8-11"` / `"0,2,4"` into a `cpu_set_t`-layout mask (bit
/// `cpu % 64` of word `cpu / 64`). Empty items (trailing/double commas) are
/// tolerated; anything else must be `N` or `A-B` with A ≤ B < 1024.
///
/// Lives beside [`CpuSet`] because two layers validate the same syntax: the
/// loader (so `lint` can reject a rule) and the apply path (which needs the
/// mask). Alias *resolution* reads sysfs and stays in `aranicy-apply`.
pub fn parse_cpu_list(s: &str) -> Result<Vec<u64>, String> {
    let mut mask = Vec::new();
    let mut seen = false;
    for item in s.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        seen = true;
        match item.split_once('-') {
            Some((lo, hi)) => {
                let lo: usize = lo
                    .trim()
                    .parse()
                    .map_err(|_| format!("invalid cpu range '{item}'"))?;
                let hi: usize = hi
                    .trim()
                    .parse()
                    .map_err(|_| format!("invalid cpu range '{item}'"))?;
                if lo > hi {
                    return Err(format!("invalid cpu range '{item}': start above end"));
                }
                if hi >= MAX_CPUS {
                    return Err(format!("cpu index {hi} out of range 0..={}", MAX_CPUS - 1));
                }
                for cpu in lo..=hi {
                    set_cpu(&mut mask, cpu);
                }
            }
            None => {
                let cpu: usize = item
                    .parse()
                    .map_err(|_| format!("invalid cpu index '{item}'"))?;
                if cpu >= MAX_CPUS {
                    return Err(format!("cpu index {cpu} out of range 0..={}", MAX_CPUS - 1));
                }
                set_cpu(&mut mask, cpu);
            }
        }
    }
    if !seen {
        return Err("empty cpu list".to_string());
    }
    Ok(mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cpu_list_ranges_to_bits() {
        // "0-3,8-11" -> bits 0..3 and 8..11 = 0x0f0f
        assert_eq!(parse_cpu_list("0-3,8-11").unwrap(), vec![0x0f0f]);
        assert_eq!(parse_cpu_list("0,2,4").unwrap(), vec![0x15]);
        // Words beyond the first follow cpu_set_t layout.
        assert_eq!(parse_cpu_list("64-65").unwrap(), vec![0, 0b11]);
    }

    #[test]
    fn parse_cpu_list_errors() {
        assert!(parse_cpu_list("").is_err());
        assert!(parse_cpu_list("a").is_err());
        assert!(parse_cpu_list("4-2").is_err());
        assert!(parse_cpu_list("1024").is_err());
        assert!(parse_cpu_list("0-1024").is_err());
    }
}
