//! Policy application layer — pure syscalls, no logging in the hot path.
//!
//! Phase 0 evidence (docs/BENCHMARKS.md): the apply path is *not* the CPU
//! problem (ruled vs unruled storms differed by ~2.5 pp), but per-action
//! `/proc/<pid>/task` walks were. Implementations here therefore resolve the
//! task list once per process (`apply_policy`) and reuse it across actions.
//! ESRCH/EPERM are normal under spawn storms: they are reported, never logged
//! per event.
//!
//! Apply order (docs/COMPATIBILITY.md, ananicy-cpp parity): nice →
//! latency_nice → sched+rtprio → ioclass+ionice → oom_score_adj → cgroup →
//! cpuset. The superset `autogroup` write runs last (it has no slot in the
//! upstream order). nice / latency_nice / sched / ionice / cpuset are per-TID
//! (Linux nice, ioprio, scheduler policy and affinity are thread-scoped);
//! oom_score_adj / cgroup / autogroup are process-scoped and applied once.
//!
//! Error taxonomy: ESRCH (and ENOENT on `/proc/<pid>/…` entries, which
//! vanishes with the process) → [`ActionOutcome::Gone`], and the remaining
//! actions/tids are still attempted; EPERM/EACCES → [`ActionOutcome::Denied`];
//! any other errno → [`ActionOutcome::Failed`]. Syscall failures never panic.
//!
//! Hot-path budget: no `format!`, no logging; all `/proc` and cgroup path
//! construction and integer formatting goes through a stack buffer
//! ([`StackPath`]). Alias cpuset resolution reads sysfs and may allocate (it
//! is expected to be cached daemon-side across events).

use aranicy_model::{CgroupDef, CpuSet, IoClass, Policy, SchedPolicy};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// Which policy dimensions are enabled (ananicy.conf `apply_*` gates).
#[derive(Debug, Clone, Copy)]
pub struct ApplyOptions {
    pub nice: bool,
    pub latency_nice: bool,
    pub sched: bool,
    pub ionice: bool,
    pub oom_score_adj: bool,
    pub cgroup: bool,
    pub cpuset: bool,
    pub autogroup: bool,
    /// ananicy.conf `cgroup_realtime_workaround` (default on, parity): RT
    /// (fifo/rr) tasks must not sit in cpu-throttled cgroups — the rule's
    /// named cgroup is skipped and the task is moved to the cgroup root
    /// instead (`cgroups.assign(pid, "")`).
    pub cgroup_realtime_workaround: bool,
}

impl Default for ApplyOptions {
    fn default() -> Self {
        Self {
            nice: true,
            latency_nice: true,
            sched: true,
            ionice: true,
            oom_score_adj: true,
            cgroup: true,
            cpuset: true,
            autogroup: true,
            cgroup_realtime_workaround: true,
        }
    }
}

/// Per-action outcome. `Skipped` is a gate or a missing field, not an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActionOutcome {
    Applied,
    #[default]
    Skipped,
    /// ESRCH: the process died mid-apply. Expected under spawn storms.
    Gone,
    /// EPERM/EACCES or kernel-unsupported. The label is the dimension name.
    Denied(&'static str),
    Failed(i32),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ApplyReport {
    pub nice: ActionOutcome,
    pub latency_nice: ActionOutcome,
    pub sched: ActionOutcome,
    pub ionice: ActionOutcome,
    pub oom_score_adj: ActionOutcome,
    pub cgroup: ActionOutcome,
    pub cpuset: ActionOutcome,
    pub autogroup: ActionOutcome,
}

impl ApplyReport {
    /// Dimension outcomes in canonical apply order (docs/COMPATIBILITY.md;
    /// `autogroup` last, see module docs). For ordered daemon-side reporting.
    pub fn ordered(&self) -> [(&'static str, ActionOutcome); 8] {
        [
            (DIM_NICE, self.nice),
            (DIM_LATENCY_NICE, self.latency_nice),
            (DIM_SCHED, self.sched),
            (DIM_IONICE, self.ionice),
            (DIM_OOM, self.oom_score_adj),
            (DIM_CGROUP, self.cgroup),
            (DIM_CPUSET, self.cpuset),
            (DIM_AUTOGROUP, self.autogroup),
        ]
    }
}

/// Kernel capability probes, run once at startup (never per event).
#[derive(Debug, Clone, Copy)]
pub struct Capabilities {
    pub latency_nice: bool,
    pub cgroup_v2: bool,
}

// Dimension labels returned in `Denied` and in [`ApplyReport::ordered`].
const DIM_NICE: &str = "nice";
const DIM_LATENCY_NICE: &str = "latency_nice";
const DIM_SCHED: &str = "sched";
const DIM_IONICE: &str = "ionice";
const DIM_OOM: &str = "oom_score_adj";
const DIM_CGROUP: &str = "cgroup";
const DIM_CPUSET: &str = "cpuset";
const DIM_AUTOGROUP: &str = "autogroup";

// ---------------------------------------------------------------------------
// Stack buffers (hot path: no `format!`, no heap for paths/integers)
// ---------------------------------------------------------------------------

/// Fixed-capacity decimal/path builder for the per-event path.
struct StackPath {
    buf: [u8; 320],
    len: usize,
}

impl StackPath {
    fn new() -> Self {
        Self {
            buf: [0; 320],
            len: 0,
        }
    }

    fn push(&mut self, bytes: &[u8]) -> bool {
        if self.len + bytes.len() > self.buf.len() {
            return false;
        }
        self.buf[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
        true
    }

    fn push_u64(&mut self, mut v: u64) -> bool {
        let mut tmp = [0u8; 20];
        let mut i = tmp.len();
        loop {
            i -= 1;
            tmp[i] = b'0' + (v % 10) as u8;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        self.push(&tmp[i..])
    }

    fn push_i64(&mut self, v: i64) -> bool {
        if v < 0 {
            self.push(b"-") && self.push_u64(v.unsigned_abs())
        } else {
            self.push_u64(v as u64)
        }
    }

    fn as_path(&self) -> &Path {
        Path::new(OsStr::from_bytes(&self.buf[..self.len]))
    }

    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

fn proc_entry_path(pid: u32, suffix: &[u8], out: &mut StackPath) -> bool {
    out.push(b"/proc/") && out.push_u64(pid as u64) && out.push(b"/") && out.push(suffix)
}

fn parse_u32_bytes(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() || !bytes.iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut v: u64 = 0;
    for &b in bytes {
        v = v.checked_mul(10)?.checked_add((b - b'0') as u64)?;
    }
    u32::try_from(v).ok()
}

// ---------------------------------------------------------------------------
// Error taxonomy
// ---------------------------------------------------------------------------

fn errno() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO)
}

fn classify_errno(err: i32, dim: &'static str) -> ActionOutcome {
    match err {
        libc::ESRCH => ActionOutcome::Gone,
        libc::EPERM | libc::EACCES => ActionOutcome::Denied(dim),
        e => ActionOutcome::Failed(e),
    }
}

/// `proc_pid_entry`: the path lives under `/proc/<pid>/…`, where ENOENT means
/// the process (or its proc entry) is gone — classify as `Gone`.
fn classify_io(err: &io::Error, dim: &'static str, proc_pid_entry: bool) -> ActionOutcome {
    match err.raw_os_error() {
        Some(libc::ESRCH) => ActionOutcome::Gone,
        Some(libc::ENOENT) if proc_pid_entry => ActionOutcome::Gone,
        Some(e) => classify_errno(e, dim),
        None => ActionOutcome::Failed(libc::EIO),
    }
}

/// Fold per-tid outcomes into one dimension outcome. Strictest wins:
/// Failed > Denied > Gone > Applied > Skipped — under spawn storms partial
/// success hides nothing: any failure (including `Gone`) is the reported truth.
fn merge_dim(a: ActionOutcome, b: ActionOutcome) -> ActionOutcome {
    fn rank(o: ActionOutcome) -> u8 {
        match o {
            ActionOutcome::Failed(_) => 4,
            ActionOutcome::Denied(_) => 3,
            ActionOutcome::Gone => 2,
            ActionOutcome::Applied => 1,
            ActionOutcome::Skipped => 0,
        }
    }
    if rank(b) > rank(a) {
        b
    } else {
        a
    }
}

// ---------------------------------------------------------------------------
// Syscall wrappers
// ---------------------------------------------------------------------------

/// `struct sched_attr` VER1 layout (kernel ≥ 5.14 adds util hints). libc only
/// exposes the 48-byte VER0 `sched_attr`; we own the layout so the E2BIG
/// size-quirk retry can shrink `size` to [`SCHED_ATTR_SIZE_VER0`] for kernels
/// whose struct predates ours.
#[repr(C)]
#[derive(Clone, Copy)]
struct SchedAttr {
    size: u32,
    sched_policy: u32,
    sched_flags: u64,
    sched_nice: i32,
    sched_priority: u32,
    sched_runtime: u64,
    sched_deadline: u64,
    sched_period: u64,
    sched_util_min: u32,
    sched_util_max: u32,
}

const SCHED_ATTR_SIZE_VER0: u32 = 48;
const SCHED_ATTR_SIZE_VER1: u32 = 56;

// uapi/linux/sched.h; SCHED_FLAG_LATENCY_NICE (5.13+) is not in libc.
const SCHED_FLAG_KEEP_PARAMS: u64 = 1 << 3;
const SCHED_FLAG_LATENCY_NICE: u64 = 1 << 7;

impl SchedAttr {
    /// Latency-nice-only update: SCHED_OTHER + KEEP_PARAMS keeps scheduling
    /// parameters untouched while `sched_nice` carries the latency nice.
    fn latency(nice: i8) -> Self {
        Self {
            size: SCHED_ATTR_SIZE_VER1,
            sched_policy: libc::SCHED_OTHER as u32,
            sched_flags: SCHED_FLAG_LATENCY_NICE | SCHED_FLAG_KEEP_PARAMS,
            sched_nice: nice as i32,
            sched_priority: 0,
            sched_runtime: 0,
            sched_deadline: 0,
            sched_period: 0,
            sched_util_min: 0,
            sched_util_max: 0,
        }
    }
}

/// `sched_setattr(2)` via raw syscall; on E2BIG (kernel struct shorter than
/// ours) retry once with the VER0 size. Returns raw errno on failure.
fn sched_setattr_raw(pid: libc::pid_t, attr: &mut SchedAttr) -> Result<(), i32> {
    attr.size = SCHED_ATTR_SIZE_VER1;
    let ret = unsafe {
        libc::syscall(
            libc::SYS_sched_setattr,
            pid,
            attr as *const SchedAttr,
            0u32,
        )
    };
    if ret == 0 {
        return Ok(());
    }
    let err = errno();
    if err == libc::E2BIG {
        attr.size = SCHED_ATTR_SIZE_VER0;
        let ret = unsafe {
            libc::syscall(
                libc::SYS_sched_setattr,
                pid,
                attr as *const SchedAttr,
                0u32,
            )
        };
        return if ret == 0 {
            Ok(())
        } else {
            Err(errno())
        };
    }
    Err(err)
}

// ioprio: IOPRIO_PRIO_VALUE(class, data) = (class << 13) | data.
const IOPRIO_CLASS_SHIFT: u32 = 13;
const IOPRIO_CLASS_NONE: i32 = 0;
const IOPRIO_CLASS_RT: i32 = 1;
const IOPRIO_CLASS_BE: i32 = 2;
const IOPRIO_CLASS_IDLE: i32 = 3;
const IOPRIO_WHO_PROCESS: libc::c_int = 1;

fn ioprio_class(class: IoClass) -> i32 {
    match class {
        IoClass::None => IOPRIO_CLASS_NONE,
        IoClass::Realtime => IOPRIO_CLASS_RT,
        IoClass::BestEffort => IOPRIO_CLASS_BE,
        IoClass::Idle => IOPRIO_CLASS_IDLE,
    }
}

fn encode_ioprio(class: IoClass, level: Option<u8>) -> i32 {
    let data = match class {
        // Level is meaningless for idle/none (data is reserved, must be 0).
        IoClass::Idle | IoClass::None => 0,
        // Rules loader validates 0..=7; clamp defensively. Absent level = 0.
        _ => i32::from(level.unwrap_or(0)).min(7),
    };
    (ioprio_class(class) << IOPRIO_CLASS_SHIFT) | data
}

/// `SYS_ioprio_set`/`SYS_ioprio_get` are present in libc for x86_64 gnu
/// (251/252, verified against libc 0.2.191); the bare numeric fallback from
/// the contract is therefore not needed here.
fn ioprio_set_raw(pid: libc::pid_t, value: i32) -> Result<(), i32> {
    let ret = unsafe {
        libc::syscall(
            libc::SYS_ioprio_set,
            IOPRIO_WHO_PROCESS,
            pid,
            value as libc::c_int,
        )
    };
    if ret == 0 {
        Ok(())
    } else {
        Err(errno())
    }
}

#[cfg(test)]
fn ioprio_get_raw(pid: libc::pid_t) -> i32 {
    unsafe { libc::syscall(libc::SYS_ioprio_get, IOPRIO_WHO_PROCESS, pid) as i32 }
}

fn setpriority_raw(tid: u32, nice: i8) -> Result<(), i32> {
    let ret = unsafe {
        libc::setpriority(libc::PRIO_PROCESS, tid as libc::id_t, i32::from(nice))
    };
    if ret == 0 {
        Ok(())
    } else {
        Err(errno())
    }
}

fn map_sched(policy: SchedPolicy) -> libc::c_int {
    match policy {
        SchedPolicy::Fifo => libc::SCHED_FIFO,
        SchedPolicy::Rr => libc::SCHED_RR,
        SchedPolicy::Normal => libc::SCHED_OTHER,
        SchedPolicy::Batch => libc::SCHED_BATCH,
        SchedPolicy::Idle => libc::SCHED_IDLE,
    }
}

/// RT priority: default 1 for fifo/rr (rtprio absent); 0 for every other
/// policy ("only set rtprio>0 for fifo/rr"). Rule range is 0..=99.
fn resolve_rtprio(policy: SchedPolicy, rtprio: Option<u8>) -> libc::c_int {
    match policy {
        SchedPolicy::Fifo | SchedPolicy::Rr => i32::from(rtprio.unwrap_or(1)).min(99),
        _ => 0,
    }
}

fn setscheduler_raw(tid: u32, policy: SchedPolicy, rtprio: Option<u8>) -> Result<(), i32> {
    let param = libc::sched_param {
        sched_priority: resolve_rtprio(policy, rtprio),
    };
    let ret = unsafe {
        libc::sched_setscheduler(tid as libc::pid_t, map_sched(policy), &param)
    };
    if ret == 0 {
        Ok(())
    } else {
        Err(errno())
    }
}

/// Mask layout: little-endian bit `cpu % 64` of word `cpu / 64`
/// (same as a Linux cpu_set_t / cpumask). Parsed masks are ≤ 1024 CPUs
/// (glibc CPU_SETSIZE).
const CPU_MASK_WORDS: usize = 16;
const CPU_MASK_BYTES: usize = CPU_MASK_WORDS * 8;

fn setaffinity_raw(tid: u32, mask: &[u64]) -> Result<(), i32> {
    let mut buf = [0u64; CPU_MASK_WORDS];
    let n = mask.len().min(CPU_MASK_WORDS);
    buf[..n].copy_from_slice(&mask[..n]);
    let ret = unsafe {
        libc::syscall(
            libc::SYS_sched_setaffinity,
            tid as libc::pid_t,
            CPU_MASK_BYTES as libc::size_t,
            buf.as_ptr(),
        )
    };
    if ret == 0 {
        Ok(())
    } else {
        Err(errno())
    }
}

// ---------------------------------------------------------------------------
// /proc writes (process-scoped dimensions)
// ---------------------------------------------------------------------------

fn write_proc_entry(pid: u32, suffix: &[u8], data: &[u8], dim: &'static str) -> ActionOutcome {
    let mut sp = StackPath::new();
    if !proc_entry_path(pid, suffix, &mut sp) {
        return ActionOutcome::Failed(libc::ENAMETOOLONG);
    }
    match fs::write(sp.as_path(), data) {
        Ok(()) => ActionOutcome::Applied,
        Err(e) => classify_io(&e, dim, true),
    }
}

fn read_proc_entry(pid: u32, suffix: &[u8], buf: &mut [u8]) -> io::Result<usize> {
    use std::io::Read;
    let mut sp = StackPath::new();
    if !proc_entry_path(pid, suffix, &mut sp) {
        return Err(io::Error::from_raw_os_error(libc::ENAMETOOLONG));
    }
    let mut f = fs::File::open(sp.as_path())?;
    f.read(buf)
}

/// Contract autogroup line: `"<id> (nice <n>)"` (brief format). Mainline
/// kernels use a different ABI (see [`apply_autogroup`]); this form is tried
/// first and falls back to the kernel form on EINVAL.
fn format_autogroup_line(id: u64, nice: i8, out: &mut StackPath) -> bool {
    out.push_u64(id)
        && out.push(b" (nice ")
        && out.push_i64(i64::from(nice))
        && out.push(b")")
}

/// Leading decimal id of `"<id> (nice <n>)"`; `None` for mainline's
/// `"/autogroup-<id> nice <n>"` line (no leading decimal).
fn parse_autogroup_id(bytes: &[u8]) -> Option<u64> {
    let end = bytes.iter().position(|&b| !b.is_ascii_digit()).unwrap_or(bytes.len());
    if end == 0 {
        return None;
    }
    let mut v: u64 = 0;
    for &b in &bytes[..end] {
        v = v.checked_mul(10)?.checked_add(u64::from(b - b'0'))?;
    }
    Some(v)
}

fn classify_autogroup_io(err: &io::Error) -> ActionOutcome {
    match err.raw_os_error() {
        Some(libc::ESRCH) => ActionOutcome::Gone,
        // ENOENT: kernel without CONFIG_SCHED_AUTOGROUP. EINVAL: kernel
        // rejects the "<id> (nice <n>)" write. Both tolerated as unsupported.
        Some(libc::ENOENT) | Some(libc::EINVAL) | Some(libc::ENOSYS) => {
            ActionOutcome::Denied(DIM_AUTOGROUP)
        }
        Some(e) => classify_errno(e, DIM_AUTOGROUP),
        None => ActionOutcome::Failed(libc::EIO),
    }
}

fn try_write_autogroup(pid: u32, data: &[u8]) -> io::Result<()> {
    let mut sp = StackPath::new();
    if !proc_entry_path(pid, b"autogroup", &mut sp) {
        return Err(io::Error::from_raw_os_error(libc::ENAMETOOLONG));
    }
    fs::write(sp.as_path(), data)
}

/// Write the process autogroup nice. The file's write ABI differs from the
/// brief's stated `"<id> (nice <n>)"` format: mainline kernels read back
/// `"/autogroup-<id> nice <n>"` and their write handler accepts the plain
/// integer nice (verified on 7.3.0-rc6-cachyos, 2026-10-22 — the contract
/// form returns EINVAL). So: parse the current line (which yields the id for
/// the contract form when the line starts with one), try the contract write
/// first, and fall back to the kernel-ABI integer write on EINVAL/ENOSYS.
/// ENOENT/EINVAL on both forms stay `Denied("autogroup")`.
fn apply_autogroup(pid: u32, nice: i8) -> ActionOutcome {
    let mut buf = [0u8; 64];
    let n = match read_proc_entry(pid, b"autogroup", &mut buf) {
        Ok(n) => n,
        Err(e) => return classify_autogroup_io(&e),
    };
    if let Some(id) = parse_autogroup_id(&buf[..n]) {
        let mut line = StackPath::new();
        if !format_autogroup_line(id, nice, &mut line) {
            return ActionOutcome::Failed(libc::ENAMETOOLONG);
        }
        match try_write_autogroup(pid, line.as_bytes()) {
            Ok(()) => return ActionOutcome::Applied,
            Err(e)
                if !matches!(
                    e.raw_os_error(),
                    Some(libc::EINVAL) | Some(libc::ENOSYS)
                ) =>
            {
                return classify_autogroup_io(&e);
            }
            Err(_) => {} // contract form rejected: fall back to kernel ABI
        }
    }
    let mut line = StackPath::new();
    if !line.push_i64(i64::from(nice)) {
        return ActionOutcome::Failed(libc::ENAMETOOLONG);
    }
    match try_write_autogroup(pid, line.as_bytes()) {
        Ok(()) => ActionOutcome::Applied,
        Err(e) => classify_autogroup_io(&e),
    }
}

// ---------------------------------------------------------------------------
// cgroup v2 manager
// ---------------------------------------------------------------------------

/// `/proc/<pid>/…`-independent: cgroup.procs errors map via raw errno —
/// ESRCH = pid died before attach, ENOENT = cgroup name not created.
fn cgroup_error(e: &io::Error) -> ActionOutcome {
    classify_errno(e.raw_os_error().unwrap_or(libc::EIO), DIM_CGROUP)
}

fn valid_cgroup_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.starts_with('/')
        && !name.contains('/')
        && !name.contains('\0')
}

/// cpu.max value for `CPUQuota` (percent of ALL cpus): quota = pct × ncpu ×
/// 1 s / 100, period fixed at 100000 us. E.g. 50% of 8 cpus → 400000 us.
fn cpu_quota_micros(pct: u8, ncpu: usize) -> u64 {
    u64::from(pct) * ncpu as u64 * 100_000 / 100
}

fn format_cpu_max(quota_micros: u64, out: &mut StackPath) -> bool {
    out.push_u64(quota_micros) && out.push(b" 100000")
}

/// First cgroup2 mount from /proc/self/mounts (unescaping octal-escaped
/// mount fields, as in `\040`).
fn find_cgroup2_mount() -> io::Result<PathBuf> {
    let data = fs::read_to_string("/proc/self/mounts")?;
    for line in data.lines() {
        let mut fields = line.split_whitespace();
        let _device = fields.next();
        let Some(target) = fields.next() else { continue };
        if fields.next() == Some("cgroup2") {
            return Ok(PathBuf::from(unescape_mount_field(target)));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no cgroup2 mount in /proc/self/mounts",
    ))
}

fn unescape_mount_field(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 4], 8) {
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// cgroup v2 manager. Path convention (ananicy-cpp parity):
/// `<cgroup2-mount>/<name>`; membership write goes to `cgroup.procs`, quota to
/// `cpu.max` (`"<max> 100000"` with quota = pct × ncpu × 1s / 100).
pub struct CgroupManager {
    pub mount: PathBuf,
    pub created: BTreeMap<String, CgroupDef>,
}

impl CgroupManager {
    /// Probe the cgroup v2 mount; create the definitions from `*.cgroups`.
    ///
    /// Each map key is the cgroup name (the `CgroupDef.name` mirror is only
    /// data). Names are rejected when empty or containing path separators —
    /// an empty name would put `cpu.max` on the root cgroup. `cpu.max` is
    /// written only for defs with a quota; `available_parallelism` supplies
    /// ncpu. `AlreadyExists` is tolerated (re-init is idempotent).
    pub fn init(defs: BTreeMap<String, CgroupDef>) -> io::Result<Self> {
        let mount = find_cgroup2_mount()?;
        let ncpu = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let mut created = BTreeMap::new();
        for (name, def) in defs {
            if !valid_cgroup_name(&name) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid cgroup name (empty, '.'/'..' or path separator)",
                ));
            }
            let dir = mount.join(&name);
            match fs::create_dir(&dir) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
            if let Some(pct) = def.cpu_quota_pct {
                let micros = cpu_quota_micros(pct, ncpu);
                let mut val = StackPath::new();
                if !format_cpu_max(micros, &mut val) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "cpu.max value overflow",
                    ));
                }
                fs::write(dir.join("cpu.max"), val.as_bytes())?;
            }
            created.insert(name, def);
        }
        Ok(Self { mount, created })
    }

    /// Move `pid` into `name` ("" = root cgroup).
    pub fn assign(&self, pid: u32, name: &str) -> ActionOutcome {
        if !name.is_empty() && !valid_cgroup_name(name) {
            return ActionOutcome::Failed(libc::EINVAL);
        }
        let mut sp = StackPath::new();
        let ok = sp.push(self.mount.as_os_str().as_bytes())
            && (name.is_empty() || (sp.push(b"/") && sp.push(name.as_bytes())))
            && sp.push(b"/cgroup.procs");
        if !ok {
            return ActionOutcome::Failed(libc::ENAMETOOLONG);
        }
        let mut val = StackPath::new();
        if !(val.push_u64(u64::from(pid)) && val.push(b"\n")) {
            return ActionOutcome::Failed(libc::ENAMETOOLONG);
        }
        match fs::write(sp.as_path(), val.as_bytes()) {
            Ok(()) => ActionOutcome::Applied,
            Err(e) => cgroup_error(&e),
        }
    }
}

// ---------------------------------------------------------------------------
// cpuset parsing and alias resolution
// ---------------------------------------------------------------------------

/// CPU mask: bit `cpu % 64` of word `cpu / 64` (cpu_set_t layout). CPU
/// indices are limited to 0..=1023 (glibc CPU_SETSIZE).
const MAX_CPUS: usize = 1024;

fn set_cpu(mask: &mut Vec<u64>, cpu: usize) {
    let word = cpu / 64;
    if mask.len() <= word {
        mask.resize(word + 1, 0);
    }
    mask[word] |= 1 << (cpu % 64);
}

/// Parse `"0-3,8-11"` / `"0,2,4"` into a cpu_set_t-layout mask. Empty items
/// (trailing/double commas) are tolerated; anything else must be `N` or
/// `A-B` with A ≤ B < 1024.
fn parse_cpu_list(s: &str) -> Result<Vec<u64>, String> {
    let mut mask = Vec::new();
    let mut seen = false;
    for item in s.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        seen = true;
        match item.split_once('-') {
            Some((lo, hi)) => {
                let lo: usize = lo
                    .trim()
                    .parse()
                    .map_err(|_| format!("invalid cpu range '{item}'"))?;
                let hi: usize = hi
                    .trim()
                    .parse()
                    .map_err(|_| format!("invalid cpu range '{item}'"))?;
                if lo > hi {
                    return Err(format!("invalid cpu range '{item}': start above end"));
                }
                if hi >= MAX_CPUS {
                    return Err(format!("cpu index {hi} out of range 0..={}", MAX_CPUS - 1));
                }
                for cpu in lo..=hi {
                    set_cpu(&mut mask, cpu);
                }
            }
            None => {
                let cpu: usize = item
                    .parse()
                    .map_err(|_| format!("invalid cpu index '{item}'"))?;
                if cpu >= MAX_CPUS {
                    return Err(format!("cpu index {cpu} out of range 0..={}", MAX_CPUS - 1));
                }
                set_cpu(&mut mask, cpu);
            }
        }
    }
    if !seen {
        return Err("empty cpu list".to_string());
    }
    Ok(mask)
}

fn read_cpu_list_file(path: &Path, what: &str) -> Result<Vec<u64>, String> {
    let data = fs::read_to_string(path)
        .map_err(|e| format!("{what}: cannot read {}: {e}", path.display()))?;
    parse_cpu_list(data.trim())
        .map_err(|e| format!("{what}: cannot parse {}: {e}", path.display()))
}

/// `llc-N`: CPUs whose top-level cache (highest `cache/index*`) carries sysfs
/// cache id N, taken from that index's `shared_cpu_list`.
fn resolve_llc(id_str: &str) -> Result<Vec<u64>, String> {
    let want: u32 = id_str
        .parse()
        .map_err(|_| format!("invalid llc cache id '{id_str}'"))?;
    let cpu_root = Path::new("/sys/devices/system/cpu");
    let entries = fs::read_dir(cpu_root)
        .map_err(|e| format!("llc-{want}: cannot list {}: {e}", cpu_root.display()))?;
    let mut saw_cpu = false;
    for entry in entries.flatten() {
        let fname = entry.file_name();
        let Some(num) = fname.to_str().and_then(|n| n.strip_prefix("cpu")) else {
            continue;
        };
        if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        saw_cpu = true;
        let cache_dir = entry.path().join("cache");
        let mut top: Option<u64> = None;
        if let Ok(ce) = fs::read_dir(&cache_dir) {
            for c in ce.flatten() {
                let cname = c.file_name();
                if let Some(idx) = cname.to_str().and_then(|n| n.strip_prefix("index")) {
                    if let Ok(v) = idx.parse::<u64>() {
                        top = Some(top.map_or(v, |t| t.max(v)));
                    }
                }
            }
        }
        let Some(idx) = top else { continue };
        let top_dir = cache_dir.join(format!("index{idx}"));
        // `id` is absent on some architectures; that makes llc-N unresolvable.
        let id = fs::read_to_string(top_dir.join("id")).map_err(|_| {
            format!(
                "llc-{want}: no cache id file under {} (architecture without cache ids)",
                top_dir.display()
            )
        })?;
        if id.trim().parse::<u32>().ok() == Some(want) {
            return read_cpu_list_file(&top_dir.join("shared_cpu_list"), "llc shared_cpu_list");
        }
    }
    if saw_cpu {
        Err(format!("llc-{want}: no CPU with top-level cache id {want}"))
    } else {
        Err(format!("llc-{want}: no CPUs found under {}", cpu_root.display()))
    }
}

/// `node-N`: NUMA node N's cpulist.
fn resolve_node(id_str: &str) -> Result<Vec<u64>, String> {
    let want: u64 = id_str
        .parse()
        .map_err(|_| format!("invalid NUMA node id '{id_str}'"))?;
    let path = format!("/sys/devices/system/node/node{want}/cpulist");
    read_cpu_list_file(Path::new(&path), &format!("node-{want}"))
}

fn online_cpus() -> Result<Vec<u64>, String> {
    read_cpu_list_file(
        Path::new("/sys/devices/system/cpu/online"),
        "all-cores",
    )
}

/// Per-CPU capacity signal, sources in order: `cpu_capacity`, then
/// `cpufreq/cpuinfo_max_freq`, then `acpi_cppc/highest_perf`. The fallbacks
/// matter on hybrid CPUs that report a uniform `cpu_capacity`; /proc/cpuinfo
/// carries no per-core-type field on x86, so there is no source beyond these.
fn cpu_capacity_of(cpu: usize) -> Option<u64> {
    for suffix in [
        "cpu_capacity",
        "cpufreq/cpuinfo_max_freq",
        "acpi_cppc/highest_perf",
    ] {
        let path = format!("/sys/devices/system/cpu/cpu{cpu}/{suffix}");
        if let Ok(s) = fs::read_to_string(&path) {
            if let Ok(v) = s.trim().parse::<u64>() {
                return Some(v);
            }
        }
    }
    None
}

/// `big-cores`/`performance-cores` = cpus at the max capacity;
/// `little-cores`/`efficiency-cores` = cpus at the min capacity. Mid-tier
/// clusters belong to neither. Uniform capacities → clear error.
fn resolve_capacity_cluster(big: bool) -> Result<Vec<u64>, String> {
    let mask = online_cpus()?;
    let cpus: Vec<usize> = (0..MAX_CPUS).filter(|&c| mask.get(c / 64).is_some_and(|w| w & (1 << (c % 64)) != 0)).collect();
    if cpus.is_empty() {
        return Err("no online CPUs found".to_string());
    }
    let mut caps = Vec::with_capacity(cpus.len());
    for &cpu in &cpus {
        caps.push(cpu_capacity_of(cpu).ok_or_else(|| {
            format!("cannot determine capacity of cpu{cpu} (no cpu_capacity/cpufreq/acpi_cppc sysfs)")
        })?);
    }
    let max = caps.iter().copied().max().unwrap();
    let min = caps.iter().copied().min().unwrap();
    if max == min {
        return Err(
            "no big/little asymmetry: all CPUs report equal capacity".to_string(),
        );
    }
    let target = if big { max } else { min };
    let mut out = Vec::new();
    for (&cpu, &cap) in cpus.iter().zip(caps.iter()) {
        if cap == target {
            set_cpu(&mut out, cpu);
        }
    }
    Ok(out)
}

fn resolve_alias(name: &str) -> Result<Vec<u64>, String> {
    if let Some(id) = name.strip_prefix("llc-") {
        return resolve_llc(id);
    }
    if let Some(id) = name.strip_prefix("node-") {
        return resolve_node(id);
    }
    match name {
        "all-cores" => online_cpus(),
        "big-cores" | "performance-cores" => resolve_capacity_cluster(true),
        "little-cores" | "efficiency-cores" => resolve_capacity_cluster(false),
        // No kernel marker identifies the boost-preferred CCDs (turbo) or
        // the V-Cache CCDs (x3d); those hosts are configured out-of-band.
        "turbo-cores" => Err(
            "alias 'turbo-cores' unsupported: no turbo-core marker exposed by the kernel".to_string(),
        ),
        "x3d-cache" | "x3d-frequency" => Err(
            "alias 'x3d-*' unsupported as a cpuset: 3D V-Cache mode is host-controlled (x3d_mode is a config input only)".to_string(),
        ),
        other => Err(format!("unknown cpuset alias '{other}'")),
    }
}

/// Resolve a `cpuset` value into a CPU mask. Explicit strings are parsed as
/// `0-3,8-11`; aliases (`big-cores`, `little-cores`, `turbo-cores`,
/// `performance-cores`, `efficiency-cores`, `all-cores`, `llc-N`, `node-N`)
/// resolve against /sys topology.
pub fn resolve_cpuset(value: &CpuSet) -> Result<Vec<u64>, String> {
    match value {
        CpuSet::Explicit(s) => parse_cpu_list(s),
        CpuSet::Alias(s) => resolve_alias(s),
    }
}

// ---------------------------------------------------------------------------
// Capability probing
// ---------------------------------------------------------------------------

/// Probe kernel capabilities (latency_nice via sched_setattr self-probe,
/// cgroup v2 presence). Must not be called from the hot path.
pub fn probe_capabilities() -> Capabilities {
    Capabilities {
        latency_nice: probe_latency_nice(),
        cgroup_v2: Path::new("/sys/fs/cgroup/cgroup.controllers").exists(),
    }
}

/// Probe on our own pid with latency_nice 0 + KEEP_PARAMS: a no-op where the
/// flag exists. EINVAL/ENOSYS mean the kernel does not know the flag; EPERM/
/// EACCES mean it does but we may not set it (still "supported"); E2BIG is
/// retried inside [`sched_setattr_raw`] with the older struct size.
fn probe_latency_nice() -> bool {
    let pid = unsafe { libc::getpid() };
    let mut attr = SchedAttr::latency(0);
    match sched_setattr_raw(pid, &mut attr) {
        Ok(()) => true,
        Err(err) => !matches!(err, libc::EINVAL | libc::ENOSYS),
    }
}

// ---------------------------------------------------------------------------
// Ordered apply
// ---------------------------------------------------------------------------

/// Per-thread dimension: Linux nice is per-task (`setpriority(PRIO_PROCESS,
/// tid, …)`).
fn apply_nice(tid: u32, nice: i8) -> ActionOutcome {
    match setpriority_raw(tid, nice) {
        Ok(()) => ActionOutcome::Applied,
        Err(e) => classify_errno(e, DIM_NICE),
    }
}

fn latency_value(policy: &Policy) -> Option<i8> {
    // Parity: absent latency_nice falls back to nice.
    policy.latency_nice.or(policy.nice)
}

fn apply_latency_nice(tid: u32, value: i8) -> ActionOutcome {
    let mut attr = SchedAttr::latency(value);
    match sched_setattr_raw(tid as libc::pid_t, &mut attr) {
        Ok(()) => ActionOutcome::Applied,
        Err(err) => match err {
            libc::ESRCH => ActionOutcome::Gone,
            // EINVAL/ENOSYS: the kernel rejected LATENCY_NICE outright —
            // report as unsupported, matching the startup probe.
            libc::EPERM | libc::EACCES | libc::EINVAL | libc::ENOSYS => {
                ActionOutcome::Denied(DIM_LATENCY_NICE)
            }
            e => ActionOutcome::Failed(e),
        },
    }
}

fn apply_sched(tid: u32, sched: SchedPolicy, rtprio: Option<u8>) -> ActionOutcome {
    match setscheduler_raw(tid, sched, rtprio) {
        Ok(()) => ActionOutcome::Applied,
        Err(e) => classify_errno(e, DIM_SCHED),
    }
}

fn apply_ionice(tid: u32, class: IoClass, level: Option<u8>) -> ActionOutcome {
    match ioprio_set_raw(tid as libc::pid_t, encode_ioprio(class, level)) {
        Ok(()) => ActionOutcome::Applied,
        Err(e) => classify_errno(e, DIM_IONICE),
    }
}

fn apply_cpuset(tid: u32, mask: &[u64]) -> ActionOutcome {
    match setaffinity_raw(tid, mask) {
        Ok(()) => ActionOutcome::Applied,
        Err(e) => classify_errno(e, DIM_CPUSET),
    }
}

fn read_tids(pid: u32) -> io::Result<Vec<u32>> {
    let mut sp = StackPath::new();
    if !proc_entry_path(pid, b"task", &mut sp) {
        return Err(io::Error::from_raw_os_error(libc::ENAMETOOLONG));
    }
    let mut tids = Vec::new();
    for entry in fs::read_dir(sp.as_path())? {
        let entry = entry?;
        if let Some(tid) = parse_u32_bytes(entry.file_name().as_bytes()) {
            tids.push(tid);
        }
    }
    tids.sort_unstable();
    Ok(tids)
}

/// Apply every policy dimension for `pid`. One task-dir walk, fixed order
/// (nice → latency_nice → sched → ionice → oom_score_adj → cgroup → cpuset,
/// autogroup last), ananicy-cpp parity. Short-lived processes: report `Gone`,
/// keep going with the remaining actions and tids.
pub fn apply_policy(
    pid: u32,
    policy: &Policy,
    opts: &ApplyOptions,
    caps: &Capabilities,
    cgroups: &CgroupManager,
) -> ApplyReport {
    let mut report = ApplyReport::default();

    let want_nice = opts.nice && policy.nice.is_some();
    let want_latency = opts.latency_nice && caps.latency_nice && latency_value(policy).is_some();
    let want_sched = opts.sched && policy.sched.is_some();
    let want_ionice = opts.ionice && policy.ioclass.is_some();
    let want_oom = opts.oom_score_adj && policy.oom_score_adj.is_some();
    let rt = matches!(policy.sched, Some(SchedPolicy::Fifo | SchedPolicy::Rr));
    let workaround = opts.cgroup_realtime_workaround && rt;
    // The RT workaround moves the task to the root cgroup even without a
    // `cgroup` rule field: an RT task must not sit in a throttled cgroup.
    let want_cgroup = opts.cgroup && (policy.cgroup.is_some() || workaround);
    let want_autogroup = opts.autogroup && policy.autogroup.is_some();

    // cpuset masks resolve once per process; a bad alias or parse is a rule
    // error ("unsupported/invalid cpuset"), reported without a fake errno.
    let cpuset_mask = match &policy.cpuset {
        Some(cs) if opts.cpuset => match resolve_cpuset(cs) {
            Ok(mask) => Some(mask),
            Err(_) => {
                report.cpuset = ActionOutcome::Denied(DIM_CPUSET);
                None
            }
        },
        _ => None,
    };
    let want_cpuset = cpuset_mask.is_some();

    // Single /proc/<pid>/task walk (Phase 0 root cause #7).
    let need_tasks = want_nice || want_latency || want_sched || want_ionice || want_cpuset;
    let (walked, tids) = if need_tasks {
        match read_tids(pid) {
            Ok(tids) => (true, tids),
            Err(e) => {
                if want_nice {
                    report.nice = classify_io(&e, DIM_NICE, true);
                }
                if want_latency {
                    report.latency_nice = classify_io(&e, DIM_LATENCY_NICE, true);
                }
                if want_sched {
                    report.sched = classify_io(&e, DIM_SCHED, true);
                }
                if want_ionice {
                    report.ionice = classify_io(&e, DIM_IONICE, true);
                }
                if want_cpuset {
                    report.cpuset = classify_io(&e, DIM_CPUSET, true);
                }
                (false, Vec::new())
            }
        }
    } else {
        (false, Vec::new())
    };

    if walked {
        // 1. nice (per tid)
        if let Some(v) = policy.nice.filter(|_| want_nice) {
            let mut acc = ActionOutcome::Skipped;
            for &tid in &tids {
                acc = merge_dim(acc, apply_nice(tid, v));
            }
            report.nice = acc;
        }
        // 2. latency_nice (per tid)
        if let Some(v) = latency_value(policy).filter(|_| want_latency) {
            let mut acc = ActionOutcome::Skipped;
            for &tid in &tids {
                acc = merge_dim(acc, apply_latency_nice(tid, v));
            }
            report.latency_nice = acc;
        }
        // 3. sched+rtprio (per tid)
        if let Some(sched) = policy.sched.filter(|_| want_sched) {
            let mut acc = ActionOutcome::Skipped;
            for &tid in &tids {
                acc = merge_dim(acc, apply_sched(tid, sched, policy.rtprio));
            }
            report.sched = acc;
        }
        // 4. ioclass+ionice (per tid)
        if let Some(class) = policy.ioclass.filter(|_| want_ionice) {
            let mut acc = ActionOutcome::Skipped;
            for &tid in &tids {
                acc = merge_dim(acc, apply_ionice(tid, class, policy.ionice));
            }
            report.ionice = acc;
        }
    }

    // 5. oom_score_adj (process-level, once)
    if want_oom {
        let mut val = StackPath::new();
        if val.push_i64(i64::from(policy.oom_score_adj.unwrap_or(0))) {
            report.oom_score_adj = write_proc_entry(
                pid,
                b"oom_score_adj",
                val.as_bytes(),
                DIM_OOM,
            );
        } else {
            report.oom_score_adj = ActionOutcome::Failed(libc::ENAMETOOLONG);
        }
    }

    // 6. cgroup (process-level, once)
    if want_cgroup {
        report.cgroup = if workaround {
            // RT workaround: skip the rule's cgroup, pin to the root cgroup.
            cgroups.assign(pid, "")
        } else {
            cgroups.assign(pid, policy.cgroup.as_deref().unwrap_or(""))
        };
    }

    // 7. cpuset (per tid)
    if walked {
        if let Some(mask) = &cpuset_mask {
            let mut acc = ActionOutcome::Skipped;
            for &tid in &tids {
                acc = merge_dim(acc, apply_cpuset(tid, mask));
            }
            report.cpuset = acc;
        }
    }

    // 8. autogroup (process-level, once; superset, applied last)
    if want_autogroup {
        report.autogroup = apply_autogroup(pid, policy.autogroup.unwrap_or(0));
    }

    report
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn parse_cpu_list_ranges_to_bits() {
        // "0-3,8-11" -> bits 0..3 and 8..11 = 0x0f0f
        assert_eq!(parse_cpu_list("0-3,8-11").unwrap(), vec![0x0f0f]);
        assert_eq!(parse_cpu_list("0,2,4").unwrap(), vec![0x15]);
        // Words beyond the first follow cpu_set_t layout.
        assert_eq!(parse_cpu_list("64-65").unwrap(), vec![0, 0b11]);
    }

    #[test]
    fn parse_cpu_list_errors() {
        assert!(parse_cpu_list("").is_err());
        assert!(parse_cpu_list("a").is_err());
        assert!(parse_cpu_list("4-2").is_err());
        assert!(parse_cpu_list("1024").is_err());
        assert!(parse_cpu_list("0-1024").is_err());
    }

    #[test]
    fn explicit_cpuset_resolves_without_sysfs() {
        let v = CpuSet::Explicit("0-3,8-11".into());
        assert_eq!(resolve_cpuset(&v).unwrap(), vec![0x0f0f]);
    }

    #[test]
    fn unknown_alias_is_an_error_not_a_panic() {
        let v = CpuSet::Alias("warp-cores".into());
        assert!(resolve_cpuset(&v).unwrap_err().contains("unknown cpuset alias"));
        let v = CpuSet::Alias("turbo-cores".into());
        assert!(resolve_cpuset(&v).is_err());
        let v = CpuSet::Alias("x3d-cache".into());
        assert!(resolve_cpuset(&v).is_err());
    }

    #[test]
    fn cpu_quota_math_and_format() {
        // 50% of 8 cpus -> 4 cores of budget = 400000us per 100000us period.
        assert_eq!(cpu_quota_micros(50, 8), 400_000);
        let mut buf = StackPath::new();
        assert!(format_cpu_max(400_000, &mut buf));
        assert_eq!(buf.as_bytes(), b"400000 100000");
        assert_eq!(cpu_quota_micros(100, 4), 400_000);
    }

    #[test]
    fn sched_policy_mapping() {
        assert_eq!(map_sched(SchedPolicy::Fifo), libc::SCHED_FIFO);
        assert_eq!(map_sched(SchedPolicy::Rr), libc::SCHED_RR);
        assert_eq!(map_sched(SchedPolicy::Normal), libc::SCHED_OTHER);
        assert_eq!(map_sched(SchedPolicy::Batch), libc::SCHED_BATCH);
        assert_eq!(map_sched(SchedPolicy::Idle), libc::SCHED_IDLE);
    }

    #[test]
    fn rtprio_only_for_fifo_rr() {
        assert_eq!(resolve_rtprio(SchedPolicy::Fifo, None), 1);
        assert_eq!(resolve_rtprio(SchedPolicy::Rr, None), 1);
        assert_eq!(resolve_rtprio(SchedPolicy::Rr, Some(50)), 50);
        assert_eq!(resolve_rtprio(SchedPolicy::Normal, Some(50)), 0);
        assert_eq!(resolve_rtprio(SchedPolicy::Idle, Some(99)), 0);
    }

    #[test]
    fn ioprio_value_encoding() {
        // Best-effort level 3: (2 << 13) | 3 = 16387.
        assert_eq!(encode_ioprio(IoClass::BestEffort, Some(3)), 16387);
        assert_eq!(encode_ioprio(IoClass::Realtime, Some(7)), (1 << 13) | 7);
        // Idle/none ignore the level; absent level defaults to 0.
        assert_eq!(encode_ioprio(IoClass::Idle, Some(7)), 3 << 13);
        assert_eq!(encode_ioprio(IoClass::None, Some(7)), 0);
        assert_eq!(encode_ioprio(IoClass::BestEffort, None), 2 << 13);
        // Defensive clamp of out-of-range levels to 7.
        assert_eq!(encode_ioprio(IoClass::BestEffort, Some(200)), (2 << 13) | 7);
    }

    #[test]
    fn error_taxonomy_from_errno() {
        assert_eq!(classify_errno(libc::ESRCH, DIM_NICE), ActionOutcome::Gone);
        assert_eq!(
            classify_errno(libc::EPERM, DIM_SCHED),
            ActionOutcome::Denied(DIM_SCHED)
        );
        assert_eq!(
            classify_errno(libc::EACCES, DIM_SCHED),
            ActionOutcome::Denied(DIM_SCHED)
        );
        assert_eq!(
            classify_errno(libc::EINVAL, DIM_NICE),
            ActionOutcome::Failed(libc::EINVAL)
        );
        // ENOENT on /proc/<pid>/… entries is the process-gone case.
        let gone = io::Error::from_raw_os_error(libc::ENOENT);
        assert_eq!(classify_io(&gone, DIM_OOM, true), ActionOutcome::Gone);
        assert_eq!(
            classify_io(&gone, DIM_CGROUP, false),
            ActionOutcome::Failed(libc::ENOENT)
        );
    }

    #[test]
    fn report_order_matches_compat_contract() {
        let report = ApplyReport::default();
        let dims: Vec<&str> = report.ordered().iter().map(|(n, _)| *n).collect();
        assert_eq!(
            dims,
            [
                "nice",
                "latency_nice",
                "sched",
                "ionice",
                "oom_score_adj",
                "cgroup",
                "cpuset",
                "autogroup",
            ]
        );
    }

    #[test]
    fn merge_prefers_strictest_outcome() {
        use ActionOutcome::*;
        assert_eq!(merge_dim(Applied, Gone), Gone);
        assert_eq!(merge_dim(Gone, Applied), Gone);
        assert_eq!(merge_dim(Applied, Failed(5)), Failed(5));
        assert_eq!(merge_dim(Denied(DIM_NICE), Applied), Denied(DIM_NICE));
        assert_eq!(merge_dim(Gone, Denied(DIM_NICE)), Denied(DIM_NICE));
        assert_eq!(merge_dim(Skipped, Applied), Applied);
        assert_eq!(merge_dim(Skipped, Skipped), Skipped);
    }

    #[test]
    fn latency_nice_falls_back_to_nice() {
        let p = Policy {
            nice: Some(7),
            ..Default::default()
        };
        assert_eq!(latency_value(&p), Some(7));
        let p = Policy {
            nice: Some(7),
            latency_nice: Some(-2),
            ..Default::default()
        };
        assert_eq!(latency_value(&p), Some(-2));
        assert_eq!(latency_value(&Policy::default()), None);
    }

    #[test]
    fn autogroup_line_round_trip() {
        let mut line = StackPath::new();
        assert!(format_autogroup_line(12, -3, &mut line));
        assert_eq!(line.as_bytes(), b"12 (nice -3)");
        assert_eq!(parse_autogroup_id(b"12 (nice -3)\n"), Some(12));
        assert_eq!(parse_autogroup_id(b"(nice 0)"), None);
    }

    // ---- root-gated integration tests ------------------------------------
    //
    // These exercise live syscalls against a real child and need root (or
    // CAP_SYS_NICE + proc write access). Run with:
    //     sudo cargo test -p aranicy-apply -- --ignored
    // Each test kills and reaps its own child on exit.

    /// Applies nice 7 + best-effort ionice 3 to a `sleep` child and verifies
    /// them via /proc/<pid>/stat plus libc getpriority/ioprio_get.
    #[test]
    #[ignore = "root-gated: run `sudo cargo test -p aranicy-apply -- --ignored`"]
    fn root_apply_nice_and_ionice() {
        let mut child = Command::new("sleep").arg("30").spawn().expect("spawn sleep");
        let pid = child.id();
        let policy = Policy {
            nice: Some(7),
            ioclass: Some(IoClass::BestEffort),
            ionice: Some(3),
            ..Default::default()
        };
        let caps = probe_capabilities();
        let cgroups = CgroupManager::init(BTreeMap::new()).expect("cgroup2 mount");
        let report = apply_policy(pid, &policy, &ApplyOptions::default(), &caps, &cgroups);

        assert_eq!(report.nice, ActionOutcome::Applied, "nice outcome");
        assert_eq!(report.ionice, ActionOutcome::Applied, "ionice outcome");

        let got = unsafe { libc::getpriority(libc::PRIO_PROCESS, pid as libc::id_t) };
        assert_eq!(got, 7, "getpriority");
        assert_eq!(proc_stat_nice(pid).expect("read /proc/<pid>/stat"), 7, "stat nice");
        assert_eq!(
            ioprio_get_raw(pid as libc::pid_t),
            encode_ioprio(IoClass::BestEffort, Some(3)),
            "ioprio_get"
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    /// Verifies the process-scoped writes: oom_score_adj round-trips through
    /// /proc; autogroup must be Applied or Denied (kernel support), never a
    /// hard failure, and Applied must show nice 5 in /proc/<pid>/autogroup.
    #[test]
    #[ignore = "root-gated: run `sudo cargo test -p aranicy-apply -- --ignored`"]
    fn root_apply_oom_and_autogroup() {
        let mut child = Command::new("sleep").arg("30").spawn().expect("spawn sleep");
        let pid = child.id();
        let policy = Policy {
            oom_score_adj: Some(123),
            autogroup: Some(5),
            ..Default::default()
        };
        let caps = probe_capabilities();
        let cgroups = CgroupManager::init(BTreeMap::new()).expect("cgroup2 mount");
        let report = apply_policy(pid, &policy, &ApplyOptions::default(), &caps, &cgroups);

        assert_eq!(report.oom_score_adj, ActionOutcome::Applied, "oom outcome");
        let oom = fs::read_to_string(format!("/proc/{pid}/oom_score_adj")).expect("read oom");
        assert_eq!(oom.trim(), "123", "oom_score_adj value");

        assert!(
            matches!(
                report.autogroup,
                ActionOutcome::Applied | ActionOutcome::Denied(_)
            ),
            "autogroup outcome {:?}",
            report.autogroup
        );
        if report.autogroup == ActionOutcome::Applied {
            let ag = fs::read_to_string(format!("/proc/{pid}/autogroup")).expect("read autogroup");
            // Both ABIs end in the nice value: "<id> (nice 5)" or
            // "/autogroup-<id> nice 5".
            let got = ag
                .rsplit_once("nice ")
                .and_then(|(_, v)| v.trim().parse::<i32>().ok());
            assert_eq!(got, Some(5), "autogroup line {ag:?}");
        }

        let _ = child.kill();
        let _ = child.wait();
    }

    /// Field 19 (nice) of /proc/<pid>/stat; the parenthesised comm may hold
    /// spaces, so parsing resumes after the last ')'.
    fn proc_stat_nice(pid: u32) -> Option<i32> {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let tail = &stat[stat.rfind(')')? + 1..];
        // After comm: state(3) ppid(4) … nice(19) -> 17th whitespace field.
        let nice = tail.split_whitespace().nth(16)?;
        nice.parse().ok()
    }
}
