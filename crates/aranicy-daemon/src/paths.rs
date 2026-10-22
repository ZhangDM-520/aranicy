//! Config/rules path resolution and the pidfile location.
//!
//! Resolution order (docs/COMPATIBILITY.md): flags > `ANANICY_RS_CONF*` >
//! `ANANICY_CPP_CONF*` (compat) > defaults. The config file default is
//! `<rules-root>/ananicy.conf`, which for the default rules root is
//! `/etc/ananicy.d/ananicy.conf`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const DEFAULT_CONFDIR: &str = "/etc/ananicy.d";
pub const DEFAULT_RUN_DIR: &str = "/run";

/// Fully resolved daemon input paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// `ananicy.conf` location.
    pub config: PathBuf,
    /// Rules root (`*.rules` / `*.types` / `*.cgroups`).
    pub config_dir: PathBuf,
}

/// Resolve with an injected environment lookup (testable without mutating
/// process env). Precedence per key: CLI flag > `ANANICY_RS_*` > `ANANICY_CPP_*`
/// > default.
pub fn resolve(
    cli_config: Option<PathBuf>,
    cli_config_dir: Option<PathBuf>,
    env: impl Fn(&str) -> Option<OsString>,
) -> Paths {
    let config_dir = cli_config_dir
        .or_else(|| {
            env("ANANICY_RS_CONFDIR")
                .or_else(|| env("ANANICY_CPP_CONFDIR"))
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFDIR));
    let config = cli_config
        .or_else(|| {
            env("ANANICY_RS_CONF")
                .or_else(|| env("ANANICY_CPP_CONF"))
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| config_dir.join("ananicy.conf"));
    Paths { config, config_dir }
}

/// Same, reading the real process environment.
pub fn resolve_from_env(cli_config: Option<PathBuf>, cli_config_dir: Option<PathBuf>) -> Paths {
    resolve(cli_config, cli_config_dir, |key| std::env::var_os(key))
}

/// Pidfile location helper (`<run_dir>/aranicy.pid`).
pub fn pidfile_path_in(run_dir: &Path) -> PathBuf {
    run_dir.join("aranicy.pid")
}

/// The daemon's singleton pidfile path.
pub fn pidfile_path() -> PathBuf {
    pidfile_path_in(Path::new(DEFAULT_RUN_DIR))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(map: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: Vec<(String, String)> = map
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key: &str| {
            map.iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn defaults_when_no_flags_or_env() {
        let p = resolve(None, None, env(&[]));
        assert_eq!(p.config_dir, PathBuf::from("/etc/ananicy.d"));
        assert_eq!(p.config, PathBuf::from("/etc/ananicy.d/ananicy.conf"));
    }

    #[test]
    fn config_default_follows_rules_root() {
        let p = resolve(None, Some(PathBuf::from("/srv/rules")), env(&[]));
        assert_eq!(p.config, PathBuf::from("/srv/rules/ananicy.conf"));
    }

    #[test]
    fn rs_env_beats_cpp_env_and_flags_beat_env() {
        let vars = [
            ("ANANICY_RS_CONFDIR", "/rs/dir"),
            ("ANANICY_CPP_CONFDIR", "/cpp/dir"),
            ("ANANICY_RS_CONF", "/rs/conf"),
            ("ANANICY_CPP_CONF", "/cpp/conf"),
        ];
        let p = resolve(None, None, env(&vars));
        assert_eq!(p.config_dir, PathBuf::from("/rs/dir"));
        assert_eq!(p.config, PathBuf::from("/rs/conf"));

        let p = resolve(
            Some(PathBuf::from("/flag/conf")),
            Some(PathBuf::from("/flag/dir")),
            env(&vars),
        );
        assert_eq!(p.config_dir, PathBuf::from("/flag/dir"));
        assert_eq!(p.config, PathBuf::from("/flag/conf"));
    }

    #[test]
    fn cpp_env_is_compat_fallback() {
        let e = env(&[
            ("ANANICY_CPP_CONFDIR", "/cpp/dir"),
            ("ANANICY_CPP_CONF", "/cpp/conf"),
        ]);
        let p = resolve(None, None, e);
        assert_eq!(p.config_dir, PathBuf::from("/cpp/dir"));
        assert_eq!(p.config, PathBuf::from("/cpp/conf"));
    }

    #[test]
    fn pidfile_path_helper() {
        assert_eq!(
            pidfile_path_in(Path::new("/run")),
            PathBuf::from("/run/aranicy.pid")
        );
        assert_eq!(pidfile_path(), PathBuf::from("/run/aranicy.pid"));
    }
}
