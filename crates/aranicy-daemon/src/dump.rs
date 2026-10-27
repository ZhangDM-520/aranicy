//! `aranicy dump` — JSON views over loaded state and live `/proc` (unprivileged).
//!
//! `Policy` has no `Serialize` (model stays dependency-free), so every shape
//! is built manually with `serde_json::json!`. Only present (`Some`) policy
//! fields are emitted.

use aranicy_model::{CpuSet, IoClass, Policy, SchedPolicy};
use aranicy_rules::RuleSet;
use serde_json::{json, Map, Value};

use crate::bias_state;
use crate::config::Config;
use crate::load;
use crate::paths::Paths;
use crate::procname;

fn sched_str(s: SchedPolicy) -> &'static str {
    match s {
        SchedPolicy::Fifo => "fifo",
        SchedPolicy::Rr => "rr",
        SchedPolicy::Normal => "normal",
        SchedPolicy::Batch => "batch",
        SchedPolicy::Idle => "idle",
    }
}

fn ioclass_str(c: IoClass) -> &'static str {
    match c {
        IoClass::BestEffort => "best-effort",
        IoClass::Realtime => "realtime",
        IoClass::Idle => "idle",
        IoClass::None => "none",
    }
}

/// Insert the policy's present fields into `map`.
fn policy_into(map: &mut Map<String, Value>, p: &Policy) {
    fn put(map: &mut Map<String, Value>, key: &str, val: Value) {
        map.insert(key.to_string(), val);
    }
    if let Some(v) = p.nice {
        put(map, "nice", json!(v));
    }
    if let Some(v) = p.latency_nice {
        put(map, "latency_nice", json!(v));
    }
    if let Some(s) = p.sched {
        put(map, "sched", json!(sched_str(s)));
    }
    if let Some(v) = p.rtprio {
        put(map, "rtprio", json!(v));
    }
    if let Some(c) = p.ioclass {
        put(map, "ioclass", json!(ioclass_str(c)));
    }
    if let Some(v) = p.ionice {
        put(map, "ionice", json!(v));
    }
    if let Some(v) = p.oom_score_adj {
        put(map, "oom_score_adj", json!(v));
    }
    if let Some(c) = &p.cgroup {
        put(map, "cgroup", json!(&**c));
    }
    if let Some(cs) = &p.cpuset {
        let (kind, value) = match cs {
            CpuSet::Explicit(s) => ("explicit", s),
            CpuSet::Alias(s) => ("alias", s),
        };
        put(map, "cpuset", json!({ "kind": kind, "value": &**value }));
    }
    if let Some(v) = p.autogroup {
        put(map, "autogroup", json!(v));
    }
}

/// The resolved policy as a JSON object (absent fields omitted).
pub fn policy_json(p: &Policy) -> Value {
    let mut map = Map::new();
    policy_into(&mut map, p);
    Value::Object(map)
}

fn rule_json(name: &str, pattern: Option<&str>, kind: &str, p: &Policy) -> Value {
    let mut map = Map::new();
    map.insert("name".into(), json!(name));
    map.insert("match".into(), json!(kind));
    if let Some(pattern) = pattern {
        map.insert("pattern".into(), json!(pattern));
    }
    policy_into(&mut map, p);
    Value::Object(map)
}

/// `{"exact": [...], "regex": [...]}` with deterministic ordering.
pub fn rules_json(rs: &RuleSet) -> Value {
    let mut exact: Vec<Value> = rs
        .exact
        .iter()
        .map(|(name, policy)| rule_json(name, None, "exact", policy))
        .collect();
    exact.sort_by_key(|v| v["name"].as_str().unwrap_or("").to_string());
    let mut regex: Vec<Value> = rs
        .regexes
        .iter()
        .map(|(re, label, policy)| rule_json(label, Some(re.as_str()), "regex", policy))
        .collect();
    regex.sort_by_key(|v| v["name"].as_str().unwrap_or("").to_string());
    json!({ "exact": exact, "regex": regex })
}

/// `{ "<type>": {<policy fields>}, ... }`.
pub fn types_json(rs: &RuleSet) -> Value {
    let map: Map<String, Value> = rs
        .types
        .iter()
        .map(|(name, policy)| {
            let mut obj = Map::new();
            policy_into(&mut obj, policy);
            (name.to_string(), Value::Object(obj))
        })
        .collect();
    Value::Object(map)
}

/// `{ "<cgroup>": {"cpu_quota_pct": N|null}, ... }`.
pub fn cgroups_json(rs: &RuleSet) -> Value {
    let map: Map<String, Value> = rs
        .cgroups
        .iter()
        .map(|(name, def)| {
            (
                name.to_string(),
                json!({ "cpu_quota_pct": def.cpu_quota_pct }),
            )
        })
        .collect();
    Value::Object(map)
}

/// `[{"pid", "name", "nice"}, ...]` over live `/proc`.
pub fn proc_json() -> Value {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Value::Array(out);
    };
    for entry in entries.flatten() {
        let Some(pid) = procname::entry_pid(&entry.file_name()) else {
            continue;
        };
        let Some((name, nice)) = procname::derive_name_for_pid(pid) else {
            continue;
        };
        let name = String::from_utf8_lossy(&name).into_owned();
        out.push(json!({ "pid": pid, "name": name, "nice": nice }));
    }
    out.sort_by_key(|v| v["pid"].as_u64().unwrap_or(0));
    Value::Array(out)
}

/// `{"<pid>": "<autogroup content>", ...}` over live `/proc`.
pub fn autogroup_json() -> Value {
    let mut map = Map::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Value::Object(map);
    };
    for entry in entries.flatten() {
        let Some(pid) = procname::entry_pid(&entry.file_name()) else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(format!("/proc/{pid}/autogroup")) else {
            continue;
        };
        map.insert(pid.to_string(), json!(content.trim_end_matches('\n')));
    }
    Value::Object(map)
}

/// `{"path", "note", "state"}` over the persisted bias snapshot.
///
/// The daemon saves the snapshot debounced (>= 60 s cadence), so on-disk state
/// can lag the live engine; the emitted `note` says so. Tolerant like
/// `bias_state::load`: a missing or corrupt file yields `"state": null`, and a
/// hard IO error (e.g. EACCES unprivileged) is reported in `"error"` instead
/// of failing the dump.
pub fn bias_json(paths: &Paths) -> anyhow::Result<Value> {
    let cfg = Config::load(&paths.config)?;
    Ok(bias_json_at(std::path::Path::new(&cfg.bias_state_path)))
}

fn bias_json_at(path: &std::path::Path) -> Value {
    let (state, error) = match bias_state::load(path) {
        Ok(state) => (state, Value::Null),
        Err(e) => (None, json!(e.to_string())),
    };
    json!({
        "path": path.display().to_string(),
        "note": "on-disk snapshot is debounced; live bias may be up to ~60 s newer",
        "state": state,
        "error": error,
    })
}

/// `aranicy dump <what>` — JSON to stdout. `rules`/`types`/`cgroups` load the
/// rule tree (honoring the config gates); `proc`/`autogroup` touch only
/// `/proc`; `bias` reads the persisted bias snapshot (config only).
pub fn run(what: &str, paths: &Paths) -> anyhow::Result<()> {
    let value = match what {
        "proc" => proc_json(),
        "autogroup" => autogroup_json(),
        "bias" => bias_json(paths)?,
        "rules" | "types" | "cgroups" => {
            let (_cfg, rules) = load::load_all(paths)?;
            match what {
                "rules" => rules_json(&rules),
                "types" => types_json(&rules),
                _ => cgroups_json(&rules),
            }
        }
        other => anyhow::bail!("unknown dump target {other:?}"),
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aranicy_model::CgroupDef;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        // Tests must not touch /tmp; use the package-local target/ tree.
        let dir = std::env::current_dir()
            .expect("cwd")
            .join("target")
            .join(format!("dump_test_{}_{}", std::process::id(), tag));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn synthetic_ruleset() -> RuleSet {
        let mut rs = RuleSet::empty();
        let foo = Policy {
            nice: Some(5),
            sched: Some(SchedPolicy::Batch),
            rtprio: None,
            cgroup: Some(Arc::from("games")),
            ..Policy::default()
        };
        let bar = Policy {
            nice: Some(-5),
            latency_nice: Some(2),
            cpuset: Some(CpuSet::Alias(Arc::from("big-cores"))),
            autogroup: Some(-3),
            ..Policy::default()
        };
        rs.exact.insert(Arc::from("foo"), Arc::new(foo));
        rs.regexes.push((
            regex::Regex::new("ba.*").unwrap(),
            Arc::from("bar-rule"),
            Arc::new(bar),
        ));
        rs.types.insert(
            Arc::from("game"),
            Arc::new(Policy {
                nice: Some(2),
                ..Policy::default()
            }),
        );
        let mut cgroups: BTreeMap<Arc<str>, CgroupDef> = BTreeMap::new();
        cgroups.insert(
            Arc::from("games"),
            CgroupDef {
                name: Arc::from("games"),
                cpu_quota_pct: Some(50),
            },
        );
        rs.cgroups = cgroups;
        rs
    }

    #[test]
    fn policy_json_omits_absent_fields() {
        let p = Policy {
            nice: Some(5),
            ioclass: Some(IoClass::BestEffort),
            ionice: Some(4),
            ..Policy::default()
        };
        let v = policy_json(&p);
        assert_eq!(v["nice"], json!(5));
        assert_eq!(v["ioclass"], json!("best-effort"));
        assert_eq!(v["ionice"], json!(4));
        assert!(v.get("sched").is_none());
        assert!(v.get("cgroup").is_none());
    }

    #[test]
    fn policy_json_renders_sched_and_cpuset() {
        let p = Policy {
            sched: Some(SchedPolicy::Fifo),
            rtprio: Some(10),
            cpuset: Some(CpuSet::Explicit(Arc::from("0-3,8"))),
            ..Policy::default()
        };
        let v = policy_json(&p);
        assert_eq!(v["sched"], json!("fifo"));
        assert_eq!(v["rtprio"], json!(10));
        assert_eq!(
            v["cpuset"],
            json!({ "kind": "explicit", "value": "0-3,8" })
        );
    }

    #[test]
    fn rules_dump_shape_for_synthetic_ruleset() {
        let rs = synthetic_ruleset();
        let v = rules_json(&rs);

        assert_eq!(v["exact"].as_array().unwrap().len(), 1);
        let exact = &v["exact"][0];
        assert_eq!(exact["name"], json!("foo"));
        assert_eq!(exact["match"], json!("exact"));
        assert_eq!(exact["nice"], json!(5));
        assert_eq!(exact["sched"], json!("batch"));
        assert_eq!(exact["cgroup"], json!("games"));
        assert!(exact.get("latency_nice").is_none());

        assert_eq!(v["regex"].as_array().unwrap().len(), 1);
        let re = &v["regex"][0];
        assert_eq!(re["name"], json!("bar-rule"));
        assert_eq!(re["match"], json!("regex"));
        assert_eq!(re["pattern"], json!("ba.*"));
        assert_eq!(re["nice"], json!(-5));
        assert_eq!(re["cpuset"], json!({ "kind": "alias", "value": "big-cores" }));
    }

    #[test]
    fn types_dump_shape_for_synthetic_ruleset() {
        let v = types_json(&synthetic_ruleset());
        assert_eq!(v["game"]["nice"], json!(2));
    }

    #[test]
    fn cgroups_dump_shape_for_synthetic_ruleset() {
        let v = cgroups_json(&synthetic_ruleset());
        assert_eq!(v["games"]["cpu_quota_pct"], json!(50));
    }

    #[test]
    fn proc_dump_includes_own_pid() {
        let v = proc_json();
        let arr = v.as_array().unwrap();
        let self_pid = std::process::id();
        let me = arr.iter().find(|o| o["pid"] == json!(self_pid));
        assert!(me.is_some(), "own pid must appear in dump proc");
        assert!(me.unwrap()["name"].as_str().unwrap().len() as i64 > 0);
    }

    #[test]
    fn bias_dump_reads_persisted_snapshot() {
        use crate::bias_state::{BiasSnapshot, EntrySnapshot, BIAS_STATE_VERSION};
        let dir = scratch_dir("bias_dump");
        let path = dir.join("bias-state");
        let snap = BiasSnapshot {
            version: BIAS_STATE_VERSION,
            threshold_ema: Some(1234.5),
            entries: vec![EntrySnapshot {
                name: "bulk".into(),
                bias: 3,
                confidence: 0.25,
                ..EntrySnapshot::default()
            }],
        };
        bias_state::save(&snap, &path).expect("save");
        let v = bias_json_at(&path);
        assert_eq!(v["path"], json!(path.display().to_string()));
        assert_eq!(v["error"], Value::Null);
        assert_eq!(v["state"]["threshold_ema"], json!(1234.5));
        assert_eq!(v["state"]["entries"][0]["name"], json!("bulk"));
        assert_eq!(v["state"]["entries"][0]["bias"], json!(3));
        assert_eq!(v["state"]["entries"][0]["confidence"], json!(0.25));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bias_dump_missing_file_is_null_state() {
        let dir = scratch_dir("bias_missing");
        let v = bias_json_at(&dir.join("absent"));
        assert_eq!(v["state"], Value::Null);
        assert_eq!(v["error"], Value::Null);
        std::fs::remove_dir_all(&dir).ok();
    }
}
