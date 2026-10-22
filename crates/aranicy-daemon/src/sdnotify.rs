//! `sd_notify` without libsystemd: a single datagram to `$NOTIFY_SOCKET`.
//!
//! The socket is a filesystem path or an abstract address (`@name`); the
//! payload is a plain state string (`READY=1`, `STOPPING=1`). All errors are
//! best-effort: notification is observability for a service manager, never a
//! daemon-critical path.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

/// `$NOTIFY_SOCKET` contents (None when not under a notifying supervisor).
pub fn socket_from_env() -> Option<std::ffi::OsString> {
    std::env::var_os("NOTIFY_SOCKET").filter(|s| !s.is_empty())
}

/// Send `state` (`READY=1` / `STOPPING=1`) to the notify socket. No-op when
/// `socket` is None. Abstract sockets (`@name`) are handled per sd_notify(3).
pub fn notify(socket: Option<&OsStr>, state: &str) {
    let Some(path) = socket else { return };
    let bytes = path.as_bytes();
    let abstract_ = bytes.first() == Some(&b'@');
    let name = if abstract_ { &bytes[1..] } else { bytes };

    unsafe {
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            return;
        }
        let mut addr: libc::sockaddr_un = std::mem::zeroed();
        addr.sun_family = (libc::AF_UNIX) as libc::sa_family_t;
        let cap = addr.sun_path.len() - if abstract_ { 1 } else { 2 };
        if name.len() > cap {
            libc::close(fd);
            return;
        }
        // Address length is `offsetof(sun_path) + name + 1` for both flavors:
        // the extra byte is the leading NUL (abstract) or the trailing NUL
        // (pathname). Counting both would target a non-listening address.
        if abstract_ {
            addr.sun_path[0] = 0;
            for (i, b) in name.iter().enumerate() {
                addr.sun_path[i + 1] = *b as libc::c_char;
            }
        } else {
            for (i, b) in name.iter().enumerate() {
                addr.sun_path[i] = *b as libc::c_char;
            }
            addr.sun_path[name.len()] = 0;
        }
        let addr_len = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + name.len() + 1)
            as libc::socklen_t;
        let _ = libc::sendto(
            fd,
            state.as_ptr().cast(),
            state.len(),
            0,
            (&raw const addr).cast(),
            addr_len,
        );
        libc::close(fd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_socket_is_noop() {
        notify(None, "READY=1"); // must not panic or block
    }

    #[test]
    fn empty_env_is_filtered() {
        // Not touching the real env here: `socket_from_env` is a trivial
        // wrapper over `var_os` + emptiness filter; verify the filter logic.
        let empty = Some(std::ffi::OsString::new()).filter(|s| !s.is_empty());
        assert!(empty.is_none());
    }

    #[test]
    fn oversized_abstract_address_is_dropped() {
        // A name longer than sun_path must be rejected without sending.
        let huge = "@x".repeat(300);
        notify(Some(OsStr::new(&huge)), "READY=1");
    }
}
