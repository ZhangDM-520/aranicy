//! Exec-event frame format shared by `aranicy-collectord` and `aranicy`.
//!
//! The collector captures exec events, resolves the match name at capture
//! time (see its `events` module) and pushes fixed-size frames; the daemon
//! decodes them. Fixed width keeps the reader allocation-free and lets a
//! partial frame be detected with `read_exact`.
//!
//! ```text
//! frame (280 bytes, native endian):
//!   u32 pid | u8 comm_len | u8 reserved | u16 name_len
//!   [u8; 16] comm | [u8; 256] name
//! ```
//!
//! `NAME_LEN` mirrors `aranicy_procname::NAME_CAP`: a shorter field would
//! truncate capture-time names (and can cut a UTF-8 sequence, which the
//! daemon would then drop as unresolved), so exec matching would disagree
//! with every other path.

pub const COMM_LEN: usize = 16;
pub const NAME_LEN: usize = 256;
pub const FRAME_LEN: usize = 4 + 1 + 1 + 2 + COMM_LEN + NAME_LEN;

/// One exec event as delivered to the daemon.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExecFrame {
    pub pid: u32,
    pub comm: [u8; COMM_LEN],
    pub comm_len: u8,
    /// Match name resolved at capture (`basename(argv[0])` → exe → comm).
    /// Empty when resolution failed; the daemon then falls back to comm.
    pub name: Vec<u8>,
}

impl ExecFrame {
    pub fn new(pid: u32, comm: [u8; COMM_LEN], comm_len: u8, name: &[u8]) -> Self {
        let mut n = name.to_vec();
        n.truncate(NAME_LEN);
        Self {
            pid,
            comm,
            comm_len: comm_len.min(COMM_LEN as u8),
            name: n,
        }
    }

    pub fn comm_bytes(&self) -> &[u8] {
        &self.comm[..self.comm_len as usize]
    }

    pub fn encode(&self) -> [u8; FRAME_LEN] {
        let mut out = [0u8; FRAME_LEN];
        out[0..4].copy_from_slice(&self.pid.to_ne_bytes());
        out[4] = self.comm_len;
        out[6..8].copy_from_slice(&(self.name.len() as u16).to_ne_bytes());
        out[8..8 + COMM_LEN].copy_from_slice(&self.comm);
        out[24..24 + self.name.len()].copy_from_slice(&self.name);
        out
    }

    pub fn decode(bytes: &[u8; FRAME_LEN]) -> Self {
        let mut comm = [0u8; COMM_LEN];
        comm.copy_from_slice(&bytes[8..8 + COMM_LEN]);
        let name_len =
            (u16::from_ne_bytes(bytes[6..8].try_into().expect("2 bytes")) as usize).min(NAME_LEN);
        Self {
            pid: u32::from_ne_bytes(bytes[0..4].try_into().expect("4 bytes")),
            comm,
            comm_len: bytes[4].min(COMM_LEN as u8),
            name: bytes[24..24 + name_len].to_vec(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trips() {
        let mut comm = [0u8; COMM_LEN];
        comm[..5].copy_from_slice(b"cc1pl");
        let f = ExecFrame::new(4242, comm, 5, b"cc1plus");
        let back = ExecFrame::decode(&f.encode());
        assert_eq!(back, f);
        assert_eq!(back.comm_bytes(), b"cc1pl");
        assert_eq!(back.name, b"cc1plus");
    }

    #[test]
    fn frame_truncates_long_names() {
        let long = [b'x'; NAME_LEN + 10];
        let f = ExecFrame::new(1, [0; COMM_LEN], 0, &long);
        assert_eq!(f.name.len(), NAME_LEN);
        assert_eq!(ExecFrame::decode(&f.encode()).name.len(), NAME_LEN);
    }

    #[test]
    fn frame_length_matches_layout() {
        assert_eq!(FRAME_LEN, 280);
    }

    #[test]
    fn empty_name_survives_the_round_trip() {
        let f = ExecFrame::new(9, [0; COMM_LEN], 0, b"");
        assert!(ExecFrame::decode(&f.encode()).name.is_empty());
    }
}
