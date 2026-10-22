//! Config + rules loading shared by every CLI action.

use aranicy_rules::{load_dir, LoadOptions, RuleSet};

use crate::config::Config;
use crate::paths::Paths;

/// Load `ananicy.conf` then the rules tree (gated by `rule_load`/`type_load`/
/// `cgroup_load`). Config missing -> defaults + warning; rules dir missing ->
/// io error (fatal for the caller).
pub fn load_all(paths: &Paths) -> std::io::Result<(Config, RuleSet)> {
    let cfg = Config::load(&paths.config)?;
    let opts = LoadOptions {
        rule_load: cfg.rule_load,
        type_load: cfg.type_load,
        cgroup_load: cfg.cgroup_load,
    };
    let rules = load_dir(&paths.config_dir, &opts)?;
    Ok((cfg, rules))
}

/// Rule-count identity used by `lint` and the reload summary.
pub fn rule_count(rules: &RuleSet) -> usize {
    rules.exact.len() + rules.regexes.len()
}
