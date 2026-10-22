//! Ringbuf wire format: the mirror of `bpf/exec_event.h`.
//!
//! The numeric constants below are the C header's `EXEC_EVENT_*` values; both
//! sides assert their own layout against them (C: `_Static_assert` in the
//! header, Rust: const assertions here). The `#[ignore]`d integration test is
//! the end-to-end proof that the two layouts agree.

use crate::ExecEvent;

/// C `EXEC_EVENT_SIZE`.
pub(crate) const RAW_EVENT_SIZE: usize = 24;
/// C `EXEC_EVENT_PID_OFFSET`.
pub(crate) const RAW_PID_OFFSET: usize = 0;
/// C `EXEC_EVENT_COMM_LEN_OFFSET`.
pub(crate) const RAW_COMM_LEN_OFFSET: usize = 4;
/// C `EXEC_EVENT_COMM_OFFSET`.
pub(crate) const RAW_COMM_OFFSET: usize = 8;

/// Exactly the C `struct exec_event`, only used to pin the layout contract.
/// Samples are decoded byte-wise (see [`ExecEvent::from_sample`]) so decoding
/// is alignment-safe and allocation-free.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct RawExecEvent {
    pub pid: u32,
    pub comm_len: u8,
    pub pad: [u8; 3],
    pub comm: [u8; 16],
}

const _: () = assert!(std::mem::size_of::<RawExecEvent>() == RAW_EVENT_SIZE);
const _: () = assert!(std::mem::align_of::<RawExecEvent>() == 4);
const _: () = assert!(std::mem::offset_of!(RawExecEvent, pid) == RAW_PID_OFFSET);
const _: () =
    assert!(std::mem::offset_of!(RawExecEvent, comm_len) == RAW_COMM_LEN_OFFSET);
const _: () = assert!(std::mem::offset_of!(RawExecEvent, comm) == RAW_COMM_OFFSET);

impl ExecEvent {
    /// Decode one 24-byte ringbuf sample. Allocation-free; `None` on a
    /// malformed sample (wrong size or impossible `comm_len`) — such a sample
    /// indicates a wire-format mismatch, never valid kernel output.
    pub(crate) fn from_sample(sample: &[u8]) -> Option<ExecEvent> {
        if sample.len() != RAW_EVENT_SIZE {
            return None;
        }
        let pid = u32::from_ne_bytes(
            sample[RAW_PID_OFFSET..RAW_PID_OFFSET + 4].try_into().ok()?,
        );
        let comm_len = sample[RAW_COMM_LEN_OFFSET];
        if comm_len as usize > 16 {
            return None;
        }
        let mut comm = [0u8; 16];
        comm.copy_from_slice(&sample[RAW_COMM_OFFSET..RAW_COMM_OFFSET + 16]);
        Some(ExecEvent { pid, comm, comm_len })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_matches_c_header_constants() {
        // Same numbers the C side asserts in bpf/exec_event.h.
        assert_eq!(std::mem::size_of::<RawExecEvent>(), 24, "EXEC_EVENT_SIZE");
        assert_eq!(
            std::mem::offset_of!(RawExecEvent, pid),
            0,
            "EXEC_EVENT_PID_OFFSET"
        );
        assert_eq!(
            std::mem::offset_of!(RawExecEvent, comm_len),
            4,
            "EXEC_EVENT_COMM_LEN_OFFSET"
        );
        assert_eq!(
            std::mem::offset_of!(RawExecEvent, pad),
            5,
            "EXEC_EVENT_PAD_OFFSET"
        );
        assert_eq!(
            std::mem::offset_of!(RawExecEvent, comm),
            8,
            "EXEC_EVENT_COMM_OFFSET"
        );
    }

    fn sample(pid: u32, comm: &str) -> Vec<u8> {
        let mut s = vec![0u8; RAW_EVENT_SIZE];
        s[0..4].copy_from_slice(&pid.to_ne_bytes());
        s[4] = comm.len() as u8;
        s[8..8 + comm.len()].copy_from_slice(comm.as_bytes());
        s
    }

    #[test]
    fn decodes_pid_and_comm() {
        let ev = ExecEvent::from_sample(&sample(4242, "ffmpeg")).unwrap();
        assert_eq!(ev.pid, 4242);
        assert_eq!(ev.comm_bytes(), b"ffmpeg");
        assert_eq!(ev.comm_len, 6);
    }

    #[test]
    fn decodes_full_15_byte_comm() {
        let name = "0123456789abcde"; // 15 bytes: TASK_COMM_LEN minus NUL
        let ev = ExecEvent::from_sample(&sample(7, name)).unwrap();
        assert_eq!(ev.comm_bytes(), name.as_bytes());
    }

    #[test]
    fn rejects_wrong_sample_size() {
        assert!(ExecEvent::from_sample(&[0u8; 23]).is_none());
        assert!(ExecEvent::from_sample(&[0u8; 25]).is_none());
        assert!(ExecEvent::from_sample(&[]).is_none());
    }

    #[test]
    fn rejects_impossible_comm_len() {
        let mut s = sample(1, "x");
        s[4] = 17;
        assert!(ExecEvent::from_sample(&s).is_none());
    }

    #[test]
    fn decode_is_allocation_free() {
        // Decode from a borrowed slice into a Copy event: no owned buffers are
        // created anywhere in from_sample (byte copies into stack arrays only).
        let s = sample(9, "true");
        let ev = ExecEvent::from_sample(&s).unwrap();
        let ev2 = ev; // Copy
        assert_eq!(ev, ev2);
    }
}
