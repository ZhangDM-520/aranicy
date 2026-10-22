//! Match-name derivation and `/proc/<pid>/stat` parsing.
//!
//! Hot path contract: every helper here works on caller-provided stack buffers
//! and performs no heap allocation. Name resolution order (docs/
//! COMPATIBILITY.md): `basename(argv[0])` from `/proc/<pid>/cmdline` (first
//! NUL-terminated argument only, capped), then `basename(/proc/<pid>/exe)`
//! with a trailing `" (deleted)"` stripped, then the event's comm bytes.

use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Cap for the derived name / `/proc` read buffer (brief: ~256 bytes).
pub const NAME_CAP: usize = 256;
/// `/proc/<pid>/stat` line buffer (long comms + fields fit comfortably).
pub const STAT_CAP: usize = 512;

const PROC_PATH_CAP: usize = 64;

/// Bytes before the first NUL in a cmdline buffer (the argv[0] entry).
pub fn first_nul_arg(cmdline: &[u8]) -> &[u8] {
    match cmdline.iter().position(|&b| b == 0) {
        Some(i) => &cmdline[..i],
        None => cmdline,
    }
}

/// GNU-style basename: trailing slashes trimmed, then bytes after the last
/// `/`. Works on raw bytes (argv[0] need not be UTF-8).
pub fn basename(path: &[u8]) -> &[u8] {
    let end = path.iter().rposition(|&b| b != b'/').map_or(0, |i| i + 1);
    let path = &path[..end];
    match path.iter().rposition(|&b| b == b'/') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

/// Strip the kernel's deleted-suffix from a readlink target.
pub fn strip_deleted(target: &[u8]) -> &[u8] {
    const SUFFIX: &[u8] = b" (deleted)";
    if target.len() >= SUFFIX.len() && target.ends_with(SUFFIX) {
        &target[..target.len() - SUFFIX.len()]
    } else {
        target
    }
}

/// Build the NUL-terminated `/proc/<pid>/<suffix>` path; returns the length
/// excluding the NUL (`out[len] == 0` is guaranteed).
fn proc_path(pid: u32, suffix: &[u8], out: &mut [u8; PROC_PATH_CAP]) -> Option<usize> {
    let mut len = 0usize;
    let mut push = |bytes: &[u8]| -> Option<()> {
        if len + bytes.len() + 1 > out.len() {
            return None;
        }
        out[len..len + bytes.len()].copy_from_slice(bytes);
        len += bytes.len();
        Some(())
    };
    push(b"/proc/")?;
    let mut digits = [0u8; 10];
    let mut n = 0usize;
    let mut v = u64::from(pid);
    if v == 0 {
        digits[0] = b'0';
        n = 1;
    } else {
        while v > 0 && n < digits.len() {
            digits[n] = b'0' + (v % 10) as u8;
            v /= 10;
            n += 1;
        }
        digits[..n].reverse();
    }
    push(&digits[..n])?;
    push(b"/")?;
    push(suffix)?;
    out[len] = 0;
    Some(len)
}

/// `read(2)` of a NUL-terminated path into `buf`; returns bytes read.
fn read_fd(path: &[u8], buf: &mut [u8]) -> Option<usize> {
    debug_assert_eq!(path.last().copied(), Some(0));
    unsafe {
        let fd = libc::open(path.as_ptr().cast(), libc::O_RDONLY | libc::O_CLOEXEC);
        if fd < 0 {
            return None;
        }
        let n = libc::read(fd, buf.as_mut_ptr().cast(), buf.len());
        libc::close(fd);
        if n <= 0 {
            None
        } else {
            Some(n as usize)
        }
    }
}

/// `readlink(2)` of a NUL-terminated path into `buf`; returns bytes read.
fn readlink_fd(path: &[u8], buf: &mut [u8]) -> Option<usize> {
    debug_assert_eq!(path.last().copied(), Some(0));
    let n = unsafe { libc::readlink(path.as_ptr().cast(), buf.as_mut_ptr().cast(), buf.len()) };
    if n <= 0 {
        None
    } else {
        Some(n as usize)
    }
}

/// Read `/proc/<pid>/stat` into `buf`; returns bytes read.
pub fn read_stat(pid: u32, buf: &mut [u8; STAT_CAP]) -> Option<usize> {
    let mut path = [0u8; PROC_PATH_CAP];
    let len = proc_path(pid, b"stat", &mut path)?;
    read_fd(&path[..=len], buf)
}

/// The comm field of a stat line: between the first `(` and the *last* `)`
/// (comm itself may contain spaces and parentheses).
pub fn stat_comm(stat: &[u8]) -> Option<&[u8]> {
    let open = stat.iter().position(|&b| b == b'(')?;
    let close = stat.iter().rposition(|&b| b == b')')?;
    if close <= open {
        return None;
    }
    Some(&stat[open + 1..close])
}

/// The nice field (stat field 19) of a stat line.
pub fn stat_nice(stat: &[u8]) -> Option<i8> {
    let close = stat.iter().rposition(|&b| b == b')')?;
    let rest = &stat[close + 1..];
    // Tokens after comm start at field 3 (state); nice is field 19 -> index 16.
    let mut idx = 0usize;
    for tok in rest.split(|&b| b == b' ') {
        if tok.is_empty() {
            continue;
        }
        if idx == 16 {
            return std::str::from_utf8(tok).ok()?.parse::<i8>().ok();
        }
        idx += 1;
    }
    None
}

/// Basename-compaction in place: given `buf[..n]` holds a path, move its
/// basename to the front and return its length.
fn compact_basename(buf: &mut [u8], n: usize) -> usize {
    let name_len = {
        let name = basename(&buf[..n]);
        name.len()
    };
    if name_len == 0 {
        return 0;
    }
    buf.copy_within(n - name_len..n, 0);
    name_len
}

/// Derive the daemon's match name for `pid` into `buf`, returning the name's
/// length (0 = unresolved). `comm` is the kernel comm hint (may be empty).
/// No heap allocation.
pub fn derive_name(pid: u32, comm: &[u8], buf: &mut [u8; NAME_CAP]) -> usize {
    let mut path = [0u8; PROC_PATH_CAP];

    // 1. basename(argv[0]) from cmdline (single read, first NUL arg only).
    if let Some(len) = proc_path(pid, b"cmdline", &mut path) {
        if let Some(n) = read_fd(&path[..=len], buf) {
            let arg0_len = first_nul_arg(&buf[..n]).len();
            let name_len = compact_basename(buf, arg0_len);
            if name_len > 0 {
                return name_len;
            }
        }
    }

    // 2. basename of /proc/<pid>/exe (readlink), " (deleted)" stripped.
    if let Some(len) = proc_path(pid, b"exe", &mut path) {
        if let Some(n) = readlink_fd(&path[..=len], buf) {
            let target_len = strip_deleted(&buf[..n]).len();
            let name_len = compact_basename(buf, target_len);
            if name_len > 0 {
                return name_len;
            }
        }
    }

    // 3. The event's comm bytes.
    let comm = first_nul_arg(comm);
    if comm.is_empty() {
        return 0;
    }
    let n = comm.len().min(NAME_CAP);
    buf[..n].copy_from_slice(&comm[..n]);
    compact_basename(buf, n)
}

/// Derive the match name for a live pid (dump/drift paths).
pub fn derive_name_for_pid(pid: u32) -> Option<(Vec<u8>, i8)> {
    let mut stat = [0u8; STAT_CAP];
    let n = read_stat(pid, &mut stat)?;
    let nice = stat_nice(&stat[..n])?;
    let comm = stat_comm(&stat[..n]).unwrap_or(b"");
    let mut buf = [0u8; NAME_CAP];
    let len = derive_name(pid, comm, &mut buf);
    if len == 0 {
        return None;
    }
    Some((buf[..len].to_vec(), nice))
}

/// PID of a `/proc` directory entry (numeric names only).
pub fn entry_pid(name: &std::ffi::OsStr) -> Option<u32> {
    std::str::from_utf8(name.as_bytes()).ok()?.parse::<u32>().ok()
}

/// `basename(argv[0])`-style match name for a plain path string (used by
/// `match-test`, which takes the name directly).
pub fn normalize_cli_name(name: &str) -> &str {
    std::str::from_utf8(basename(Path::new(name).as_os_str().as_bytes()))
        .unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basename_logic() {
        assert_eq!(basename(b"/usr/bin/foo"), b"foo");
        assert_eq!(basename(b"foo"), b"foo");
        assert_eq!(basename(b""), b"");
        assert_eq!(basename(b"/"), b"");
        assert_eq!(basename(b"/usr/bin/"), b"bin");
        assert_eq!(basename(b"///a//b"), b"b");
    }

    #[test]
    fn basename_preserves_non_utf8_bytes() {
        assert_eq!(basename(b"/bin/\xff\xfe"), b"\xff\xfe");
        assert_eq!(basename(b"\x80bad"), b"\x80bad");
    }

    #[test]
    fn deleted_suffix_stripping() {
        assert_eq!(strip_deleted(b"/usr/bin/foo (deleted)"), b"/usr/bin/foo");
        assert_eq!(strip_deleted(b"/usr/bin/foo"), b"/usr/bin/foo");
        assert_eq!(strip_deleted(b" (deleted)"), b"");
        assert_eq!(strip_deleted(b"x (deleted) (deleted)"), b"x (deleted)");
    }

    #[test]
    fn first_nul_arg_slices_at_first_nul() {
        assert_eq!(first_nul_arg(b"foo\0bar\0"), b"foo");
        assert_eq!(first_nul_arg(b"no-nul"), b"no-nul");
        assert_eq!(first_nul_arg(b""), b"");
        assert_eq!(first_nul_arg(b"\0hidden"), b"");
        assert_eq!(first_nul_arg(b"\xff\xfe\0x"), b"\xff\xfe");
    }

    #[test]
    fn stat_comm_handles_spaces_and_parens() {
        let stat = b"42 (evil ) name) S 1 2 3";
        assert_eq!(stat_comm(stat), Some(&b"evil ) name"[..]));
    }

    #[test]
    fn stat_nice_is_field_nineteen() {
        // Fields after ')': state(3), then ppid..priority (4..18) = 15 numbers,
        // then nice(19).
        let stat = b"42 (name) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 -5 20 0";
        assert_eq!(stat_nice(stat), Some(-5));
        let stat = b"42 (name with spaces) S 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 19 20";
        assert_eq!(stat_nice(stat), Some(19));
    }

    #[test]
    fn stat_nice_missing_is_none() {
        assert_eq!(stat_nice(b"42 (name) S 1 2 3"), None);
        assert_eq!(stat_nice(b"no parens here"), None);
    }

    #[test]
    fn compact_basename_moves_name_to_front() {
        let mut buf = *b"/usr/bin/foo\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";
        let n = compact_basename(&mut buf, 12);
        assert_eq!(&buf[..n], b"foo");
    }

    #[test]
    fn proc_path_shape() {
        let mut out = [0u8; PROC_PATH_CAP];
        let len = proc_path(1234, b"cmdline", &mut out).unwrap();
        assert_eq!(&out[..len], b"/proc/1234/cmdline");
        assert_eq!(out[len], 0);
    }

    #[test]
    fn derive_name_resolves_own_pid() {
        // Unprivileged: our own /proc entries are always readable.
        let mut buf = [0u8; NAME_CAP];
        let n = derive_name(std::process::id(), b"ignored-hint", &mut buf);
        assert!(n > 0, "self name must resolve");
        assert_eq!(basename(&buf[..n]), &buf[..n]);
    }

    #[test]
    fn derive_name_falls_back_to_comm_for_missing_pid() {
        // A pid that does not exist: cmdline and exe both fail -> comm fallback.
        let mut buf = [0u8; NAME_CAP];
        let n = derive_name(u32::MAX - 1, b"ghost-comm", &mut buf);
        assert_eq!(&buf[..n], b"ghost-comm");
    }

    #[test]
    fn entry_pid_accepts_numeric_only() {
        use std::ffi::OsStr;
        assert_eq!(entry_pid(OsStr::new("123")), Some(123));
        assert_eq!(entry_pid(OsStr::new("self")), None);
        assert_eq!(entry_pid(OsStr::new("12x")), None);
    }
}
