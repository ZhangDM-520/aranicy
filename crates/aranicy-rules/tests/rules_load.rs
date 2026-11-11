//! End-to-end loader tests. Rule fixtures are written to a unique temp dir at
//! test time (CARGO_TARGET_TMPDIR) and removed on drop — no real rule-pack
//! data is committed (upstream packs are GPL).

use aranicy_model::{CpuSet, IoClass, SchedPolicy};
use aranicy_rules::{load_dir, LoadOptions, MatchKind, RuleSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let base = std::env::var_os("CARGO_TARGET_TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("target")
                    .join("test-tmp")
            });
        let root = base.join(format!(
            "aranicy-rules-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn write(&self, rel: &str, content: &str) {
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
    }

    fn load(&self, opts: LoadOptions) -> RuleSet {
        load_dir(&self.root, &opts).unwrap()
    }

    fn load_default(&self) -> RuleSet {
        self.load(LoadOptions::default())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn messages(rs: &RuleSet) -> Vec<&str> {
    rs.warnings.iter().map(|w| w.message.as_str()).collect()
}

#[test]
fn tolerates_comments_blanks_crlf_and_trailing_garbage() {
    let fx = Fixture::new("tolerance");
    fx.write(
        "a.rules",
        "\n \n\t# comment {\"name\": \"nope\"}\n\
         {\"name\": \"g1\", \"nice\": 1} trailing garbage without braces\n\
         {\"name\": \"crlf\", \"nice\": -3}\r\n\
         # another comment\n",
    );
    let rs = fx.load_default();
    assert_eq!(rs.exact.len(), 2);
    assert!(rs.warnings.is_empty(), "unexpected warnings: {:?}", rs.warnings);
    assert_eq!(rs.lookup("g1").unwrap().0.nice, Some(1));
    assert_eq!(rs.lookup("crlf").unwrap().0.nice, Some(-3));
}

#[test]
fn bad_line_warns_and_skips_only_that_line() {
    let fx = Fixture::new("badline");
    fx.write(
        "a.rules",
        "{\"name\": \"ok1\", \"nice\": 1}\n\
         {\"name\": \"bad\", \"nice\": }\n\
         {\"name\": \"ok2\", \"nice\": 2}\n",
    );
    let rs = fx.load_default();
    assert_eq!(rs.exact.len(), 2);
    assert!(rs.lookup("ok1").is_some());
    assert!(rs.lookup("ok2").is_some());
    assert!(rs.lookup("bad").is_none());
    assert_eq!(rs.warnings.len(), 1);
    assert_eq!(rs.warnings[0].line, 2);
    assert!(rs.warnings[0].message.contains("invalid JSON"), "{:?}", rs.warnings);
}

#[test]
fn classifies_objects_by_content() {
    let fx = Fixture::new("classify");
    fx.write(
        "all.rules",
        "{\"name\": \"prog\", \"nice\": 1}\n\
         {\"name_regex\": \"reg.*x\", \"nice\": 2}\n\
         {\"type\": \"tdef\", \"nice\": 3}\n\
         {\"cgroup\": \"cgdef\", \"CPUQuota\": 25}\n\
         {\"mystery\": true}\n",
    );
    let rs = fx.load_default();
    assert_eq!(rs.exact.len(), 1);
    assert!(rs.lookup("prog").is_some());
    assert_eq!(rs.regexes.len(), 1);
    assert_eq!(rs.types.len(), 1);
    assert_eq!(rs.types["tdef"].nice, Some(3));
    assert_eq!(rs.cgroups.len(), 1);
    assert_eq!(rs.cgroups["cgdef"].cpu_quota_pct, Some(25));
    assert_eq!(rs.warnings.len(), 1);
    assert!(messages(&rs)[0].contains("unclassified"), "{:?}", rs.warnings);
}

#[test]
fn unknown_keys_warn_but_keep_rule_cmdlines_noted_once() {
    let fx = Fixture::new("unknown");
    fx.write(
        "a.rules",
        "{\"name\": \"u\", \"nice\": 1, \"bogus\": 42, \"cmdlines\": [\"a\", \"b\"]}\n",
    );
    let rs = fx.load_default();
    assert_eq!(rs.lookup("u").unwrap().0.nice, Some(1));
    let msgs = messages(&rs);
    assert_eq!(msgs.len(), 2, "{:?}", rs.warnings);
    assert!(msgs.iter().any(|m| m.contains("unknown key") && m.contains("bogus")));
    assert_eq!(
        msgs.iter().filter(|m| **m == "cmdlines accepted but ignored").count(),
        1
    );
}

#[test]
fn duplicate_names_last_wins_in_sorted_path_order() {
    let fx = Fixture::new("dup");
    // Written in reverse of the sorted load order on purpose.
    fx.write("z.rules", "{\"name\": \"dup\", \"nice\": 1}\n{\"type\": \"t\", \"nice\": 1}\n{\"cgroup\": \"cg\", \"CPUQuota\": 10}\n");
    fx.write("a.rules", "{\"name\": \"dup\", \"nice\": 2}\n{\"type\": \"t\", \"nice\": 2}\n{\"cgroup\": \"cg\", \"CPUQuota\": 20}\n");
    let rs = fx.load_default();
    // a.rules loads first, z.rules last -> z.rules values win outright.
    assert_eq!(rs.lookup("dup").unwrap().0.nice, Some(1));
    assert_eq!(rs.types["t"].nice, Some(1));
    assert_eq!(rs.cgroups["cg"].cpu_quota_pct, Some(10));
}

#[test]
fn type_inheritance_override_and_null_suppression() {
    let fx = Fixture::new("merge");
    fx.write(
        "game.types",
        "{\"type\": \"game\", \"nice\": 5, \"ioclass\": \"idle\", \"ionice\": 3, \"cgroup\": \"games\"}\n",
    );
    fx.write(
        "r.rules",
        "{\"name\": \"inh\", \"type\": \"game\"}\n\
         {\"name\": \"ovr\", \"type\": \"game\", \"nice\": 10, \"ioclass\": \"realtime\"}\n\
         {\"name\": \"sup\", \"type\": \"game\", \"nice\": null, \"cgroup\": null}\n",
    );
    let rs = fx.load_default();

    let inh = rs.lookup("inh").unwrap().0;
    assert_eq!(inh.nice, Some(5));
    assert_eq!(inh.ioclass, Some(IoClass::Idle));
    assert_eq!(inh.ionice, Some(3));
    assert_eq!(inh.cgroup.as_deref(), Some("games"));

    let ovr = rs.lookup("ovr").unwrap().0;
    assert_eq!(ovr.nice, Some(10));
    assert_eq!(ovr.ioclass, Some(IoClass::Realtime));
    assert_eq!(ovr.ionice, Some(3)); // inherited
    assert_eq!(ovr.cgroup.as_deref(), Some("games"));

    // Explicit JSON null suppresses the type value instead of inheriting it.
    let sup = rs.lookup("sup").unwrap().0;
    assert_eq!(sup.nice, None);
    assert_eq!(sup.cgroup, None);
    assert_eq!(sup.ioclass, Some(IoClass::Idle)); // untouched fields still inherit
    assert_eq!(sup.ionice, Some(3));
}

#[test]
fn invalid_field_values_skip_the_rule() {
    let fx = Fixture::new("ranges");
    fx.write(
        "a.rules",
        "{\"name\": \"ok\", \"nice\": -20}\n\
         {\"name\": \"n1\", \"nice\": 50}\n\
         {\"name\": \"n2\", \"latency_nice\": -21}\n\
         {\"name\": \"n3\", \"sched\": \"deadline\"}\n\
         {\"name\": \"n4\", \"sched\": \"turbo\"}\n\
         {\"name\": \"n5\", \"rtprio\": 100}\n\
         {\"name\": \"n6\", \"ioclass\": \"ultra\"}\n\
         {\"name\": \"n7\", \"ionice\": 8}\n\
         {\"name\": \"n8\", \"oom_score_adj\": 1001}\n\
         {\"name\": \"n9\", \"autogroup\": 20}\n\
         {\"name\": \"n10\", \"name_regex\": 5}\n\
         {\"name\": \"n11\", \"type\": 5}\n\
         {\"name\": \"n12\", \"cpuset\": 5}\n",
    );
    let rs = fx.load_default();
    assert_eq!(rs.exact.len(), 1, "only the valid rule survives: {:?}", rs.exact.keys().collect::<Vec<_>>());
    assert_eq!(rs.lookup("ok").unwrap().0.nice, Some(-20));
    assert_eq!(rs.warnings.len(), 12, "{:?}", rs.warnings);
    assert!(messages(&rs)[2].contains("deadline"), "{:?}", rs.warnings);
}

#[test]
fn regex_is_unanchored_first_in_load_order_wins() {
    let fx = Fixture::new("regex");
    fx.write(
        "a.rules",
        "{\"name_regex\": \"foo\", \"nice\": 1}\n\
         {\"name_regex\": \"oo\", \"nice\": 2}\n",
    );
    let rs = fx.load_default();
    let (policy, kind) = rs.lookup("xxfooyy").unwrap();
    assert_eq!(kind, MatchKind::Regex);
    assert_eq!(policy.nice, Some(1)); // "foo" loaded first
    // Unanchored substring semantics: no anchors added.
    assert!(rs.lookup("xxfooyy").is_some());
    assert!(rs.lookup("nope").is_none());
}

#[test]
fn exact_match_beats_regex() {
    let fx = Fixture::new("exactwins");
    fx.write(
        "a.rules",
        "{\"name_regex\": \"foo\", \"nice\": 1}\n\
         {\"name\": \"xxfooyy\", \"nice\": 2}\n",
    );
    let rs = fx.load_default();
    let (policy, kind) = rs.lookup("xxfooyy").unwrap();
    assert_eq!(kind, MatchKind::Exact);
    assert_eq!(policy.nice, Some(2));
}

#[test]
fn invalid_regex_warns_and_only_drops_regex_registration() {
    let fx = Fixture::new("badregex");
    fx.write(
        "a.rules",
        "{\"name\": \"keep\", \"name_regex\": \"(?=x)\", \"nice\": 7}\n\
         {\"name_regex\": \"fo(o\", \"nice\": 8}\n",
    );
    let rs = fx.load_default();
    assert_eq!(rs.regexes.len(), 0);
    assert_eq!(rs.lookup("keep").unwrap().0.nice, Some(7)); // exact registration kept
    assert_eq!(rs.warnings.len(), 2);
    assert!(messages(&rs).iter().all(|m| m.contains("name_regex")), "{:?}", rs.warnings);
}

#[test]
fn lookup_is_case_sensitive_and_reports_match_kind() {
    let fx = Fixture::new("case");
    fx.write("a.rules", "{\"name\": \"Foo\", \"nice\": 1}\n{\"name_regex\": \"Bar\", \"nice\": 2}\n");
    let rs = fx.load_default();
    assert_eq!(rs.lookup("Foo").unwrap().1, MatchKind::Exact);
    assert!(rs.lookup("foo").is_none(), "name matching is case-sensitive");
    assert!(rs.lookup("bar").is_none(), "regex matching is case-sensitive");
    assert_eq!(rs.lookup("aBarb").unwrap().1, MatchKind::Regex);
    assert!(rs.lookup("zzz").is_none());
}

#[test]
fn cgroup_definitions_and_cpu_quota_validation() {
    let fx = Fixture::new("cgroups");
    fx.write(
        "a.cgroups",
        "{\"cgroup\": \"a\", \"CPUQuota\": 50}\n\
         {\"cgroup\": \"b\"}\n\
         {\"cgroup\": \"c\", \"CPUQuota\": 150}\n\
         {\"cgroup\": \"d\", \"CPUQuota\": null}\n",
    );
    let rs = fx.load_default();
    assert_eq!(rs.cgroups["a"].cpu_quota_pct, Some(50));
    assert_eq!(rs.cgroups["b"].cpu_quota_pct, None);
    assert_eq!(rs.cgroups["d"].cpu_quota_pct, None);
    assert!(!rs.cgroups.contains_key("c"), "out-of-range CPUQuota rejects the def");
    assert_eq!(rs.warnings.len(), 1);
    assert!(messages(&rs)[0].contains("CPUQuota"), "{:?}", rs.warnings);
}

#[test]
fn load_options_gate_file_kinds() {
    let fx = Fixture::new("gates");
    fx.write("a.rules", "{\"name\": \"r\", \"nice\": 1}\n");
    fx.write("b.types", "{\"type\": \"t\", \"nice\": 2}\n");
    fx.write("c.cgroups", "{\"cgroup\": \"cg\", \"CPUQuota\": 5}\n");
    let rs = fx.load(LoadOptions {
        rule_load: false,
        type_load: true,
        cgroup_load: true,
        cpuset_check: None,
    });
    assert!(rs.exact.is_empty());
    assert_eq!(rs.types.len(), 1);
    assert_eq!(rs.cgroups.len(), 1);
}

#[test]
fn type_defined_after_rule_file_is_still_inherited() {
    let fx = Fixture::new("twopass");
    // 00.rules sorts before 99.types; the type must still resolve.
    fx.write("00.rules", "{\"name\": \"r\", \"type\": \"late\"}\n");
    fx.write("99.types", "{\"type\": \"late\", \"nice\": 4}\n");
    let rs = fx.load_default();
    assert_eq!(rs.lookup("r").unwrap().0.nice, Some(4));
    assert!(rs.warnings.is_empty(), "{:?}", rs.warnings);
}

#[test]
fn cpuset_alias_names_map_to_alias() {
    let fx = Fixture::new("cpuset");
    fx.write(
        "a.rules",
        "{\"name\": \"a1\", \"cpuset\": \"big-cores\"}\n\
         {\"name\": \"a2\", \"cpuset\": \"llc-0\"}\n\
         {\"name\": \"a3\", \"cpuset\": \"node-2\"}\n\
         {\"name\": \"a4\", \"cpuset\": \"0-3,8-11\"}\n\
         {\"name\": \"a5\", \"cpuset\": \"llc-\"}\n\
         {\"name\": \"a6\", \"cpuset\": \"x3d-cache\"}\n",
    );
    let rs = fx.load_default();
    assert_eq!(rs.lookup("a1").unwrap().0.cpuset, Some(CpuSet::Alias("big-cores".into())));
    assert_eq!(rs.lookup("a2").unwrap().0.cpuset, Some(CpuSet::Alias("llc-0".into())));
    assert_eq!(rs.lookup("a3").unwrap().0.cpuset, Some(CpuSet::Alias("node-2".into())));
    assert_eq!(rs.lookup("a4").unwrap().0.cpuset, Some(CpuSet::Explicit("0-3,8-11".into())));
    assert_eq!(rs.lookup("a5").unwrap().0.cpuset, Some(CpuSet::Explicit("llc-".into())));
    assert_eq!(rs.lookup("a6").unwrap().0.cpuset, Some(CpuSet::Alias("x3d-cache".into())));
}

#[test]
fn sched_string_mapping() {
    let fx = Fixture::new("sched");
    fx.write(
        "a.rules",
        "{\"name\": \"s1\", \"sched\": \"fifo\", \"rtprio\": 50}\n\
         {\"name\": \"s2\", \"sched\": \"other\"}\n\
         {\"name\": \"s3\", \"sched\": \"normal\"}\n\
         {\"name\": \"s4\", \"sched\": \"batch\"}\n\
         {\"name\": \"s5\", \"sched\": \"idle\"}\n\
         {\"name\": \"s6\", \"sched\": \"rr\"}\n",
    );
    let rs = fx.load_default();
    assert_eq!(rs.lookup("s1").unwrap().0.sched, Some(SchedPolicy::Fifo));
    assert_eq!(rs.lookup("s1").unwrap().0.rtprio, Some(50));
    assert_eq!(rs.lookup("s2").unwrap().0.sched, Some(SchedPolicy::Normal));
    assert_eq!(rs.lookup("s3").unwrap().0.sched, Some(SchedPolicy::Normal));
    assert_eq!(rs.lookup("s4").unwrap().0.sched, Some(SchedPolicy::Batch));
    assert_eq!(rs.lookup("s5").unwrap().0.sched, Some(SchedPolicy::Idle));
    assert_eq!(rs.lookup("s6").unwrap().0.sched, Some(SchedPolicy::Rr));
}

#[test]
fn rules_that_can_never_match_are_named_in_the_load() {
    let f = Fixture::new("never-match");
    f.write(
        "50-a.rules",
        "{\"name\": \"\", \"nice\": 1}\n\
         {\"name\": \"cc1*\", \"nice\": 2}\n",
    );
    let rs = f.load_default();
    let m = messages(&rs);
    assert!(
        m.iter().any(|x| x.contains("empty name can never match")),
        "{m:?}"
    );
    assert!(
        m.iter().any(|x| x.contains("patterns need name_regex")),
        "{m:?}"
    );
    assert_eq!(rs.exact.len(), 2, "both still load");
}

#[test]
fn a_rules_directory_file_that_is_never_read_is_reported_not_ignored() {
    let f = Fixture::new("skip-ext");
    f.write("50-a.rules", "{\"name\": \"ok\", \"nice\": 1}\n");
    f.write("60-b.json", "{\"name\": \"json\", \"nice\": 1}\n");
    f.write("70-c.rules.bak", "{\"name\": \"bak\", \"nice\": 1}\n");
    f.write("README.md", "# notes, not a corpus\n");
    let rs = f.load_default();
    let skipped: Vec<_> = rs
        .warnings
        .iter()
        .filter(|w| w.message.contains("skipped"))
        .map(|w| w.file.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(skipped, vec!["60-b.json", "70-c.rules.bak"], "{skipped:?}");
    assert_eq!(rs.exact.len(), 1, "only the .rules file is loaded");
}

/// The host half of the cpuset check must reach a *well-formed* explicit list
/// too: `16-31` on a 12-CPU host parses, and `sched_setaffinity` then answers
/// EINVAL, so the loader is the only place that can say so.
#[test]
fn a_well_formed_explicit_cpuset_is_checked_against_the_host() {
    let f = Fixture::new("cpuset-explicit-host");
    f.write(
        "10-a.rules",
        "{\"name\": \"foo\", \"cpuset\": \"24-27\"}\n",
    );
    let opts = LoadOptions {
        cpuset_check: Some(|cs: &CpuSet| match cs {
            CpuSet::Explicit(_) => Err("none of the listed CPUs is online on this host".to_string()),
            _ => Ok(()),
        }),
        ..LoadOptions::default()
    };
    let rs = f.load(opts);
    let m = messages(&rs);
    assert!(
        m.iter().any(|x| x.contains("cannot be resolved on this host")
            && x.contains("24-27")),
        "{m:?}"
    );
}

/// Matching is on the executable's basename (then `argv[0]`, then comm), so a
/// rule that spells a path is dead weight for any process whose `exe` or
/// `argv[0]` yields a basename; `lint` has to say so rather than pass it
/// silently.
#[test]
fn a_name_with_a_path_separator_is_reported_at_load() {
    let f = Fixture::new("path-name");
    f.write(
        "10-a.rules",
        "{\"name\": \"/usr/bin/foo\", \"nice\": 5}\n         {\"name\": \"foo\", \"nice\": 5}\n",
    );
    let rs = f.load_default();
    let m = messages(&rs);
    // Exactly one rule is wrong: the plain `foo` beside it must stay silent.
    assert_eq!(m.len(), 1, "{m:?}");
    assert!(m[0].contains("path separator") && m[0].contains("/usr/bin/foo"), "{m:?}");
    // `comm` is the key whenever neither `exe` nor `argv[0]` yields a
    // basename, so the separator shapes include a zombie and a task whose
    // `exe` is unreadable with an `argv[0]` that yields none; the message must
    // not claim impossibility.
    assert!(!m[0].contains("never apply"), "{m:?}");

    // A rule that also carries a regex still applies through it, so its dead
    // exact key is not a lint failure.
    let f = Fixture::new("path-name-regex");
    f.write(
        "10-a.rules",
        "{\"name\": \"/usr/bin/sleep\", \"name_regex\": \"^sleep$\", \"nice\": 5}\n",
    );
    let rs = f.load_default();
    assert!(messages(&rs).is_empty(), "{:?}", rs.warnings);
    assert_eq!(
        rs.lookup("sleep").map(|(p, _)| p.nice),
        Some(Some(5)),
        "the regex still matches"
    );

    // An uncompiled regex is no escape hatch: the exact key is still dead, so
    // it is named alongside the invalid-pattern warning.
    let f = Fixture::new("path-name-bad-regex");
    f.write(
        "10-a.rules",
        "{\"name\": \"/usr/bin/foo\", \"name_regex\": \"[\", \"nice\": 5}\n",
    );
    let rs = f.load_default();
    let m = messages(&rs);
    assert!(m.iter().any(|x| x.contains("invalid name_regex")), "{m:?}");
    assert!(
        m.iter().any(|x| x.contains("path separator") && x.contains("/usr/bin/foo")),
        "the dead exact key is named too: {m:?}"
    );

    // A regex may contain a separator and still match a key
    // (`^(/usr/bin/)?foo$` matches `foo`), so a pattern is never judged dead
    // from its text: no warning, and lint stays green.
    let f = Fixture::new("path-name-live-regex");
    f.write(
        "10-a.rules",
        "{\"name_regex\": \"^(/usr/bin/)?foo$\", \"nice\": 5}\n\
         {\"name\": \"foo\", \"name_regex\": \"^/usr/bin/foo$\", \"nice\": 6}\n",
    );
    let rs = f.load_default();
    assert!(messages(&rs).is_empty(), "{:?}", rs.warnings);
    assert_eq!(
        rs.lookup("foo").map(|(p, kind)| (p.nice, kind)),
        Some((Some(6), MatchKind::Exact)),
        "the exact key still wins for `foo`"
    );

    // The recorded limit: no *basename* key can match this rule (the exact key
    // is a path, the pattern needs one), so it is dead weight for every process
    // whose `exe` or `argv[0]` yields a basename — yet a pattern's reach is
    // undecidable from its text, so it loads silently. The rule itself is
    // registered: a key carrying a separator would match it. Keep this pin
    // honest if a language check ever replaces the limit.
    let f = Fixture::new("path-name-both-dead");
    f.write(
        "10-a.rules",
        "{\"name\": \"/usr/bin/foo\", \"name_regex\": \"^/usr/bin/foo$\", \"nice\": 5}\n",
    );
    let rs = f.load_default();
    assert!(messages(&rs).is_empty(), "{:?}", rs.warnings);
    assert!(rs.lookup("foo").is_none(), "no basename key matches it");
    assert_eq!(
        rs.lookup("/usr/bin/foo").map(|(p, kind)| (p.nice, kind)),
        Some((Some(5), MatchKind::Exact)),
        "the rule is registered; only a basename key is out of reach"
    );
}

#[test]
fn duplicate_names_and_undefined_cgroups_are_reported() {
    let f = Fixture::new("dup-cgroup");
    f.write(
        "10-a.rules",
        "{\"name\": \"zz\", \"nice\": 15}\n\
         {\"name\": \"cgrule\", \"cgroup\": \"nope\"}\n",
    );
    f.write("90-z.rules", "{\"name\": \"zz\", \"nice\": 2}\n");
    let rs = f.load_default();
    let m = messages(&rs);
    assert!(
        m.iter().any(|x| x.contains("duplicate name \"zz\"")),
        "{m:?}"
    );
    assert!(m.iter().any(|x| x.contains("unknown cgroup \"nope\"")), "{m:?}");
    // Parity is unchanged: the later definition still wins.
    assert_eq!(rs.lookup("zz").unwrap().0.nice, Some(2));
}

/// A cpuset that cannot resolve is dropped silently at apply time (the
/// process keeps its affinity), so the loader must reject it — the documented
/// not-usable aliases and malformed explicit lists, with file:line.
#[test]
fn unusable_and_malformed_cpusets_are_reported_at_load() {
    let fx = Fixture::new("cpuset-warn");
    fx.write(
        "a.rules",
        "{\"name\": \"tar\", \"cpuset\": \"turbo-cores\"}\n\
         {\"name\": \"xz\", \"cpuset\": \"0-4-9\"}\n\
         {\"name\": \"ok\", \"cpuset\": \"0-3,8\"}\n\
         {\"name\": \"big\", \"cpuset\": \"big-cores\"}\n\
         {\"name\": \"amd\", \"cpuset\": \"x3d-cache\"}\n",
    );
    let rs = fx.load_default();
    let msgs = messages(&rs);
    assert_eq!(msgs.len(), 3, "{msgs:?}");
    assert!(
        msgs.iter().any(|m| m.contains("turbo-cores") && m.contains("not usable")),
        "{msgs:?}"
    );
    assert!(
        msgs.iter().any(|m| m.contains("0-4-9") && m.contains("invalid cpuset")),
        "{msgs:?}"
    );
    assert!(
        msgs.iter().any(|m| m.contains("x3d-cache") && m.contains("not usable")),
        "{msgs:?}"
    );
    for w in &rs.warnings {
        assert!(w.file.ends_with("a.rules"), "{w:?}");
        assert!(w.line > 0, "{w:?}");
    }
}

/// `"bias": false` is the per-name opt-out from the learned axis; it travels
/// through types like every other policy field, and a non-boolean is a load
/// error rather than a silent no-op.
#[test]
fn bias_opt_out_parses_inherits_and_type_checks() {
    let fx = Fixture::new("bias-optout");
    fx.write(
        "a.types",
        "{\"type\": \"pinned\", \"nice\": -5, \"bias\": false}\n",
    );
    fx.write(
        "a.rules",
        "{\"name\": \"game\", \"type\": \"pinned\"}\n\
         {\"name\": \"tool\", \"bias\": true}\n\
         {\"name\": \"bad\", \"bias\": \"no\"}\n",
    );
    let rs = fx.load_default();
    let (game, _) = rs.lookup("game").unwrap();
    assert_eq!(game.bias, Some(false));
    assert_eq!(game.nice, Some(-5), "the rest of the type still inherits");
    let (tool, _) = rs.lookup("tool").unwrap();
    assert_eq!(tool.bias, Some(true));
    assert!(rs.lookup("bad").is_none(), "a non-boolean bias rejects the line");
    assert!(
        messages(&rs)
            .iter()
            .any(|m| m.contains("bias: expected boolean")),
        "{:?}",
        rs.warnings
    );
}

/// A host-dependent alias resolves or it does not — only the caller's
/// resolver knows. The loader must surface its refusal with file:line, and
/// must not invent one when no resolver is supplied.
#[test]
fn the_host_cpuset_check_is_what_reports_a_host_dependent_alias() {
    fn uniform_box(cs: &aranicy_model::CpuSet) -> Result<(), String> {
        match cs {
            aranicy_model::CpuSet::Alias(a) if &**a == "big-cores" => {
                Err("no big/little asymmetry".to_string())
            }
            _ => Ok(()),
        }
    }
    let fx = Fixture::new("cpuset-host");
    fx.write(
        "a.rules",
        "{\"name\": \"tar\", \"cpuset\": \"big-cores\"}\n\
         {\"name\": \"ok\", \"cpuset\": \"0-1\"}\n",
    );
    let rs = fx.load(LoadOptions {
        cpuset_check: Some(uniform_box),
        ..LoadOptions::default()
    });
    assert_eq!(rs.warnings.len(), 1, "{:?}", rs.warnings);
    let w = &rs.warnings[0];
    assert!(w.file.ends_with("a.rules") && w.line == 1, "{w:?}");
    assert!(
        w.message.contains("big-cores") && w.message.contains("cannot be resolved on this host"),
        "{}",
        w.message
    );

    // Without a resolver the static checks stand alone: no invented warning.
    let fx2 = Fixture::new("cpuset-nohost");
    fx2.write("a.rules", "{\"name\": \"tar\", \"cpuset\": \"big-cores\"}\n");
    assert!(fx2.load_default().warnings.is_empty());
}
