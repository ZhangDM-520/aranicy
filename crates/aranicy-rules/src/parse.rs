//! Internal line/object parsing for the rules loader.
//!
//! Each JSONL line is parsed to [`serde_json::Value`] first, then fields are
//! extracted manually with type checks. This keeps the JSON `null`-vs-absent
//! distinction alive, which type merging (RFC 7396-flavoured) needs: absent
//! = inherit, explicit `null` = suppress, value = override.

use aranicy_model::{CgroupDef, CpuSet, IoClass, Policy, SchedPolicy};
use serde_json::{Map, Value};
use std::sync::Arc;

/// Per-field presence marker: `None` = key absent (inherit), `Some(None)` =
/// explicit JSON `null` (suppress), `Some(Some(v))` = value (override).
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct PartialPolicy {
    pub nice: Option<Option<i8>>,
    pub latency_nice: Option<Option<i8>>,
    pub sched: Option<Option<SchedPolicy>>,
    pub rtprio: Option<Option<u8>>,
    pub ioclass: Option<Option<IoClass>>,
    pub ionice: Option<Option<u8>>,
    pub oom_score_adj: Option<Option<i16>>,
    pub cgroup: Option<Option<Arc<str>>>,
    pub cpuset: Option<Option<CpuSet>>,
    pub autogroup: Option<Option<i8>>,
}

impl PartialPolicy {
    /// Flatten `over` onto `base` (the referenced type def): present values
    /// override, explicit `null` suppresses the inherited value, absent keys
    /// inherit. The result is final — `lookup()` never merges again.
    pub(crate) fn merge(base: &Self, over: &Self) -> Policy {
        Policy {
            nice: over.nice.or(base.nice).flatten(),
            latency_nice: over.latency_nice.or(base.latency_nice).flatten(),
            sched: over.sched.or(base.sched).flatten(),
            rtprio: over.rtprio.or(base.rtprio).flatten(),
            ioclass: over.ioclass.or(base.ioclass).flatten(),
            ionice: over.ionice.or(base.ionice).flatten(),
            oom_score_adj: over.oom_score_adj.or(base.oom_score_adj).flatten(),
            cgroup: over.cgroup.clone().or(base.cgroup.clone()).flatten(),
            cpuset: over.cpuset.clone().or(base.cpuset.clone()).flatten(),
            autogroup: over.autogroup.or(base.autogroup).flatten(),
        }
    }

    /// Flatten with an empty base (type defs and type-free rules).
    pub(crate) fn flatten(&self) -> Policy {
        Self::merge(&Self::default(), self)
    }
}

/// The parseable span of a raw line: from the first `{` to the *last* `}` on
/// the line (ananicy-cpp parity — trailing garbage without braces is ignored,
/// garbage containing `}` extends the span and then fails to parse).
/// `None` when the line has no such span.
pub(crate) fn extract_span(line: &str) -> Option<&str> {
    let start = line.find('{')?;
    let end = line.rfind('}')?;
    if end < start {
        return None;
    }
    Some(&line[start..=end])
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// `Ok(None)` for explicit `null`; `Err` for a present-but-invalid value
/// (the caller rejects the whole line).
fn int_field(value: &Value, key: &str, lo: i64, hi: i64) -> Result<Option<i64>, String> {
    match value {
        Value::Null => Ok(None),
        Value::Number(n) => match n.as_i64() {
            Some(v) if (lo..=hi).contains(&v) => Ok(Some(v)),
            Some(v) => Err(format!("{key}: {v} out of range {lo}..={hi}")),
            None => Err(format!("{key}: expected integer, got {n}")),
        },
        other => Err(format!("{key}: expected integer, got {}", type_name(other))),
    }
}

fn str_field<'a>(value: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match value {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s.as_str())),
        other => Err(format!("{key}: expected string, got {}", type_name(other))),
    }
}

fn parse_sched(s: &str) -> Result<SchedPolicy, String> {
    match s {
        "fifo" => Ok(SchedPolicy::Fifo),
        "rr" => Ok(SchedPolicy::Rr),
        "normal" | "other" => Ok(SchedPolicy::Normal),
        "batch" => Ok(SchedPolicy::Batch),
        "idle" => Ok(SchedPolicy::Idle),
        // Reportable through ananicy-cpp's dump, but never settable.
        "deadline" => Err("sched: \"deadline\" is reportable but unsettable".to_string()),
        other => Err(format!("sched: invalid value \"{other}\"")),
    }
}

fn parse_ioclass(s: &str) -> Result<IoClass, String> {
    match s {
        "best-effort" => Ok(IoClass::BestEffort),
        "realtime" => Ok(IoClass::Realtime),
        "idle" => Ok(IoClass::Idle),
        "none" => Ok(IoClass::None),
        other => Err(format!("ioclass: invalid value \"{other}\"")),
    }
}

const CPUSET_ALIASES: &[&str] = &[
    "big-cores",
    "little-cores",
    "turbo-cores",
    "performance-cores",
    "efficiency-cores",
    "all-cores",
    "x3d-cache",
    "x3d-frequency",
];

/// Alias names: the fixed table plus `llc-N` / `node-N` with numeric `N`.
/// Case-sensitive; anything else is an explicit cpuset string.
fn is_alias_name(s: &str) -> bool {
    CPUSET_ALIASES.contains(&s)
        || ["llc-", "node-"].iter().any(|prefix| {
            s.strip_prefix(*prefix)
                .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
        })
}

fn parse_cpuset(s: &str) -> CpuSet {
    if is_alias_name(s) {
        CpuSet::Alias(Arc::from(s))
    } else {
        CpuSet::Explicit(Arc::from(s))
    }
}

/// Keys carrying policy fields (plus `cmdlines`, accepted-and-ignored).
/// Classification keys (`name`, `name_regex`, `type`, `cgroup`, `CPUQuota`)
/// are passed in via `extra_known` per object kind.
pub(crate) const POLICY_KEYS: &[&str] = &[
    "nice",
    "latency_nice",
    "sched",
    "rtprio",
    "ioclass",
    "ionice",
    "oom_score_adj",
    "cgroup",
    "cpuset",
    "autogroup",
    "cmdlines",
];

/// Parse the policy-bearing fields of one object. Non-fatal notes (unknown
/// keys, `cmdlines`) are returned alongside; a present-but-invalid value is
/// fatal for the whole line (`Err`), matching "reject at load and continue".
pub(crate) fn parse_policy_fields(
    map: &Map<String, Value>,
    extra_known: &[&str],
) -> Result<(PartialPolicy, Vec<String>), String> {
    let mut p = PartialPolicy::default();
    let mut notes = Vec::new();
    for (key, value) in map {
        match key.as_str() {
            // Every arm below runs only when the key is present, so the
            // outer `Some` is always set: `Some(None)` = explicit `null`
            // (suppress), `Some(Some(v))` = value (override).
            "nice" => p.nice = Some(int_field(value, "nice", -20, 19)?.map(|v| v as i8)),
            "latency_nice" => {
                p.latency_nice = Some(int_field(value, "latency_nice", -20, 19)?.map(|v| v as i8))
            }
            "autogroup" => {
                p.autogroup = Some(int_field(value, "autogroup", -20, 19)?.map(|v| v as i8))
            }
            "rtprio" => p.rtprio = Some(int_field(value, "rtprio", 0, 99)?.map(|v| v as u8)),
            "ionice" => p.ionice = Some(int_field(value, "ionice", 0, 7)?.map(|v| v as u8)),
            "oom_score_adj" => {
                p.oom_score_adj =
                    Some(int_field(value, "oom_score_adj", -1000, 1000)?.map(|v| v as i16))
            }
            "sched" => {
                p.sched = match str_field(value, "sched")? {
                    None => Some(None),
                    Some(s) => Some(Some(parse_sched(s)?)),
                }
            }
            "ioclass" => {
                p.ioclass = match str_field(value, "ioclass")? {
                    None => Some(None),
                    Some(s) => Some(Some(parse_ioclass(s)?)),
                }
            }
            "cgroup" => {
                p.cgroup = match str_field(value, "cgroup")? {
                    None => Some(None),
                    Some(s) => Some(Some(Arc::from(s))),
                }
            }
            "cpuset" => {
                p.cpuset = match str_field(value, "cpuset")? {
                    None => Some(None),
                    Some(s) => Some(Some(parse_cpuset(s))),
                }
            }
            // Python-fork legacy: accepted, ignored, noted once per object.
            "cmdlines" => notes.push("cmdlines accepted but ignored".to_string()),
            other if POLICY_KEYS.contains(&other) || extra_known.contains(&other) => {}
            other => notes.push(format!("unknown key \"{other}\" ignored")),
        }
    }
    Ok((p, notes))
}

/// A program rule after field extraction but before type flattening.
pub(crate) struct RuleDraft {
    pub name: Option<Arc<str>>,
    /// The `name_regex` pattern string (also the display label when the rule
    /// has no `name`).
    pub pattern: Option<Arc<str>>,
    /// Compiled at parse time; `None` after a regex syntax error (only the
    /// regex registration is dropped — the exact-name rule still stands).
    pub regex: Option<regex::Regex>,
    pub type_ref: Option<Arc<str>>,
    pub partial: PartialPolicy,
    pub file: std::path::PathBuf,
    pub line: usize,
}

pub(crate) enum Object {
    Rule(RuleDraft),
    Type { key: Arc<str>, partial: PartialPolicy },
    Cgroup(CgroupDef),
}

/// Classify one JSON object by content (COMPATIBILITY.md): string `name` or
/// `name_regex` present → program rule; else string `type` → type def; else
/// string `cgroup` → cgroup def; else unclassified (warning). Non-fatal
/// notes come back alongside; `Err` rejects the whole line.
pub(crate) fn classify(map: &Map<String, Value>) -> Result<(Object, Vec<String>), String> {
    let is_rule = map.get("name").is_some_and(Value::is_string) || map.contains_key("name_regex");
    if is_rule {
        // `null` on name/name_regex/type is treated as absent (these are not
        // policy fields; only policy fields participate in null suppression).
        let name = map
            .get("name")
            .map(|v| str_field(v, "name"))
            .transpose()?
            .flatten()
            .map(Arc::from);
        let pattern = map
            .get("name_regex")
            .map(|v| str_field(v, "name_regex"))
            .transpose()?
            .flatten()
            .map(Arc::from);
        let type_ref = map
            .get("type")
            .map(|v| str_field(v, "type"))
            .transpose()?
            .flatten()
            .map(Arc::from);
        let (partial, notes) = parse_policy_fields(map, &["name", "name_regex", "type"])?;
        Ok((
            Object::Rule(RuleDraft {
                name,
                pattern,
                regex: None,
                type_ref,
                partial,
                file: std::path::PathBuf::new(),
                line: 0,
            }),
            notes,
        ))
    } else if let Some(key) = map.get("type").and_then(Value::as_str) {
        let (partial, notes) = parse_policy_fields(map, &["type"])?;
        Ok((Object::Type { key: Arc::from(key), partial }, notes))
    } else if let Some(key) = map.get("cgroup").and_then(Value::as_str) {
        let cpu_quota_pct = match map.get("CPUQuota") {
            None => None,
            Some(v) => int_field(v, "CPUQuota", 0, 100)?.map(|n| n as u8),
        };
        let notes = map
            .keys()
            .filter(|k| k.as_str() != "cgroup" && k.as_str() != "CPUQuota")
            .map(|k| format!("unknown key \"{k}\" ignored"))
            .collect();
        Ok((
            Object::Cgroup(CgroupDef {
                name: Arc::from(key),
                cpu_quota_pct,
            }),
            notes,
        ))
    } else {
        Err("unclassified object: expected string name/name_regex, type, or cgroup".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_is_first_open_to_last_close_brace() {
        assert_eq!(extract_span("x {\"a\":1} tail"), Some("{\"a\":1}"));
        // Literal last-`}` rule: garbage containing `}` extends the span.
        assert_eq!(
            extract_span("{\"a\":1} tail {\"b\":2}"),
            Some("{\"a\":1} tail {\"b\":2}")
        );
        assert_eq!(extract_span("no braces"), None);
        assert_eq!(extract_span("} {"), None);
    }

    #[test]
    fn cpuset_alias_detection() {
        for alias in [
            "big-cores",
            "little-cores",
            "turbo-cores",
            "performance-cores",
            "efficiency-cores",
            "all-cores",
            "x3d-cache",
            "x3d-frequency",
            "llc-0",
            "llc-12",
            "node-3",
        ] {
            assert!(matches!(parse_cpuset(alias), CpuSet::Alias(_)), "{alias}");
        }
        for explicit in ["0-3,8-11", "llc-", "node-", "llc-x", "llc-00x", "BIG-CORES"] {
            assert!(matches!(parse_cpuset(explicit), CpuSet::Explicit(_)), "{explicit}");
        }
    }

    #[test]
    fn merge_inherits_overrides_and_null_suppresses() {
        let base = PartialPolicy {
            nice: Some(Some(5)),
            ioclass: Some(Some(IoClass::Idle)),
            cgroup: Some(Some(Arc::from("games"))),
            ..Default::default()
        };
        // Absent field inherits.
        let over = PartialPolicy::default();
        let p = PartialPolicy::merge(&base, &over);
        assert_eq!(p.nice, Some(5));
        assert_eq!(p.cgroup.as_deref(), Some("games"));

        // Present value overrides.
        let over = PartialPolicy {
            nice: Some(Some(10)),
            ..Default::default()
        };
        let p = PartialPolicy::merge(&base, &over);
        assert_eq!(p.nice, Some(10));
        assert_eq!(p.ioclass, Some(IoClass::Idle));

        // Explicit null suppresses the inherited value.
        let over = PartialPolicy {
            nice: Some(None),
            cgroup: Some(None),
            ..Default::default()
        };
        let p = PartialPolicy::merge(&base, &over);
        assert_eq!(p.nice, None);
        assert_eq!(p.cgroup, None);
        assert_eq!(p.ioclass, Some(IoClass::Idle));
    }

    #[test]
    fn invalid_values_are_fatal_null_is_not() {
        let mut map = Map::new();
        map.insert("nice".into(), Value::from(50));
        assert!(parse_policy_fields(&map, &[]).is_err());

        let mut map = Map::new();
        map.insert("nice".into(), Value::Null);
        let (p, notes) = parse_policy_fields(&map, &[]).unwrap();
        assert_eq!(p.nice, Some(None));
        assert!(notes.is_empty());
    }
}
