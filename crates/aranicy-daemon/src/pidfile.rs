//! Singleton pidfile with `flock(2)` (mandatory lock, `LOCK_NB`).
//!
//! The lock is released automatically when the fd closes, so a crashed daemon
//! leaves no stale lock — only a stale file, which the next start overwrites.
//! The file content is the owner pid for `aranicy --reload` (SIGHUP).

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

/// Held pidfile: unlocked and removed on drop.
pub struct PidFile {
    file: File,
    path: PathBuf,
}

#[derive(Debug)]
pub enum PidFileError {
    /// Another daemon holds the lock; `pid` is its recorded pid (if readable).
    AlreadyRunning { pid: Option<u32>, path: PathBuf },
    Io(std::io::Error),
}

impl std::fmt::Display for PidFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRunning { pid, path } => write!(
                f,
                "another aranicy instance is already running (pid {}); if that pid is stale, remove {}",
                pid.map(|p| p.to_string()).unwrap_or_else(|| "unknown".into()),
                path.display()
            ),
            Self::Io(e) => write!(f, "pidfile: {e}"),
        }
    }
}

impl std::error::Error for PidFileError {}

impl From<std::io::Error> for PidFileError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Acquire the singleton lock at `path`, recording our pid.
pub fn acquire(path: &Path) -> Result<PidFile, PidFileError> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    match flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) {
        Ok(()) => {}
        Err(e) if e.raw_os_error() == Some(libc::EWOULDBLOCK) => {
            return Err(PidFileError::AlreadyRunning {
                pid: read_pid_from(&mut file),
                path: path.to_path_buf(),
            });
        }
        Err(e) => return Err(PidFileError::Io(e)),
    }
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    writeln!(file, "{}", std::process::id())?;
    file.flush()?;
    Ok(PidFile {
        file,
        path: path.to_path_buf(),
    })
}

impl Drop for PidFile {
    fn drop(&mut self) {
        // Unlink first, release the lock second (closing the fd does the
        // release). The reverse order opens a window in which a starting
        // daemon locks this inode and then has its path removed by us: two
        // live daemons, one of them invisible to `--reload`.
        let _ = std::fs::remove_file(&self.path);
        let _ = flock(self.file.as_raw_fd(), libc::LOCK_UN);
    }
}

/// Read the recorded pid without taking the lock (`--reload`).
pub fn read_pid(path: &Path) -> Option<u32> {
    read_pid_at(path).ok().flatten()
}

/// [`read_pid`] with the failure kept: a pidfile that exists but cannot be
/// read (mode 0600, a directory, EACCES) is not the same fact as "no daemon".
fn read_pid_at(path: &Path) -> std::io::Result<Option<u32>> {
    match std::fs::File::open(path) {
        Ok(mut file) => Ok(read_pid_from(&mut file)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn read_pid_from(file: &mut File) -> Option<u32> {
    let mut buf = [0u8; 32];
    file.seek(SeekFrom::Start(0)).ok()?;
    let n = file.read(&mut buf).ok()?;
    std::str::from_utf8(&buf[..n])
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()
}

/// Why `--reload` refused to signal the pid it read. A stale pidfile names a
/// recycled pid, and SIGHUP's default action is *terminate*: the check has to
/// happen before the signal, not after (round-3 dana-15).
#[derive(Debug, PartialEq, Eq)]
pub enum ReloadTargetError {
    Missing { path: PathBuf },
    /// The file names a pid, but nothing holds the singleton lock: stale.
    Stale { pid: u32, path: PathBuf },
    /// The pid is alive but its executable is not this one.
    NotAranicy { pid: u32, exe: String },
    /// `/proc/<pid>/exe` unreadable (the process is gone, or /proc hides it).
    Unreadable { pid: u32, why: String },
    /// The file (or the link inside `/proc`) is only readable as root. Told
    /// apart from [`Self::Unreadable`] so an unprivileged `--reload` says
    /// what to do instead of implying the daemon is unidentifiable.
    PermissionDenied { path: PathBuf, what: String },
}

impl std::fmt::Display for ReloadTargetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing { path } => write!(
                f,
                "no running aranicy daemon found ({} is missing)",
                path.display()
            ),
            Self::Stale { pid, path } => write!(
                f,
                "{} names pid {pid}, but no daemon holds its lock; refusing to signal a \
                 recycled pid (remove the file if no daemon is running)",
                path.display()
            ),
            Self::NotAranicy { pid, exe } => write!(
                f,
                "pid {pid} from the pidfile is {exe}, not aranicy; refusing to signal it"
            ),
            Self::Unreadable { pid, why } => write!(
                f,
                "cannot identify pid {pid} from the pidfile ({why}); refusing to signal it"
            ),
            Self::PermissionDenied { path, what } => write!(
                f,
                "cannot read {what} ({}) without privileges; \"aranicy --reload\" has to run \
                 as root to identify the daemon",
                path.display()
            ),
        }
    }
}

/// Resolve the pid `--reload` may signal: the file must exist, a process must
/// hold its lock (a daemon, not a leftover file), and that pid's executable
/// must be this one.
pub fn reload_target(path: &Path) -> Result<u32, ReloadTargetError> {
    let pid = match read_pid_at(path) {
        Ok(Some(pid)) => pid,
        Ok(None) => {
            return Err(ReloadTargetError::Missing {
                path: path.to_path_buf(),
            });
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err(ReloadTargetError::PermissionDenied {
                path: path.to_path_buf(),
                what: "the pidfile".to_string(),
            });
        }
        Err(e) => {
            return Err(ReloadTargetError::Unreadable {
                pid: 0,
                why: format!("{} is unreadable ({e})", path.display()),
            });
        }
    };
    if !lock_is_held(path) {
        return Err(ReloadTargetError::Stale {
            pid,
            path: path.to_path_buf(),
        });
    }
    let exe = match executable_name(pid) {
        Ok(name) => name,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err(ReloadTargetError::PermissionDenied {
                path: PathBuf::from(format!("/proc/{pid}/exe")),
                what: "the daemon's executable link".to_string(),
            });
        }
        Err(e) => {
            return Err(ReloadTargetError::Unreadable {
                pid,
                why: format!("/proc/{pid}/exe is not readable ({e})"),
            });
        }
    };
    let ours = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_os_string()));
    if !same_binary(ours.as_deref(), exe.as_os_str()) {
        return Err(ReloadTargetError::NotAranicy {
            pid,
            exe: exe.to_string_lossy().into_owned(),
        });
    }
    Ok(pid)
}

/// `flock(LOCK_EX|LOCK_NB)` from outside: EWOULDBLOCK means a daemon holds it.
fn lock_is_held(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    match flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) {
        Ok(()) => {
            // Ours for a moment: nothing held it. Released with the fd.
            let _ = flock(file.as_raw_fd(), libc::LOCK_UN);
            false
        }
        Err(e) => e.raw_os_error() == Some(libc::EWOULDBLOCK),
    }
}

/// Basename of `/proc/<pid>/exe`, with the " (deleted)" suffix the kernel adds
/// after an upgrade removed the running file.
fn executable_name(pid: u32) -> std::io::Result<std::ffi::OsString> {
    let link = std::fs::read_link(format!("/proc/{pid}/exe"))?;
    let name = link.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "the link has no file name")
    })?;
    Ok(strip_deleted(name.to_os_string()))
}

/// Do these name the same binary? Both sides may carry the kernel's
/// `" (deleted)"` marker, so the comparison — not merely the link read — has
/// to normalize it.
fn same_binary(ours: Option<&std::ffi::OsStr>, theirs: &std::ffi::OsStr) -> bool {
    match ours {
        Some(o) => strip_deleted(o.to_os_string()) == strip_deleted(theirs.to_os_string()),
        None => false,
    }
}

/// Drop the kernel's `" (deleted)"` suffix. It appears on `/proc/<pid>/exe`
/// after an upgrade unlinks the running binary, and on the caller's own
/// `current_exe()` in the same situation — both sides have to agree, or a
/// reload run from a replaced binary refuses its own daemon.
fn strip_deleted(name: std::ffi::OsString) -> std::ffi::OsString {
    match name.to_string_lossy().strip_suffix(" (deleted)") {
        Some(real) => real.into(),
        None => name,
    }
}

fn flock(fd: i32, op: i32) -> std::io::Result<()> {
    let r = unsafe { libc::flock(fd, op) };
    if r == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::current_dir()
            .expect("cwd")
            .join("target")
            .join(format!("pidfile_test_{}_{}", std::process::id(), tag));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn a_missing_pidfile_is_a_named_refusal() {
        let dir = scratch("missing");
        let err = reload_target(&dir.join("none.pid")).expect_err("no file");
        assert!(matches!(err, ReloadTargetError::Missing { .. }), "{err:?}");
        assert!(err.to_string().contains("no running aranicy"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The defect: an unheld pidfile means a recycled pid, and the old code
    /// SIGHUPed it anyway (pid 1 here would have been a very bad day).
    #[test]
    fn a_pidfile_no_daemon_holds_is_stale_and_never_signalled() {
        let dir = scratch("stale");
        let path = dir.join("aranicy.pid");
        std::fs::write(&path, "1\n").expect("write pid");
        let err = reload_target(&path).expect_err("stale");
        assert_eq!(
            err,
            ReloadTargetError::Stale {
                pid: 1,
                path: path.clone()
            }
        );
        assert!(err.to_string().contains("refusing to signal"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_locked_pidfile_naming_a_foreign_process_is_refused() {
        let dir = scratch("foreign");
        let path = dir.join("aranicy.pid");
        let held = acquire(&path).expect("lock");
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        std::fs::write(&path, format!("{}\n", child.id())).expect("write foreign pid");
        let err = reload_target(&path).expect_err("foreign");
        assert!(
            matches!(err, ReloadTargetError::NotAranicy { pid, .. } if pid == child.id()),
            "{err:?}"
        );
        assert!(err.to_string().contains("not aranicy"));
        let _ = child.kill();
        let _ = child.wait();
        drop(held);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The kernel marks an unlinked executable with `" (deleted)"`; the
    /// caller's own `current_exe()` carries the same suffix after an upgrade.
    /// Both sides must strip it, or `--reload` from the replaced binary
    /// refuses its own daemon.
    #[test]
    fn both_sides_of_the_executable_comparison_strip_the_deleted_marker() {
        use std::ffi::OsStr;
        assert!(
            same_binary(Some(OsStr::new("aranicy (deleted)")), OsStr::new("aranicy")),
            "a reload run from a replaced binary must accept its daemon"
        );
        assert!(same_binary(Some(OsStr::new("aranicy")), OsStr::new("aranicy (deleted)")));
        assert!(!same_binary(Some(OsStr::new("aranicy")), OsStr::new("sleep")));
        assert!(!same_binary(None, OsStr::new("aranicy")), "unknown is never a match");
    }

    /// A pidfile that exists but needs privileges is not a missing one: the
    /// two facts send the operator to different places.
    #[test]
    fn an_unreadable_pidfile_is_named_as_a_privilege_problem() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("perm");
        let path = dir.join("aranicy.pid");
        std::fs::write(&path, "1\n").expect("write pid");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        let err = reload_target(&path).expect_err("unreadable");
        assert!(
            matches!(err, ReloadTargetError::PermissionDenied { .. }),
            "{err:?}"
        );
        assert!(err.to_string().contains("as root"), "{err}");
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The singleton survives its own release: the path is unlinked on drop,
    /// and a second run can take a fresh file. (The *order* of the unlink and
    /// the lock release is a race and cannot be pinned from outside; it is
    /// fixed in `Drop` instead.)
    #[test]
    fn dropping_the_pidfile_unlinks_it_and_frees_the_singleton() {
        let dir = scratch("drop");
        let path = dir.join("aranicy.pid");
        let held = acquire(&path).expect("lock");
        assert!(
            matches!(
                acquire(&path),
                Err(PidFileError::AlreadyRunning { .. })
            ),
            "a second daemon must be refused while the first runs"
        );
        drop(held);
        assert!(!path.exists(), "drop removes the pidfile");
        let again = acquire(&path).expect("a fresh run takes a fresh file");
        drop(again);
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_locked_pidfile_naming_this_binary_is_accepted() {
        let dir = scratch("accept");
        let path = dir.join("aranicy.pid");
        let held = acquire(&path).expect("lock");
        assert_eq!(reload_target(&path), Ok(std::process::id()));
        drop(held);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
