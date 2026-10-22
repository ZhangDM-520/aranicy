//! aranicy daemon + CLI.
//!
//! CLI surface is a superset of ananicy-cpp's (docs/COMPATIBILITY.md):
//! `start`, `dump {rules,types,cgroups,proc,autogroup}`, `--reload`, plus
//! `lint` and `match-test` (dry-run) which ananicy-cpp lacks.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod config;
mod daemon;
mod dump;
mod load;
mod paths;
mod pidfile;
mod procname;
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
        #[arg(value_parser = ["rules", "types", "cgroups", "proc", "autogroup"])]
        what: String,
    },
    /// Validate rules and report problems without running (exit 1 on errors).
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
        eprintln!("warning: config: {w}");
        warned = true;
    }
    for w in &rules.warnings {
        eprintln!("warning: {}:{}: {}", w.file.display(), w.line, w.message);
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
fn match_test(name: &str, paths: &paths::Paths) -> anyhow::Result<()> {
    let (_cfg, rules) = load::load_all(paths)?;
    let name = procname::normalize_cli_name(name);
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
            eprintln!("match-test: no rule matches {name:?}");
            std::process::exit(1);
        }
    }
}
