//! Bias history persistence (see plan.md 4.3).
//!
//! Plain save/load over serde_json: atomic (tmp + rename), tolerant on load
//! only for absence (missing file yields `Ok(None)`; corrupt content is an
//! `Err` so the caller can log it and start fresh). The caller debounces
//! writes (>= 60 s). `last_seen` is unix seconds so confidence aging works
//! across restarts.

#![allow(dead_code)] // save/load are wired into daemon.rs by the caller

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::Path;

/// Snapshot schema version. Unknown JSON fields are ignored and missing
/// fields default, so older/newer files still load best-effort.
pub const BIAS_STATE_VERSION: u32 = 1;

fn default_version() -> u32 {
    BIAS_STATE_VERSION
}

/// Estimator class per name. `Unknown` until the hysteresis machine has two
/// consecutive classifying windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BiasClass {
    #[default]
    Unknown,
    Bulk,
    Responsive,
}

/// One persisted per-name history entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct EntrySnapshot {
    pub name: String,
    /// EWMA of the name's per-window weight-free score.
    pub score_ema: f64,
    /// EWMA of the name's per-window sample counts.
    pub sample_count_ewma: f64,
    pub class: BiasClass,
    pub bias: i8,
    /// Unix seconds of the last observed sample (drives aging on restore).
    pub last_seen: u64,
    /// 0.0..=1.0 evidence confidence.
    pub confidence: f64,
    /// Closed-loop mismatch freeze (restored entries keep the re-baseline flag).
    pub frozen: bool,
}

/// Full persisted bias state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct BiasSnapshot {
    #[serde(default = "default_version")]
    pub version: u32,
    /// Population threshold T_us (userspace EWMA of mean score), if known.
    pub threshold_ema: Option<f64>,
    pub entries: Vec<EntrySnapshot>,
}

/// Atomically persist `state` (write tmp file + rename). Missing parent
/// directories are created: the state path lives under `/var/lib/aranicy/`
/// which a non-systemd start may not have (ENOENT there once made every
/// save fail and `dump bias` blind).
pub fn save(state: &BiasSnapshot, path: &Path) -> io::Result<()> {
    let bytes = serde_json::to_vec(state)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            fs::create_dir_all(dir)?;
        }
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    {
        use std::io::Write;
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

/// Load persisted state. `Ok(None)` only for a missing file; a corrupt or
/// unparseable file is an `Err` (the caller logs the warning and starts
/// fresh — corruption must never be indistinguishable from absence).
pub fn load(path: &Path) -> io::Result<Option<BiasSnapshot>> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        // Tests must not touch /tmp; use the package-local target/ tree and
        // clean up after ourselves.
        let dir = std::env::current_dir()
            .expect("cwd")
            .join("target")
            .join(format!("bias_state_test_{}_{}", std::process::id(), tag));
        fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn sample_snapshot() -> BiasSnapshot {
        BiasSnapshot {
            version: BIAS_STATE_VERSION,
            threshold_ema: Some(1234.5),
            entries: vec![
                EntrySnapshot {
                    name: "app".into(),
                    score_ema: 784.0,
                    sample_count_ewma: 32.0,
                    class: BiasClass::Responsive,
                    bias: -1,
                    last_seen: 1_760_000_000,
                    confidence: 0.75,
                    frozen: false,
                },
                EntrySnapshot {
                    name: "bulk".into(),
                    score_ema: 100.0,
                    sample_count_ewma: 41.5,
                    class: BiasClass::Bulk,
                    bias: 3,
                    last_seen: 1_760_000_060,
                    confidence: 0.25,
                    frozen: true,
                },
            ],
        }
    }

    #[test]
    fn round_trip() {
        let dir = scratch_dir("round_trip");
        let path = dir.join("state.json");
        let snap = sample_snapshot();
        save(&snap, &path).expect("save");
        let loaded = load(&path).expect("load").expect("present");
        assert_eq!(loaded, snap);
        // Save again over an existing file (tmp + rename must clobber cleanly).
        save(&snap, &path).expect("resave");
        assert_eq!(load(&path).expect("load").expect("present"), snap);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_creates_missing_parent_dirs() {
        let dir = scratch_dir("mkdirs");
        let path = dir.join("nested").join("deeper").join("state.json");
        save(&sample_snapshot(), &path).expect("save must create parents");
        assert_eq!(load(&path).expect("load").expect("present"), sample_snapshot());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_is_none_and_corrupt_is_error() {
        let dir = scratch_dir("corrupt");
        assert!(load(&dir.join("absent.json")).expect("load").is_none());

        let path = dir.join("garbage.json");
        fs::write(&path, b"{ this is not json").expect("write garbage");
        let err = load(&path).expect_err("corrupt must surface as an error");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);

        fs::write(&path, b"").expect("write empty");
        assert!(load(&path).expect_err("empty is corrupt too").kind() == std::io::ErrorKind::InvalidData);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn version_tolerant_defaults() {
        // Unknown fields ignored, missing fields default, future version
        // still parses best-effort.
        let raw = br#"{"version": 99, "some_future_field": {"x": 1},
                       "entries": [{"name": "a", "bias": 2, "mystery": true}]}"#;
        let snap: BiasSnapshot = serde_json::from_slice(raw).expect("tolerant parse");
        assert_eq!(snap.version, 99);
        assert_eq!(snap.threshold_ema, None);
        assert_eq!(snap.entries.len(), 1);
        let e = &snap.entries[0];
        assert_eq!(e.name, "a");
        assert_eq!(e.bias, 2);
        assert_eq!(e.class, BiasClass::Unknown);
        assert_eq!(e.confidence, 0.0);
        assert!(!e.frozen);
    }
}
