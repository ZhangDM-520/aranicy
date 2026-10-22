//! Rules loading and matching — the ananicy compatibility surface.
//!
//! Compatibility contract (docs/COMPATIBILITY.md) in brief:
//! * `$ANANICY_CPP_CONFDIR`/`ANANICY_RS_CONFDIR` (default `/etc/ananicy.d`),
//!   scanned recursively; `*.rules` / `*.types` / `*.cgroups`, each gated by
//!   `rule_load` / `type_load` / `cgroup_load`.
//! * One JSON object per line ("JSONL"); blank lines and `#` comments skipped;
//!   text from the first `{` to the *last* `}` on the line is parsed (trailing
//!   garbage after `}` is ignored — ananicy-cpp parity); CRLF tolerated; a bad
//!   line logs a warning and is skipped, it does not abort the file.
//! * Classification by content: `name`/`name_regex` → program rule, else
//!   `type` → type rule, else `cgroup` → cgroup definition.
//! * Duplicate `name`: the later-loaded rule replaces outright (no field
//!   merging). Files are loaded in *sorted* path order (determinism superset).
//! * Type merge: rule fields override the referenced type's fields; an
//!   explicit `null` suppresses the type value (RFC 7396 merge_patch
//!   semantics). The result is flattened into [`arany_model::Policy`] once,
//!   at load.
//! * Match key: exact `name` == `basename(argv[0])` (exe basename fallback),
//!   case-sensitive. Only when no exact rule matches is `name_regex` tried —
//!   unanchored substring semantics, first match in load order wins. Regexes
//!   are compiled ONCE at load (Phase 0 root cause #1 in docs/BENCHMARKS.md
//!   was per-event recompilation in ananicy-cpp).
//! * `cmdlines` (Python-fork legacy) is accepted and ignored (lint note).

use aranicy_model::{CgroupDef, Policy};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

mod parse;

/// Which file kinds to load (ananicy.conf gates).
#[derive(Debug, Clone, Copy)]
pub struct LoadOptions {
    pub rule_load: bool,
    pub type_load: bool,
    pub cgroup_load: bool,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            rule_load: true,
            type_load: true,
            cgroup_load: true,
        }
    }
}

/// Load-time problems that did not abort the load (per-line recovery).
#[derive(Debug)]
pub struct LoadWarning {
    pub file: std::path::PathBuf,
    pub line: usize,
    pub message: String,
}

/// The compiled rule tables. Everything here is immutable after load; reload
/// builds a new `RuleSet` and swaps it in.
pub struct RuleSet {
    /// Exact-name rules (`name` field).
    pub exact: HashMap<Arc<str>, Arc<Policy>>,
    /// Regex rules in load order (`name_regex` field), compiled once. The
    /// middle element is the rule's `name` when present, else the pattern
    /// (stable identity for `log_applied_rule` reporting).
    pub regexes: Vec<(regex::Regex, Arc<str>, Arc<Policy>)>,
    /// Type bundle definitions (`*.types`), flattened without a base.
    /// Extension over the skeleton: kept so `aranicy dump types` can render
    /// them; matching never consults this map (inference is done at load).
    pub types: BTreeMap<Arc<str>, Arc<Policy>>,
    pub cgroups: BTreeMap<Arc<str>, CgroupDef>,
    pub warnings: Vec<LoadWarning>,
}

impl RuleSet {
    pub fn empty() -> Self {
        Self {
            exact: HashMap::new(),
            regexes: Vec::new(),
            types: BTreeMap::new(),
            cgroups: BTreeMap::new(),
            warnings: Vec::new(),
        }
    }

    /// Exact name first (O(1)), then regex list in load order (ananicy-cpp
    /// parity). Returns the policy and which kind of rule matched.
    pub fn lookup(&self, name: &str) -> Option<(&Arc<Policy>, MatchKind)> {
        if let Some(policy) = self.exact.get(name) {
            return Some((policy, MatchKind::Exact));
        }
        self.regexes
            .iter()
            .find(|(re, _, _)| re.is_match(name))
            .map(|(_, _, policy)| (policy, MatchKind::Regex))
    }
}

// Note: there is deliberately no comm-based prescreen. The compat match key is
// `basename(argv[0])`, and `exec -a` lets argv[0] differ from comm entirely
// (verified: a `sleep` process matched a rule named `gsa-smoke-uniq`). Any
// filter that skips name resolution based on comm would silently miss rules,
// breaking the compatibility contract. The hot path instead resolves argv[0]
// with a single stack-buffered /proc/<pid>/cmdline read (no allocations),
// then does the O(1) lookup above.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    Exact,
    Regex,
}

/// Load every rules file under `dir` (recursive, sorted paths), flattening
/// type inheritance. Warnings are collected, not printed — the caller decides.
///
/// Two passes: every object is parsed first (all type defs and cgroup defs,
/// regardless of which file sorts first), then rules are flattened against the
/// final type table — a rule in `00.rules` can reference a type defined in
/// `99.types`. Parse warnings come first in load order, then type-resolution
/// warnings in load order.
pub fn load_dir(dir: &Path, opts: &LoadOptions) -> std::io::Result<RuleSet> {
    let mut warnings = Vec::new();
    let paths = collect_files(dir, opts, &mut warnings)?;

    let mut types: BTreeMap<Arc<str>, parse::PartialPolicy> = BTreeMap::new();
    let mut cgroups: BTreeMap<Arc<str>, CgroupDef> = BTreeMap::new();
    let mut drafts: Vec<parse::RuleDraft> = Vec::new();

    for path in &paths {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) => {
                warnings.push(LoadWarning {
                    file: path.clone(),
                    line: 0,
                    message: format!("failed to read file: {e}"),
                });
                continue;
            }
        };
        for (idx, raw) in text.split('\n').enumerate() {
            let line = idx + 1;
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let Some(span) = parse::extract_span(raw) else {
                warnings.push(LoadWarning {
                    file: path.clone(),
                    line,
                    message: "no JSON object span (missing '{' or '}')".to_string(),
                });
                continue;
            };
            let value: serde_json::Value = match serde_json::from_str(span) {
                Ok(value) => value,
                Err(e) => {
                    warnings.push(LoadWarning {
                        file: path.clone(),
                        line,
                        message: format!("invalid JSON in span: {e}"),
                    });
                    continue;
                }
            };
            let serde_json::Value::Object(map) = value else {
                warnings.push(LoadWarning {
                    file: path.clone(),
                    line,
                    message: "span is not a JSON object".to_string(),
                });
                continue;
            };
            match parse::classify(&map) {
                Ok((object, notes)) => {
                    for message in notes {
                        warnings.push(LoadWarning {
                            file: path.clone(),
                            line,
                            message,
                        });
                    }
                    match object {
                        parse::Object::Rule(mut draft) => {
                            if let Some(pattern) = &draft.pattern {
                                match regex::Regex::new(pattern) {
                                    Ok(re) => draft.regex = Some(re),
                                    Err(e) => warnings.push(LoadWarning {
                                        file: path.clone(),
                                        line,
                                        message: format!("invalid name_regex \"{pattern}\": {e}"),
                                    }),
                                }
                            }
                            draft.file = path.clone();
                            draft.line = line;
                            drafts.push(draft);
                        }
                        parse::Object::Type { key, partial } => {
                            // Duplicate type keys: later load replaces outright.
                            types.insert(key, partial);
                        }
                        parse::Object::Cgroup(def) => {
                            // Duplicate cgroup keys: later load replaces outright.
                            cgroups.insert(def.name.clone(), def);
                        }
                    }
                }
                Err(message) => warnings.push(LoadWarning {
                    file: path.clone(),
                    line,
                    message,
                }),
            }
        }
    }

    let mut out = RuleSet::empty();
    out.cgroups = cgroups;
    out.types = types
        .iter()
        .map(|(key, partial)| (key.clone(), Arc::new(partial.flatten())))
        .collect();

    let mut resolve_warnings = Vec::new();
    for draft in drafts {
        let base = match &draft.type_ref {
            None => parse::PartialPolicy::default(),
            Some(type_ref) => match types.get(type_ref) {
                Some(partial) => partial.clone(),
                None => {
                    resolve_warnings.push(LoadWarning {
                        file: draft.file.clone(),
                        line: draft.line,
                        message: format!(
                            "unknown type \"{type_ref}\" referenced; rule keeps its own fields"
                        ),
                    });
                    parse::PartialPolicy::default()
                }
            },
        };
        let policy = Arc::new(parse::PartialPolicy::merge(&base, &draft.partial));
        if let Some(name) = &draft.name {
            // Duplicate exact names: later load replaces outright.
            out.exact.insert(name.clone(), policy.clone());
        }
        if let (Some(pattern), Some(re)) = (&draft.pattern, &draft.regex) {
            // Both exact and regex registration when the rule has both keys.
            let label = draft.name.clone().unwrap_or_else(|| pattern.clone());
            out.regexes.push((re.clone(), label, policy));
        }
    }
    warnings.extend(resolve_warnings);
    out.warnings = warnings;
    Ok(out)
}

/// Recursive walk collecting candidate rule files in sorted full-path order.
/// The root walk's errors propagate; nested entry errors degrade to warnings.
fn collect_files(
    dir: &Path,
    opts: &LoadOptions,
    warnings: &mut Vec<LoadWarning>,
) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    collect_recursive(dir, true, opts, warnings, &mut out)?;
    out.sort();
    Ok(out)
}

fn collect_recursive(
    dir: &Path,
    top: bool,
    opts: &LoadOptions,
    warnings: &mut Vec<LoadWarning>,
    out: &mut Vec<PathBuf>,
) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if top => return Err(e),
        Err(e) => {
            warnings.push(LoadWarning {
                file: dir.to_path_buf(),
                line: 0,
                message: format!("cannot read directory: {e}"),
            });
            return Ok(());
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                warnings.push(LoadWarning {
                    file: dir.to_path_buf(),
                    line: 0,
                    message: format!("cannot read directory entry: {e}"),
                });
                continue;
            }
        };
        let path = entry.path();
        // Only real directories are recursed (symlinked dirs are not followed,
        // so a link loop cannot hang the load).
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            collect_recursive(&path, false, opts, warnings, out)?;
        } else if let Some(kind) = file_kind(&path) {
            let enabled = match kind {
                FileKind::Rules => opts.rule_load,
                FileKind::Types => opts.type_load,
                FileKind::Cgroups => opts.cgroup_load,
            };
            if enabled {
                out.push(path);
            }
        }
    }
    Ok(())
}

enum FileKind {
    Rules,
    Types,
    Cgroups,
}

fn file_kind(path: &Path) -> Option<FileKind> {
    match path.extension()?.to_str()? {
        "rules" => Some(FileKind::Rules),
        "types" => Some(FileKind::Types),
        "cgroups" => Some(FileKind::Cgroups),
        _ => None,
    }
}
