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
        let _ = flock(self.file.as_raw_fd(), libc::LOCK_UN);
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Read the recorded pid without taking the lock (`--reload`).
pub fn read_pid(path: &Path) -> Option<u32> {
    let mut file = std::fs::File::open(path).ok()?;
    read_pid_from(&mut file)
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
        }
    }
}

/// Resolve the pid `--reload` may signal: the file must exist, a process must
/// hold its lock (a daemon, not a leftover file), and that pid's executable
/// must be this one.
pub fn reload_target(path: &Path) -> Result<u32, ReloadTargetError> {
    let Some(pid) = read_pid(path) else {
        return Err(ReloadTargetError::Missing {
            path: path.to_path_buf(),
        });
    };
    if !lock_is_held(path) {
        return Err(ReloadTargetError::Stale {
            pid,
            path: path.to_path_buf(),
        });
    }
    let Some(exe) = executable_name(pid) else {
        return Err(ReloadTargetError::Unreadable {
            pid,
            why: format!("/proc/{pid}/exe is not readable"),
        });
    };
    let ours = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_os_string()));
    if ours.as_deref() != Some(exe.as_os_str()) {
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
fn executable_name(pid: u32) -> Option<std::ffi::OsString> {
    let link = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    let name = link.file_name()?.to_os_string();
    match name.to_string_lossy().strip_suffix(" (deleted)") {
        Some(real) => Some(real.into()),
        None => Some(name),
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
