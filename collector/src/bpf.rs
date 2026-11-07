//! BPF loader for the per-task accounting program.
//!
//! Mirrors `crates/aranicy-events`: the object is embedded at compile time
//! (`include_bytes!`) and opened from memory, so the packaged daemon needs no
//! sidecar files. Field order in [`StatsCollector`] keeps `links` ahead of
//! `obj` so tracepoints detach before the object unloads.

use std::ffi::OsStr;

use anyhow::{Context, Result};
use libbpf_rs::{Link, MapCore, MapFlags, Object, ObjectBuilder, RingBuffer, RingBufferBuilder};

use aranicy_procname::{NAME_CAP, ProcResolver};

use crate::agg::{RawAcc, Snapshot, parse_raw};
use crate::events::{COMM_LEN, EventQueue, ExecFrame};

static BPF_OBJ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/collector.bpf.o"));

const MAP_NAME: &str = "tasks";
/// Batch size for `lookup_batch`; larger than any expected table, small
/// enough to keep one batch under the kernel's element cap.
const SNAPSHOT_BATCH: u32 = 1024;
const EXEC_MAP_NAME: &str = "exec_events";
const EXPECTED_PROGS: usize = 6;

fn decode_entry(key: &[u8], value: &[u8]) -> Option<(u32, RawAcc)> {
    let pid_bytes = <[u8; 4]>::try_from(key).ok()?;
    Some((u32::from_ne_bytes(pid_bytes), parse_raw(value)?))
}

pub struct StatsCollector {
    /// Field order is load-bearing: the ring buffer holds the map fd and must
    /// be dropped before the object it borrows from.
    rb: RingBuffer<'static>,
    /// Kept alive for the object's lifetime: dropping a link detaches the
    /// program (and, for `tp_btf`, is the only detach path).
    #[expect(dead_code, reason = "links detach on drop: held for lifetime")]
    links: Vec<Link>,
    obj: Object,
}

impl StatsCollector {
    /// Load and attach every program in the object (switch, tick, exec,
    /// wakeup, waking, exec-ringbuf) and wire the exec ring buffer into
    /// `queue`.
    pub fn attach(queue: EventQueue) -> Result<Self> {
        let mut builder = ObjectBuilder::default();
        let open = builder
            .open_memory(BPF_OBJ)
            .context("failed to open embedded collector BPF object")?;
        let obj = open.load().context("failed to load collector BPF object")?;

        let mut links = Vec::new();
        for prog in obj.progs_mut() {
            let name = prog.name().to_string_lossy().into_owned();
            let link = prog
                .attach()
                .with_context(|| format!("failed to attach BPF program {name}"))?;
            links.push(link);
        }
        if links.len() != EXPECTED_PROGS {
            anyhow::bail!(
                "embedded object attached {} programs, expected {EXPECTED_PROGS}",
                links.len()
            );
        }

        let proc = ProcResolver::open();
        let exec_map = obj
            .maps()
            .find(|m| m.name() == OsStr::new(EXEC_MAP_NAME))
            .context(format!("embedded object has no map named {EXEC_MAP_NAME}"))?;
        let mut rb_builder = RingBufferBuilder::new();
        rb_builder
            .add(&exec_map, move |data: &[u8]| {
                // Decode + resolve the match name here: capture-time /proc
                // reads are what make short-lived build tools match reliably.
                if data.len() < 8 + COMM_LEN {
                    return 0;
                }
                let pid = u32::from_ne_bytes(data[0..4].try_into().expect("4 bytes"));
                let comm_len = (data[4] as usize).min(COMM_LEN);
                let mut comm = [0u8; COMM_LEN];
                comm.copy_from_slice(&data[8..8 + COMM_LEN]);
                let mut name_buf = [0u8; NAME_CAP];
                let n = proc.derive_name(pid, &comm[..comm_len], &mut name_buf);
                queue.push(ExecFrame::new(pid, comm, comm_len as u8, &name_buf[..n]));
                0
            })
            .context("failed to register exec ring buffer")?;
        let rb = rb_builder
            .build()
            .context("failed to build exec ring buffer")?;

        Ok(Self { rb, links, obj })
    }

    /// Drain pending exec records (blocks up to `timeout`).
    pub fn poll_events(&self, timeout: std::time::Duration) -> Result<()> {
        self.rb.poll(timeout).context("exec ring buffer poll failed")
    }

    /// Drain the whole per-task map. Cheap enough at the 500 ms cadence the
    /// bias engine uses; the map is capped at 16 384 tasks.
    pub fn snapshot(&self) -> Result<Snapshot> {
        let map = self
            .obj
            .maps()
            .find(|m| m.name() == OsStr::new(MAP_NAME))
            .context(format!("embedded object has no map named {MAP_NAME}"))?;

        let mut out = Snapshot::new();
        // Batch reads: a key-at-a-time drain costs two bpf() calls per task
        // (~32k for a full 16k table) every cadence, which is a measurable
        // share of a core and delays the exec ring-buffer poll. Fall back to
        // the per-key path if the kernel rejects batching.
        match map.lookup_batch(SNAPSHOT_BATCH, MapFlags::ANY, MapFlags::ANY) {
            Ok(batch) => {
                for (key, value) in batch {
                    if let Some((pid, acc)) = decode_entry(&key, &value) {
                        out.insert(pid, acc);
                    }
                }
                return Ok(out);
            }
            Err(e) => {
                tracing::debug!(error = %e, "batch map read unavailable; using lookup loop");
            }
        }
        for key in map.keys() {
            let Some(value) = map
                .lookup(&key, MapFlags::ANY)
                .context("failed to read task accumulator")?
            else {
                continue; // task exited between key iteration and lookup
            };
            if let Some((pid, acc)) = decode_entry(&key, &value) {
                out.insert(pid, acc);
            }
        }
        Ok(out)
    }
}
