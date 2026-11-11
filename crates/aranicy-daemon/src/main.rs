//! aranicy daemon + CLI.
//!
//! CLI surface is a superset of ananicy-cpp's (docs/COMPATIBILITY.md):
//! `start`, `dump {rules,types,cgroups,proc,autogroup,bias}`, `--reload`, plus
//! `lint` and `match-test` (dry-run) which ananicy-cpp lacks.

use aranicy_procname as procname;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod bias;
mod bias_state;
mod classify;
mod config;
mod daemon;
mod decision;
mod dump;
mod event_client;
mod load;
mod paths;
mod pidfile;
mod sampler;
mod sdnotify;

use aranicy_rules::MatchKind;

#[derive(Parser, Debug)]
#[command(name = "aranicy", version, about = "Auto-nice daemon (eBPF exec events)")]
struct Cli {
    /// Rules directory (env: ANANICY_CPP_CONFDIR / ANANICY_RS_CONFDIR).
    #[arg(long, global = true)]
    config_dir: Option<PathBuf>,
    /// Daemon config file (env: ANANICY_CPP_CONF / ANANICY_RS_CONF).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Reload the running daemon's config AND rules (SIGHUP-equivalent).
    #[arg(long, global = true)]
    reload: bool,
    #[command(subcommand)]
    action: Option<Action>,
}

#[derive(Subcommand, Debug)]
enum Action {
    /// Run the daemon (root required).
    Start,
    /// Print loaded state as JSON (works unprivileged).
    Dump {
        #[arg(value_parser = ["rules", "types", "cgroups", "proc", "autogroup", "bias"])]
        what: String,
    },
    /// Validate rules and report problems without running.
    ///
    /// Exit 1 on any finding: an error (a rule that would not load) or a
    /// warning (a rule that loads but can never match, a file that is never
    /// read). A corpus that is green is a corpus that will produce policy.
    Lint,
    /// Show which rule a process name would match, without applying anything.
    MatchTest {
        /// Process name as the daemon derives it (basename(argv[0])).
        name: String,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    // Resolution order: flags > ANANICY_RS_CONF* > ANANICY_CPP_CONF* > /etc.
    let paths = paths::resolve_from_env(cli.config, cli.config_dir);

    if cli.reload {
        return reload_running();
    }

    match cli.action.unwrap_or(Action::Start) {
        Action::Start => daemon::start(paths),
        Action::Dump { what } => dump::run(&what, &paths),
        Action::Lint => lint(&paths),
        Action::MatchTest { name } => match_test(&name, &paths),
    }
}

/// `aranicy --reload`: SIGHUP the daemon recorded in the pidfile.
fn reload_running() -> anyhow::Result<()> {
    let pidfile = paths::pidfile_path();
    let Some(pid) = pidfile::read_pid(&pidfile) else {
        anyhow::bail!(
            "no running aranicy daemon found ({} is missing)",
            pidfile.display()
        );
    };
    let r = unsafe { libc::kill(pid as libc::pid_t, libc::SIGHUP) };
    if r != 0 {
        anyhow::bail!(
            "failed to signal aranicy (pid {pid}): {}",
            std::io::Error::last_os_error()
        );
    }
    println!("sent SIGHUP to aranicy (pid {pid})");
    Ok(())
}

/// Load everything, report every warning to stderr; exit 1 on any warning.
fn lint(paths: &paths::Paths) -> anyhow::Result<()> {
    let (cfg, rules) = match load::load_all(paths) {
        Ok(loaded) => loaded,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    let mut warned = false;
    for w in &cfg.warnings {
        // A config file that does not exist is not a corpus defect: the daemon
        // runs on defaults in that case too. Reporting it as a failure made
        // `lint` unusable as a gate on a rules-only checkout.
        if w.contains("not found; using defaults") {
            eprintln!("note: config: {w}");
            continue;
        }
        eprintln!("warning: config: {w}");
        warned = true;
    }
    for w in &rules.warnings {
        eprintln!("warning: {}:{}: {}", w.file.display(), w.line, w.message);
        warned = true;
    }
    // A green lint over an empty corpus is the worst answer this command can
    // give: it is the state every fresh install starts in.
    // Only a corpus with nothing in it at all: a `.types`- or
    // `.cgroups`-only tree is legitimate (the rules that use them may live in
    // another directory), and it must stay green.
    if cfg.rule_load && load::rule_count(&rules) == 0 && rules.types.is_empty() && rules.cgroups.is_empty() {
        eprintln!(
            "warning: 0 rules loaded from {}: this corpus will apply nothing",
            paths.config_dir.display()
        );
        warned = true;
    }
    if warned {
        eprintln!("lint: warnings found");
        std::process::exit(1);
    }
    println!(
        "OK: {} rules, {} types, {} cgroups",
        load::rule_count(&rules),
        rules.types.len(),
        rules.cgroups.len()
    );
    Ok(())
}

/// Dry-run matcher: never applies anything, no root required.
fn match_test(input: &str, paths: &paths::Paths) -> anyhow::Result<()> {
    let (_cfg, rules) = load::load_all(paths)?;
    let name = input;
    // Look the argument up as given first: a rule may legitimately name a task
    // whose comm contains a slash (`kworker/0:1-events`), and stripping to the
    // basename would report a name the task does not have. Path arguments
    // (`/usr/bin/foo`) still work through the basename fallback.
    let name = if rules.lookup(name).is_some() {
        name
    } else {
        // Two attempts, and the fallback only decides which name matched - an
        // argument that matches nothing is reported as the user typed it.
        match procname::normalize_cli_name(name) {
            base if rules.lookup(base).is_some() => base,
            _ => name,
        }
    };
    match rules.lookup(name) {
        Some((policy, kind)) => {
            let kind = match kind {
                MatchKind::Exact => "exact",
                MatchKind::Regex => "regex",
            };
            let out = serde_json::json!({
                "name": name,
                "match": kind,
                "policy": dump::policy_json(policy),
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
            Ok(())
        }
        None => {
            eprintln!("match-test: no rule matches {input:?}");
            std::process::exit(1);
        }
    }
}
