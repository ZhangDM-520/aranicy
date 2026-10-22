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

fn flock(fd: i32, op: i32) -> std::io::Result<()> {
    let r = unsafe { libc::flock(fd, op) };
    if r == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}
