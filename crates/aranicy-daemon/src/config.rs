//! `ananicy.conf` parsing (superset of the ananicy-cpp keys).
//!
//! Syntax: `key = value` per line, first `=` splits, both sides trimmed,
//! `#` comments and blank lines skipped. Unknown keys are reported in
//! [`Config::warnings`] (superset) but never fatal.

use std::path::Path;

#[derive(Debug, Clone)]
pub struct Config {
    pub check_freq: u64,
    pub cgroup_load: bool,
    pub type_load: bool,
    pub rule_load: bool,
    pub apply_nice: bool,
    pub apply_latnice: bool,
    pub apply_ionice: bool,
    pub apply_sched: bool,
    pub apply_oom_score_adj: bool,
    pub apply_cgroup: bool,
    pub apply_cpuset: bool,
    pub apply_autogroup: bool,
    pub cgroup_realtime_workaround: bool,
    pub loglevel: String,
    pub log_applied_rule: bool,
    pub x3d_mode: String,
    // Dynamic nice bias (plan.md §4.3) — driven by aranicy-collectord.
    pub bias_enable: bool,
    /// Collector stats socket (`aranicy-collectord`, single fixed source).
    pub bias_collector_socket: String,
    /// Collector exec-event socket.
    pub collector_events_socket: String,
    /// Startup readiness budget for the collector sockets (ms).
    pub collector_wait_ms: u64,
    /// Share-starvation gate: demand floor (0..1024, 1024 == one CPU).
    pub bias_starved_min_demand: u64,
    /// Share-starvation gate: starved when delivered < ratio * demand.
    pub bias_starved_ratio: f64,
    pub bias_min: i8,
    pub bias_max: i8,
    pub bias_window_s: u64,
    pub bias_deadband_pct: u64,
    pub bias_min_samples: u64,
    pub bias_cadence_ms: u64,
    pub bias_max_entries: usize,
    pub bias_half_life_s: u64,
    pub bias_state_path: String,
    /// `bias_group.<tag> = <regex>` entries: comms matching the regex learn as
    /// one pooled entry named `<tag>`. Raw (tag, regex-source) pairs; compiled
    /// once at startup (invalid regexes are warned and skipped there).
    pub bias_groups: Vec<(String, String)>,
    pub warnings: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            check_freq: 60,
            cgroup_load: true,
            type_load: true,
            rule_load: true,
            apply_nice: true,
            apply_latnice: true,
            apply_ionice: true,
            apply_sched: true,
            apply_oom_score_adj: true,
            apply_cgroup: true,
            apply_cpuset: true,
            apply_autogroup: true,
            cgroup_realtime_workaround: true,
            loglevel: "info".into(),
            log_applied_rule: false,
            x3d_mode: "auto".into(),
            bias_enable: true,
            bias_collector_socket: "/run/aranicy/collector-stats.sock".into(),
            collector_events_socket: "/run/aranicy/collector-events.sock".into(),
            collector_wait_ms: 5000,
            bias_starved_min_demand: 64,
            bias_starved_ratio: 0.5,
            bias_min: -1,
            bias_max: 3,
            bias_window_s: 60,
            bias_deadband_pct: 15,
            bias_min_samples: 30,
            bias_cadence_ms: 500,
            bias_max_entries: 4096,
            bias_half_life_s: 1800,
            bias_state_path: "/var/lib/aranicy/bias-state".into(),
            bias_groups: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

impl Config {
    /// Parse config text. Malformed lines are warnings, not errors (parity).
    pub fn parse(text: &str) -> Self {
        let mut cfg = Self::default();
        for (idx, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                cfg.warnings.push(format!("line {}: no '=' in {line:?}", idx + 1));
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            let bad = |cfg: &mut Self, what: &str| {
                cfg.warnings.push(format!("line {}: invalid {what} value {value:?}", idx + 1));
            };
            match key {
                "check_freq" => match value.parse::<u64>() {
                    Ok(v) => cfg.check_freq = v,
                    Err(_) => bad(&mut cfg, "check_freq (u64 seconds)"),
                },
                "cgroup_load" => set_bool(value, &mut cfg.cgroup_load, idx, &mut cfg.warnings),
                "type_load" => set_bool(value, &mut cfg.type_load, idx, &mut cfg.warnings),
                "rule_load" => set_bool(value, &mut cfg.rule_load, idx, &mut cfg.warnings),
                "apply_nice" => set_bool(value, &mut cfg.apply_nice, idx, &mut cfg.warnings),
                "apply_latnice" => set_bool(value, &mut cfg.apply_latnice, idx, &mut cfg.warnings),
                "apply_ionice" | "apply_ioclass" => {
                    set_bool(value, &mut cfg.apply_ionice, idx, &mut cfg.warnings)
                }
                "apply_sched" => set_bool(value, &mut cfg.apply_sched, idx, &mut cfg.warnings),
                "apply_oom_score_adj" => set_bool(value, &mut cfg.apply_oom_score_adj, idx, &mut cfg.warnings),
                "apply_cgroup" => set_bool(value, &mut cfg.apply_cgroup, idx, &mut cfg.warnings),
                "apply_cpuset" => set_bool(value, &mut cfg.apply_cpuset, idx, &mut cfg.warnings),
                "apply_autogroup" => set_bool(value, &mut cfg.apply_autogroup, idx, &mut cfg.warnings),
                "cgroup_realtime_workaround" => set_bool(
                    value,
                    &mut cfg.cgroup_realtime_workaround,
                    idx,
                    &mut cfg.warnings,
                ),
                "log_applied_rule" => set_bool(value, &mut cfg.log_applied_rule, idx, &mut cfg.warnings),
                "loglevel" => {
                    const LEVELS: [&str; 7] =
                        ["trace", "debug", "info", "warn", "error", "critical", "off"];
                    if LEVELS.contains(&value) {
                        cfg.loglevel = value.to_string();
                    } else {
                        bad(&mut cfg, "loglevel");
                    }
                }
                "x3d_mode" => {
                    const MODES: [&str; 3] = ["auto", "cache", "frequency"];
                    if MODES.contains(&value) {
                        cfg.x3d_mode = value.to_string();
                    } else {
                        bad(&mut cfg, "x3d_mode");
                    }
                }
                "bias_enable" => set_bool(value, &mut cfg.bias_enable, idx, &mut cfg.warnings),
                "bias_min" => match value.parse::<i8>() {
                    Ok(v) => cfg.bias_min = v,
                    Err(_) => bad(&mut cfg, "bias_min (i8)"),
                },
                "bias_max" => match value.parse::<i8>() {
                    Ok(v) => cfg.bias_max = v,
                    Err(_) => bad(&mut cfg, "bias_max (i8)"),
                },
                "bias_window_s" => match value.parse::<u64>() {
                    Ok(v) => cfg.bias_window_s = v,
                    Err(_) => bad(&mut cfg, "bias_window_s (u64)"),
                },
                "bias_deadband_pct" => match value.parse::<u64>() {
                    Ok(v) => cfg.bias_deadband_pct = v,
                    Err(_) => bad(&mut cfg, "bias_deadband_pct (u64)"),
                },
                "bias_min_samples" => match value.parse::<u64>() {
                    Ok(v) => cfg.bias_min_samples = v,
                    Err(_) => bad(&mut cfg, "bias_min_samples (u64)"),
                },
                "bias_cadence_ms" => match value.parse::<u64>() {
                    Ok(v) => cfg.bias_cadence_ms = v,
                    Err(_) => bad(&mut cfg, "bias_cadence_ms (u64)"),
                },
                "bias_max_entries" => match value.parse::<usize>() {
                    Ok(v) => cfg.bias_max_entries = v,
                    Err(_) => bad(&mut cfg, "bias_max_entries (usize)"),
                },
                "bias_half_life_s" => match value.parse::<u64>() {
                    Ok(v) => cfg.bias_half_life_s = v,
                    Err(_) => bad(&mut cfg, "bias_half_life_s (u64)"),
                },
                "bias_state_path" => {
                    if value.is_empty() {
                        cfg.warnings
                            .push(format!("line {}: empty bias_state_path ignored", idx + 1));
                    } else {
                        cfg.bias_state_path = value.to_string();
                    }
                }
                "bias_collector_socket" => {
                    if value.is_empty() {
                        cfg.warnings
                            .push(format!("line {}: empty bias_collector_socket ignored", idx + 1));
                    } else {
                        cfg.bias_collector_socket = value.to_string();
                    }
                }
                "collector_events_socket" => {
                    if value.is_empty() {
                        cfg.warnings.push(format!(
                            "line {}: empty collector_events_socket ignored",
                            idx + 1
                        ));
                    } else {
                        cfg.collector_events_socket = value.to_string();
                    }
                }
                "collector_wait_ms" => match value.parse::<u64>() {
                    Ok(v) => cfg.collector_wait_ms = v,
                    Err(_) => bad(&mut cfg, "collector_wait_ms (u64)"),
                },
                "bias_starved_min_demand" => match value.parse::<u64>() {
                    // task_util_est is clamped to 1024 (one full CPU), so a
                    // larger floor disables the starved gate forever.
                    Ok(v) if v < 1024 => cfg.bias_starved_min_demand = v,
                    Ok(_) => bad(&mut cfg, "bias_starved_min_demand (< 1024)"),
                    Err(_) => bad(&mut cfg, "bias_starved_min_demand (u64)"),
                },
                "bias_starved_ratio" => match value.parse::<f64>() {
                    Ok(v) if (0.0..=1.0).contains(&v) => cfg.bias_starved_ratio = v,
                    _ => bad(&mut cfg, "bias_starved_ratio (0.0..=1.0)"),
                },
                k if k.starts_with("bias_group.") => {
                    let tag = k["bias_group.".len()..].trim();
                    if tag.is_empty() {
                        cfg.warnings.push(format!("line {}: empty bias_group tag", idx + 1));
                    } else if value.is_empty() {
                        cfg.warnings.push(format!(
                            "line {}: empty regex for bias_group.{tag}",
                            idx + 1
                        ));
                    } else if cfg.bias_groups.iter().any(|(t, _)| t == tag) {
                        cfg.warnings.push(format!(
                            "line {}: duplicate bias_group.{tag}; keeping first",
                            idx + 1
                        ));
                    } else {
                        cfg.bias_groups.push((tag.to_string(), value.to_string()));
                    }
                }
                other => cfg.warnings.push(format!("line {}: unknown key {other:?}", idx + 1)),
            }
        }
        // An inverted clamp range would panic inside i8::clamp on the bias
        // thread; refuse it here with the warn-and-fallback convention.
        if cfg.bias_min > cfg.bias_max {
            cfg.warnings.push(format!(
                "bias_min ({}) > bias_max ({}); using defaults",
                cfg.bias_min, cfg.bias_max
            ));
            let d = Self::default();
            cfg.bias_min = d.bias_min;
            cfg.bias_max = d.bias_max;
        }
        cfg
    }

    /// Load from a path; a missing file yields defaults with a warning.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(Self::parse(&text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut cfg = Self::default();
                cfg.warnings.push(format!("config {:?} not found; using defaults", path));
                Ok(cfg)
            }
            Err(e) => Err(e),
        }
    }

    /// Map the `bias_*` config keys onto the bias engine's knobs (plan.md §4.3).
    pub fn bias_config(&self) -> crate::bias::BiasConfig {
        crate::bias::BiasConfig {
            window_s: self.bias_window_s,
            deadband_pct: self.bias_deadband_pct,
            min_samples: self.bias_min_samples,
            half_life_s: self.bias_half_life_s,
            max_entries: self.bias_max_entries,
            min_bias: self.bias_min,
            max_bias: self.bias_max,
            starved_min_demand: self.bias_starved_min_demand,
            starved_ratio: self.bias_starved_ratio,
            ..crate::bias::BiasConfig::default()
        }
    }

    /// Map the `apply_*` gates onto the applier's per-dimension switches.
    pub fn apply_options(&self) -> aranicy_apply::ApplyOptions {
        aranicy_apply::ApplyOptions {
            nice: self.apply_nice,
            latency_nice: self.apply_latnice,
            sched: self.apply_sched,
            ionice: self.apply_ionice,
            oom_score_adj: self.apply_oom_score_adj,
            cgroup: self.apply_cgroup,
            cpuset: self.apply_cpuset,
            autogroup: self.apply_autogroup,
            cgroup_realtime_workaround: self.cgroup_realtime_workaround,
        }
    }
}

fn set_bool(
    value: &str,
    slot: &mut bool,
    idx: usize,
    warnings: &mut Vec<String>,
) {
    match value {
        "true" => *slot = true,
        "false" => *slot = false,
        _ => warnings.push(format!("line {}: invalid boolean {value:?}", idx + 1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collector_keys_are_validated() {
        let cfg = Config::parse(
            "bias_starved_min_demand = 1024\n\
             bias_collector_socket =\n\
             bias_state_path =\n\
             collector_wait_ms = 250\n",
        );
        // A floor at/above MAX_UTIL would disable the starved gate forever,
        // and an empty path would silently override the default.
        assert_eq!(cfg.bias_starved_min_demand, 64);
        assert_eq!(cfg.bias_collector_socket, "/run/aranicy/collector-stats.sock");
        assert_eq!(cfg.bias_state_path, "/var/lib/aranicy/bias-state");
        assert_eq!(cfg.collector_wait_ms, 250);
        assert_eq!(cfg.warnings.len(), 3);

        let ok = Config::parse("bias_starved_min_demand = 1023\n");
        assert_eq!(ok.bias_starved_min_demand, 1023);
        assert!(ok.warnings.is_empty());
    }

    #[test]
    fn parses_booleans_and_numbers() {
        let cfg = Config::parse("check_freq = 30\napply_nice = false\nlog_applied_rule = true\n");
        assert_eq!(cfg.check_freq, 30);
        assert!(!cfg.apply_nice);
        assert!(cfg.log_applied_rule);
        assert!(cfg.warnings.is_empty());
    }

    #[test]
    fn comments_blanks_and_crlf() {
        let cfg = Config::parse("# comment\r\n\r\nloglevel = warn\r\n");
        assert_eq!(cfg.loglevel, "warn");
        assert!(cfg.warnings.is_empty());
    }

    #[test]
    fn bad_lines_warn_and_skip() {
        let cfg = Config::parse("check_freq = nonsense\napply_nice = maybe\ngarbage\n");
        assert_eq!(cfg.check_freq, 60); // unchanged
        assert!(cfg.apply_nice); // unchanged
        assert_eq!(cfg.warnings.len(), 3);
    }

    #[test]
    fn unknown_key_warns_but_loads() {
        let cfg = Config::parse("check_disks_schedulers = true\napply_nice = false\n");
        assert!(!cfg.apply_nice);
        assert_eq!(cfg.warnings.len(), 1);
        assert!(cfg.warnings[0].contains("unknown key"));
    }

    #[test]
    fn ioclass_alias_maps_to_ionice_gate() {
        let cfg = Config::parse("apply_ioclass = false\n");
        assert!(!cfg.apply_ionice);
    }

    #[test]
    fn missing_file_yields_defaults_with_warning() {
        let cfg = Config::load(Path::new("/nonexistent/ananicy.conf")).unwrap();
        assert_eq!(cfg.check_freq, 60);
        assert_eq!(cfg.warnings.len(), 1);
    }

    #[test]
    fn inverted_bias_clamps_warn_and_reset_to_defaults() {
        // bias_min > bias_max would panic i8::clamp on the bias thread.
        let cfg = Config::parse("bias_min = 5\nbias_max = 2\n");
        let d = Config::default();
        assert_eq!(cfg.bias_min, d.bias_min);
        assert_eq!(cfg.bias_max, d.bias_max);
        assert!(cfg.warnings.iter().any(|w| w.contains("bias_min")));
    }

    #[test]
    fn bias_group_entries_parse_and_bad_ones_warn() {
        let cfg = Config::parse(
            "bias_group.build = ^(cc1.*|lto1)$\n\
             bias_group.build = ^dup$\n\
             bias_group. = ^x$\n\
             bias_group.empty =\n",
        );
        // first definition wins; the three bad lines each warn and skip
        assert_eq!(
            cfg.bias_groups,
            vec![("build".to_string(), "^(cc1.*|lto1)$".to_string())]
        );
        assert_eq!(cfg.warnings.len(), 3);
    }

    #[test]
    fn collector_socket_and_starved_gate_keys() {
        let d = Config::default();
        assert_eq!(d.bias_collector_socket, "/run/aranicy/collector-stats.sock");
        assert_eq!(d.bias_starved_min_demand, 64);
        assert!((d.bias_starved_ratio - 0.5).abs() < f64::EPSILON);

        let cfg = Config::parse(
            "bias_collector_socket = /run/test/stats.sock\n\
             bias_starved_min_demand = 128\n\
             bias_starved_ratio = 0.75\n",
        );
        assert!(cfg.warnings.is_empty(), "warnings: {:?}", cfg.warnings);
        assert_eq!(cfg.bias_collector_socket, "/run/test/stats.sock");
        assert_eq!(cfg.bias_starved_min_demand, 128);
        assert!((cfg.bias_starved_ratio - 0.75).abs() < f64::EPSILON);

        // The gate thresholds must reach the engine, not stay in the parser.
        let bias = cfg.bias_config();
        assert_eq!(bias.starved_min_demand, 128);
        assert!((bias.starved_ratio - 0.75).abs() < f64::EPSILON);

        // Out-of-range ratio is rejected with a warning, default kept.
        let bad = Config::parse("bias_starved_ratio = 3.0\n");
        assert!((bad.bias_starved_ratio - 0.5).abs() < f64::EPSILON);
        assert_eq!(bad.warnings.len(), 1);
    }
}
