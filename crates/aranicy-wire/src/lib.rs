//! Exec-event frame format shared by `aranicy-collectord` and `aranicy`.
//!
//! The collector captures exec events, resolves the match name at capture
//! time (see its `events` module) and pushes fixed-size frames; the daemon
//! decodes them. Fixed width keeps both halves allocation-free — [`FrameBuf`]
//! is the producer's owned 280 bytes, [`FrameRef`] the consumer's borrowed
//! view — and lets a partial frame be detected before decoding. Frames travel
//! in batches of up to [`BATCH_FRAMES`], so a batch costs one `write` on the
//! producer and one `read` on the consumer; [`BatchBuf`] is the reader's
//! buffer, including the partial-frame tail a short read leaves behind.
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

use serde::{Deserialize, Serialize};

/// One full CPU in the normalized utilization scale (scx uses `1 << 10`).
pub const MAX_UTIL: u64 = 1024;

/// `static_prio` is the kernel nice shifted by this offset: `nice + 20`.
pub const NICE_OFFSET: u64 = 20;

/// Observation budget: the maximum number of per-task rows one sample window
/// may carry. Defined here once because both the collector (which cuts) and
/// the daemon (which requests) must agree on it.
/// Rows a single drain window may carry — the observation budget.
///
/// Measured on a 24-thread desktop: an idle window carries ~260 rows (a
/// 512 budget masked nothing), but a full `cargo build` reaches ~5 600
/// short-lived processes, where 512 hid ~91% of the evidence and even 4 096
/// truncated ~27%. The cap bounds a pathological window; it must sit above
/// the working set of the workload the bias exists for. Measured at 8 192
/// rows under a full rebuild: daemon ~0.3 %, collector ~0.6 % of a core, with
/// no truncation (at 4 096 the same workload truncated ~27 %).
pub const SAMPLE_CAP: usize = 8192;

/// One per-task row of a drain window — the single definition of the sample
/// contract, shared by the collector (producer) and the daemon (consumer).
///
/// Parsing is lenient by design: unknown fields are ignored and missing
/// fields default, so additive schema changes stay compatible. The consequence
/// is that a *missing* field is indistinguishable from a meaningful zero (a
/// `wait_freq` of 0 legitimately means "never slept").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sample {
    pub pid: i64,
    /// Kernel task name (16 bytes).
    pub comm: String,
    /// Kernel static priority: nice + 20 (see [`NICE_OFFSET`]).
    pub static_prio: u64,
    /// Sleeps per second of the task's OWN activity (EWMA). 0 is a real
    /// measurement.
    ///
    /// The denominator is active time (running + runnable), never wall clock:
    /// lavd's per-event EWMA reported `1e9 / inter-sleep interval`, and a
    /// window-wide denominator instead multiplies the rate by the task's duty
    /// cycle, so a shell one-liner sleeping twice in 1 ms of a 500 ms window
    /// reads 4/s and classifies as BULK (measured live 2026-11-11: that put
    /// `bash`/`cat`/`grep` at bias +3 and the compiler in the deadband).
    pub wait_freq: u64,
    /// Wakes issued to other tasks per second of the task's own activity
    /// (EWMA; same denominator as [`Sample::wait_freq`]). 0 is a real
    /// measurement: a compiler forks its children, it does not wake them.
    pub wake_freq: u64,
    /// Mean nanoseconds per schedule. 0 means "never ran" (no evidence).
    pub avg_runtime_wall: u64,
    /// DEMAND: running + runnable duty cycle, 0..=[`MAX_UTIL`] (one CPU at
    /// `MAX_UTIL`). Always `>= cpu_util_wall`.
    pub task_util_est: u64,
    /// DELIVERED: running duty cycle on the same 0..=[`MAX_UTIL`] scale.
    pub cpu_util_wall: u64,
    /// Schedules taken in this window — a COUNT, not a rate.
    ///
    /// Counts (not the smoothed `wait_freq`/`avg_runtime_wall`) are what an
    /// offline analysis needs to reconstruct the window exactly; the live
    /// classifier keeps using the per-sample means because that is what a
    /// single-row contract can carry. A missing field means 0, i.e. "no
    /// evidence" — for the lean axis only, never for the score axis.
    pub runs: u32,
    /// Sleeps entered in this window — a COUNT, not a rate. `0` alongside
    /// `runs > 0` is a real observation (a task that never slept), not a
    /// missing field.
    pub sleeps: u32,
    /// The row describes a KERNEL thread (`PF_KTHREAD`). Never bias one:
    /// nice on a kthread is meaningless, and claiming the shared step budget
    /// for `kworker/*` starves the workloads the bias exists for (measured
    /// live 2026-11-11: `kthreadd` and `ksoftirqd/12` held +3).
    ///
    /// Additive and defaulted, and a FLAG (`1`), not a measurement: the
    /// `scx_stats` row type has no `bool` field (`bool` does not implement
    /// `Meta`), so the contract keeps it numeric; [`Sample::is_kernel_thread`]
    /// is the reader. A producer that predates the field reports `0`, i.e.
    /// "not known to be a kernel thread", which degrades to the old behaviour.
    pub kthread: u64,
}

impl Sample {
    /// The row belongs to a kernel thread. Never bias one: nice on a kthread
    /// is meaningless, and claiming the shared step budget for `kworker/*`
    /// starves the workloads the bias exists for.
    pub fn is_kernel_thread(&self) -> bool {
        self.kthread != 0
    }
}

pub const COMM_LEN: usize = 16;
pub const NAME_LEN: usize = 256;
pub const FRAME_LEN: usize = 4 + 1 + 1 + 2 + COMM_LEN + NAME_LEN;

/// Frames the producer writes in one `writev` and the reader admits in one
/// `read`. 16 × 280 B = 4 480 B: one syscall per burst of 16 execs, and a
/// buffer small enough to live on the stack of both halves. (Measured before
/// batching: 0.996 reads and 0.96 writes per exec event, i.e. one each.)
pub const BATCH_FRAMES: usize = 16;

/// Bytes of one full batch.
pub const BATCH_LEN: usize = FRAME_LEN * BATCH_FRAMES;

/// One owned exec event, encoded in place: the producer's half of the
/// contract.
///
/// Fixed 280 bytes by value, so a queue of frames, a batch buffer and a
/// pass-through are all allocation-free — the old owned form carried a
/// `Vec<u8>` name and cost one heap allocation per exec event on each side.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct FrameBuf([u8; FRAME_LEN]);

/// A borrowed view over one frame's bytes: the consumer's half of the same
/// contract. `name()` never copies — the reader decodes in place.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct FrameRef<'a> {
    bytes: &'a [u8; FRAME_LEN],
}

/// `(pid, comm_len, name_len)` from a frame header.
fn header(bytes: &[u8; FRAME_LEN]) -> (u32, usize, usize) {
    let pid = u32::from_ne_bytes(bytes[0..4].try_into().expect("4 bytes"));
    let comm_len = usize::from(bytes[4].min(COMM_LEN as u8));
    let name_len = (u16::from_ne_bytes(bytes[6..8].try_into().expect("2 bytes")) as usize).min(NAME_LEN);
    (pid, comm_len, name_len)
}

impl FrameBuf {
    /// Encode one event. A name longer than [`NAME_LEN`] is truncated (see the
    /// module docs: a narrower field would cut UTF-8 and drop the event as
    /// unresolved).
    pub fn new(pid: u32, comm: [u8; COMM_LEN], comm_len: u8, name: &[u8]) -> Self {
        let name_len = name.len().min(NAME_LEN);
        let mut bytes = [0u8; FRAME_LEN];
        bytes[0..4].copy_from_slice(&pid.to_ne_bytes());
        bytes[4] = comm_len.min(COMM_LEN as u8);
        bytes[6..8].copy_from_slice(&(name_len as u16).to_ne_bytes());
        bytes[8..8 + COMM_LEN].copy_from_slice(&comm);
        bytes[24..24 + name_len].copy_from_slice(&name[..name_len]);
        Self(bytes)
    }

    /// Take ownership of bytes a reader already placed in a buffer.
    pub fn from_bytes(bytes: [u8; FRAME_LEN]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; FRAME_LEN] {
        &self.0
    }

    pub fn as_ref(&self) -> FrameRef<'_> {
        FrameRef::new(&self.0)
    }

    pub fn pid(&self) -> u32 {
        header(&self.0).0
    }

    /// Kernel comm bytes (`nice + 20`-style raw task name, no UTF-8 promise).
    pub fn comm(&self) -> &[u8] {
        let (_, comm_len, _) = header(&self.0);
        &self.0[8..8 + comm_len]
    }

    /// Match name resolved at capture (`basename(exe)` → `argv[0]` → comm).
    /// Empty when resolution failed; the daemon then falls back to comm.
    pub fn name(&self) -> &[u8] {
        let (_, _, name_len) = header(&self.0);
        &self.0[24..24 + name_len]
    }
}

impl<'a> FrameRef<'a> {
    pub fn new(bytes: &'a [u8; FRAME_LEN]) -> Self {
        Self { bytes }
    }

    pub fn as_bytes(&self) -> &'a [u8; FRAME_LEN] {
        self.bytes
    }

    pub fn pid(&self) -> u32 {
        header(self.bytes).0
    }

    pub fn comm(&self) -> &'a [u8] {
        let (_, comm_len, _) = header(self.bytes);
        &self.bytes[8..8 + comm_len]
    }

    pub fn name(&self) -> &'a [u8] {
        let (_, _, name_len) = header(self.bytes);
        &self.bytes[24..24 + name_len]
    }
}

impl Default for FrameBuf {
    fn default() -> Self {
        Self([0u8; FRAME_LEN])
    }
}

/// The reader's fixed batch buffer: bytes received, whole frames delivered,
/// and the partial-frame tail a short read leaves for the next call.
///
/// Owning the buffer is the point of the seam — a batch costs no allocation,
/// the caller iterates [`FrameRef`]s in place, and a frame split across two
/// socket reads costs a 280-byte move, not a second decoding path.
pub struct BatchBuf {
    bytes: [u8; BATCH_LEN],
    filled: usize,
}

impl BatchBuf {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop the delivered frames, keeping any partial frame for the next read.
    pub fn clear(&mut self) {
        let whole = self.len() * FRAME_LEN;
        let tail = self.filled - whole;
        if tail > 0 {
            self.bytes.copy_within(whole..self.filled, 0);
        }
        self.filled = tail;
    }

    /// Drop everything, tail included. A tail belongs to one byte stream, and
    /// the frame format has no sync word: keeping it across a reconnect would
    /// anchor every later frame at a fixed wrong offset.
    pub fn reset(&mut self) {
        self.filled = 0;
    }

    /// Where the next `read` lands: the unused tail of the buffer, after any
    /// partial frame kept by [`Self::clear`].
    pub fn spare(&mut self) -> &mut [u8] {
        &mut self.bytes[self.filled..]
    }

    /// Account for bytes a `read` just placed in [`Self::spare`].
    pub fn commit(&mut self, n: usize) {
        debug_assert!(self.filled + n <= BATCH_LEN, "read overflowed the batch");
        self.filled += n;
    }

    /// Whole frames available right now.
    pub fn len(&self) -> usize {
        self.filled / FRAME_LEN
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The whole frames, in arrival order. A trailing partial frame is not
    /// yielded; [`Self::clear`] keeps it for the next read.
    pub fn frames(&self) -> impl Iterator<Item = FrameRef<'_>> + '_ {
        (0..self.len()).map(move |i| {
            let start = i * FRAME_LEN;
            FrameRef::new(
                self.bytes[start..start + FRAME_LEN]
                    .try_into()
                    .expect("frame slice"),
            )
        })
    }

    /// Bytes currently held, whole frames plus tail (for tests and diagnostics).
    pub fn filled(&self) -> usize {
        self.filled
    }
}

impl Default for BatchBuf {
    fn default() -> Self {
        Self {
            bytes: [0u8; BATCH_LEN],
            filled: 0,
        }
    }
}

impl std::fmt::Debug for BatchBuf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BatchBuf")
            .field("frames", &self.len())
            .field("tail", &(self.filled % FRAME_LEN))
            .finish()
    }
}

impl std::fmt::Debug for FrameBuf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let r = self.as_ref();
        f.debug_struct("FrameBuf")
            .field("pid", &r.pid())
            .field("comm", &String::from_utf8_lossy(r.comm()))
            .field("name", &String::from_utf8_lossy(r.name()))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trips() {
        let mut comm = [0u8; COMM_LEN];
        comm[..5].copy_from_slice(b"cc1pl");
        let f = FrameBuf::new(4242, comm, 5, b"cc1plus");
        let back = FrameRef::new(f.as_bytes());
        assert_eq!(back.pid(), f.pid());
        assert_eq!(back.comm(), b"cc1pl");
        assert_eq!(back.name(), b"cc1plus");
    }

    #[test]
    fn frame_truncates_long_names() {
        let long = [b'x'; NAME_LEN + 10];
        let f = FrameBuf::new(1, [0; COMM_LEN], 0, &long);
        assert_eq!(f.name().len(), NAME_LEN);
        assert_eq!(FrameRef::new(f.as_bytes()).name().len(), NAME_LEN);
    }

    #[test]
    fn frame_length_matches_layout() {
        assert_eq!(FRAME_LEN, 280);
        // The queue and the batch buffers are arrays of these: a frame that
        // grew past its encoded size would silently defeat both.
        assert_eq!(std::mem::size_of::<FrameBuf>(), FRAME_LEN);
    }

    #[test]
    fn header_offsets_are_the_documented_layout() {
        let mut comm = [0u8; COMM_LEN];
        comm[..4].copy_from_slice(b"bash");
        let f = FrameBuf::new(0xdead_beef, comm, 4, b"/usr/bin/bash");
        let b = f.as_bytes();
        assert_eq!(&b[0..4], &0xdead_beefu32.to_ne_bytes());
        assert_eq!(b[4], 4);
        assert_eq!(&b[8..12], b"bash");
        assert_eq!(&b[24..24 + 13], b"/usr/bin/bash");
        // A comm_len past the field is clamped, never read out of bounds.
        let long_comm = FrameBuf::new(1, [b'x'; COMM_LEN], 200, b"n");
        assert_eq!(long_comm.comm().len(), COMM_LEN);
    }

    #[test]
    fn empty_name_survives_the_round_trip() {
        let f = FrameBuf::new(9, [0; COMM_LEN], 0, b"");
        assert!(FrameRef::new(f.as_bytes()).name().is_empty());
    }

    #[test]
    fn a_batch_yields_its_frames_in_arrival_order() {
        let mut buf = BatchBuf::new();
        let frames: Vec<FrameBuf> = (0..5)
            .map(|i| FrameBuf::new(i, [0; COMM_LEN], 0, format!("n{i}").as_bytes()))
            .collect();
        for f in &frames {
            let spare = buf.spare();
            let n = f.as_bytes().len();
            spare[..n].copy_from_slice(f.as_bytes());
            buf.commit(n);
        }
        assert_eq!(buf.len(), 5);
        let pids: Vec<u32> = buf.frames().map(|f| f.pid()).collect();
        assert_eq!(pids, vec![0, 1, 2, 3, 4]);
        let names: Vec<Vec<u8>> = buf.frames().map(|f| f.name().to_vec()).collect();
        assert_eq!(names[3], b"n3");
    }

    #[test]
    fn a_split_frame_waits_for_its_tail() {
        let mut buf = BatchBuf::new();
        let a = FrameBuf::new(7, [0; COMM_LEN], 0, b"first");
        let b = FrameBuf::new(8, [0; COMM_LEN], 0, b"second");
        let mut stream = Vec::new();
        stream.extend_from_slice(a.as_bytes());
        stream.extend_from_slice(b.as_bytes());

        // First read stops mid-frame: one whole frame, 100 bytes of the next.
        let first = &stream[..FRAME_LEN + 100];
        buf.spare()[..first.len()].copy_from_slice(first);
        buf.commit(first.len());
        assert_eq!(buf.len(), 1);
        assert_eq!(buf.frames().next().map(|f| f.pid()), Some(7));

        // The consumer drains, and the tail survives the clear.
        buf.clear();
        assert_eq!(buf.len(), 0);
        assert_eq!(buf.filled(), 100, "the partial frame is kept");

        // The rest of the batch completes it in place.
        let rest = &stream[FRAME_LEN + 100..];
        buf.spare()[..rest.len()].copy_from_slice(rest);
        buf.commit(rest.len());
        assert_eq!(buf.len(), 1);
        assert_eq!(buf.frames().next().map(|f| f.pid()), Some(8));
    }

    #[test]
    fn a_full_batch_has_room_for_exactly_batch_frames() {
        assert_eq!(BATCH_LEN % FRAME_LEN, 0);
        let mut buf = BatchBuf::new();
        for i in 0..BATCH_FRAMES {
            let f = FrameBuf::new(i as u32, [0; COMM_LEN], 0, b"n");
            let spare = buf.spare();
            spare[..FRAME_LEN].copy_from_slice(f.as_bytes());
            buf.commit(FRAME_LEN);
        }
        assert_eq!(buf.len(), BATCH_FRAMES);
        assert!(buf.spare().is_empty());
        buf.clear();
        assert_eq!(buf.filled(), 0);
        assert_eq!(buf.spare().len(), BATCH_LEN);
    }

    #[test]
    fn sample_round_trips_json() {
        let s = Sample {
            pid: 4242,
            comm: "cc1plus".into(),
            static_prio: 20 + 3,
            wait_freq: 0,
            wake_freq: 0,
            avg_runtime_wall: 3_906_250,
            task_util_est: 900,
            cpu_util_wall: 700,
            runs: 118,
            sleeps: 0,
            kthread: 0,
        };
        let json = serde_json::to_string(&s).expect("serialize");
        let back: Sample = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, s);
        assert_eq!(back.wait_freq, 0, "a meaningful zero survives the wire");
        assert_eq!(back.runs, 118, "per-window counts survive the wire");
        assert_eq!(back.sleeps, 0, "a count of zero is a real observation");
    }

    /// The kernel-thread flag is additive: an old producer omits it (0 =
    /// "not known to be a kthread"), a new one sends 1.
    #[test]
    fn the_kernel_thread_flag_survives_and_defaults_off() {
        let s = Sample {
            comm: "kworker/3".into(),
            kthread: 1,
            ..Sample::default()
        };
        assert!(s.is_kernel_thread());
        let back: Sample = serde_json::from_str(&serde_json::to_string(&s).expect("ser"))
            .expect("deserialize");
        assert!(back.is_kernel_thread(), "the flag reaches the daemon");

        let old: Sample = serde_json::from_str(r#"{"pid":1,"comm":"cargo"}"#).expect("old payload");
        assert!(
            !old.is_kernel_thread(),
            "a missing flag must not invent a kernel thread"
        );
    }

    #[test]
    fn sample_tolerates_missing_and_unknown_fields() {
        let back: Sample =
            serde_json::from_str(r#"{"pid":7,"comm":"x","cpu_heat":19}"#).expect("lenient parse");
        assert_eq!(back.pid, 7);
        assert_eq!(back.comm, "x");
        assert_eq!(back.task_util_est, 0, "missing fields default");
        // Additive since v0.3.0: an old producer without the counts still
        // parses, and 0 reads as "no evidence" on that axis only.
        assert_eq!(back.runs, 0);
        assert_eq!(back.sleeps, 0);
    }
}
