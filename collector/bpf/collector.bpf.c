// SPDX-License-Identifier: GPL-2.0-only
//
// aranicy-collectord: scheduler-agnostic per-task accounting.
//
// The kernel side only ACCUMULATES monotonic counters per task; every rate,
// EWMA and utilization figure is derived in userspace over a drain window
// (src/agg.rs). That keeps the hot path to two map lookups per switch and
// makes the arithmetic unit-testable with an injected clock.
//
// Why not PELT: `se.avg.*` is maintained by the FAIR class only. Under any
// sched_ext scheduler it stays 0 (measured on this host), so using it would
// silently freeze the utilization signal. We account running/runnable time
// ourselves from sched_switch.
//
// CO-RE: minimal struct definitions with preserve_access_index; no
// vmlinux.h and no build-time bpftool, mirroring crates/aranicy-events.
#include <linux/bpf.h>
#include <stdbool.h>
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>

#define TASK_COMM_LEN 16
/* `prev_state` of the final sched_switch of an exiting task. */
#define TASK_DEAD 0x0080

/*
 * Per-CPU preemption count. Bits 8..23 are the softirq/hardirq/NMI nesting
 * levels: a wake issued from an IRQ runs on the interrupted task's context,
 * and crediting its `current` would invent wakeups for whatever happened to be
 * burning that CPU (measured: a pure spinloop reported 22-45 wakes/s).
 */
extern const int __preempt_count __ksym;

static __always_inline int in_interrupt(void)
{
	const unsigned int *pc =
		(const unsigned int *)bpf_this_cpu_ptr(&__preempt_count);

	return pc && ((*pc & 0x00ffff00) != 0);
}

/* Minimal CO-RE view: only the fields we read are declared. */
struct task_struct {
	int pid;
	int tgid;
	int static_prio;
	char comm[TASK_COMM_LEN];
} __attribute__((preserve_access_index));

/*
 * Per-task accumulator, keyed by pid.
 *
 *   run_ns        cumulative time scheduled on a CPU
 *   runnable_ns   cumulative time runnable but not running
 *   runs          times scheduled in
 *   sleeps        times switched out to a non-running state
 *   wakes_issued  times this task woke another task
 *
 * Timestamps are CLOCK_MONOTONIC (bpf_ktime_get_ns) and only ever move
 * forward; userspace takes deltas between two drains.
 */
struct task_acc {
	char comm[TASK_COMM_LEN];
	unsigned int static_prio; /* nice + 20, the lavd wire convention */
	unsigned int pad;
	unsigned long long run_ns;
	unsigned long long runnable_ns;
	unsigned long long runs;
	unsigned long long sleeps;
	unsigned long long wakes_issued;
	unsigned long long last_run_start;
	unsigned long long last_enqueue;
	unsigned long long last_seen;
	unsigned long long last_tick;
};

/*
 * LRU, not HASH: with a plain hash map a full table makes BPF_NOEXIST inserts
 * fail SILENTLY — new tasks get no accounting and simply never appear in the
 * sample stream (observed live: the table filled with stale entries at
 * 16384/16384 and every compiler process vanished from the samples). LRU lets
 * the kernel evict the coldest task instead, so the table always accepts a
 * new task and the userspace TTL sweep is only a tidiness measure.
 */
struct {
	__uint(type, BPF_MAP_TYPE_LRU_HASH);
	__uint(max_entries, 16384);
	__type(key, unsigned int);
	__type(value, struct task_acc);
} tasks SEC(".maps");

char LICENSE[] SEC("license") = "GPL";

static __always_inline struct task_acc *acc_get_or_create(struct task_struct *p,
							  unsigned long long now)
{
	unsigned int pid = (unsigned int)p->pid;
	struct task_acc *a = bpf_map_lookup_elem(&tasks, &pid);

	if (a)
		return a;

	struct task_acc zero = {};

	__builtin_memcpy(zero.comm, p->comm, TASK_COMM_LEN);
	/* static_prio is 120 + nice; the bias engine speaks nice + 20. */
	zero.static_prio = (unsigned int)(p->static_prio - 100);
	zero.last_seen = now;
	bpf_map_update_elem(&tasks, &pid, &zero, BPF_NOEXIST);
	return bpf_map_lookup_elem(&tasks, &pid);
}

SEC("tp_btf/sched_switch")
int BPF_PROG(on_sched_switch, bool preempt, struct task_struct *prev,
	     struct task_struct *next, unsigned int prev_state)
{
	unsigned long long now = bpf_ktime_get_ns();

	if (!prev)
		return 0;
	if (next && prev->pid == next->pid)
		return 0; /* switch to self: nothing moved, do not truncate the slice */

	{
		unsigned int prev_pid = (unsigned int)prev->pid;
		unsigned int next_pid = next ? (unsigned int)next->pid : 0;

		if (prev_pid != next_pid) {
			struct task_acc *a = acc_get_or_create(prev, now);

			if (a) {
				/* Flush the runtime the tick path has not seen yet.
				 * Kernels that emit sched_stat_runtime per tick have
				 * last_tick ~= now here, so this adds the residual
				 * since that tick; kernels where the tracepoint is
				 * fair-class only (no ticks under sched_ext) fall
				 * back to the switch-in timestamp. */
				if (a->last_tick) {
					if (now > a->last_tick)
						a->run_ns += now - a->last_tick;
				} else if (a->last_run_start && now > a->last_run_start) {
					a->run_ns += now - a->last_run_start;
				}
				a->last_tick = 0;
				a->last_run_start = 0;
				/* The live nice: the bias engine's closed-loop check
				 * compares it against what it applied, so a value
				 * captured only at exec would freeze every entry. */
				a->static_prio = (unsigned int)(prev->static_prio - 100);
				if (prev_state != 0 /* TASK_RUNNING */) {
					a->sleeps += 1;
					a->last_enqueue = 0;
				} else if (a->last_enqueue == 0) {
					/* preempted: runnable from now on */
					a->last_enqueue = now;
				}
				a->last_seen = now;
			}
		}
	}

	if (next) {
		struct task_acc *a = acc_get_or_create(next, now);

		if (a) {
			if (a->last_enqueue && now > a->last_enqueue)
				a->runnable_ns += now - a->last_enqueue;
			a->last_enqueue = 0;
			a->last_run_start = now;
			a->last_tick = 0;
			a->static_prio = (unsigned int)(next->static_prio - 100);
			__builtin_memcpy(a->comm, next->comm, TASK_COMM_LEN);
			a->runs += 1;
			a->last_seen = now;
		}
	}
	return 0;
}

/*
 * Per-tick runtime accounting for the CURRENT task: the only source that
 * covers a CPU-bound task which never switches out mid-life. Upstream emits
 * this tracepoint from the fair class only, so under sched_ext kernels that
 * do not route it through the core path it never fires; `on_sched_switch`
 * flushes the residual from `last_tick`/`last_run_start` in that case, so
 * run_ns stays exact either way.
 */
SEC("tp_btf/sched_stat_runtime")
int BPF_PROG(on_sched_stat_runtime, struct task_struct *tsk,
	     unsigned long long runtime, unsigned long long vruntime)
{
	unsigned long long now = bpf_ktime_get_ns();
	struct task_acc *a = acc_get_or_create(tsk, now);

	if (a) {
		a->run_ns += runtime;
		a->last_tick = now;
		a->static_prio = (unsigned int)(tsk->static_prio - 100);
		__builtin_memcpy(a->comm, tsk->comm, TASK_COMM_LEN);
		a->last_seen = now;
	}
	return 0;
}

/*
 * Exec seed: no sched_switch happens at exec, and the task may then run to
 * completion without ever switching out. Without this the entry is only
 * created on the way out (or never), so build tools were invisible. exec runs
 * in the task's own context, i.e. it IS running: seed the run and the comm
 * (the image changed, so a reused pid's old name must not linger).
 */
SEC("tp_btf/sched_process_exec")
int BPF_PROG(on_sched_process_exec_account, struct task_struct *p)
{
	unsigned long long now = bpf_ktime_get_ns();
	struct task_acc *a = acc_get_or_create(p, now);

	if (a) {
		__builtin_memcpy(a->comm, p->comm, TASK_COMM_LEN);
		a->static_prio = (unsigned int)(p->static_prio - 100);
		/* exec replaces the image: the old identity's counters must not
		 * bleed into the new one. Leaving `last_tick` set would also let
		 * the next switch back-date the slice to the pre-exec interval. */
		a->run_ns = 0;
		a->runnable_ns = 0;
		a->runs = 1;
		a->sleeps = 0;
		a->wakes_issued = 0;
		a->last_run_start = now;
		a->last_enqueue = 0;
		a->last_tick = 0;
		a->last_seen = now;
	}
	return 0;
}

/*
 * A recycled pid may still carry the dead occupant's accumulator (the map is
 * LRU and has no exit cleanup). Reset it when the kernel hands the pid to a
 * brand-new task, before that task can be accounted anywhere.
 */
SEC("tp_btf/sched_process_fork")
int BPF_PROG(on_sched_process_fork, struct task_struct *parent,
	     struct task_struct *child)
{
	unsigned int pid = (unsigned int)child->pid;

	bpf_map_delete_elem(&tasks, &pid);
	return 0;
}

/*
 * Wakes are attributed to their WAKER, which only `sched_waking` guarantees:
 * it is emitted in the waking task's context, whereas `sched_wakeup` runs on
 * the target CPU and its "current" task is the CPU's occupant. This one hook
 * also stamps the wakee's enqueue time (the old `sched_wakeup` hook did that
 * even for `ttwu_runnable` wakeups of a task that never stopped running,
 * which counted running time as both run and runnable).
 */
SEC("tp_btf/sched_waking")
int BPF_PROG(on_sched_waking, struct task_struct *p)
{
	unsigned long long now = bpf_ktime_get_ns();
	unsigned int wakee_pid = (unsigned int)p->pid;
	struct task_acc *wakee = acc_get_or_create(p, now);

	if (wakee && wakee->last_enqueue == 0 && wakee->last_run_start == 0)
		wakee->last_enqueue = now;

	if (in_interrupt())
		return 0;

	struct task_struct *waker = (struct task_struct *)bpf_get_current_task_btf();

	if (!waker || waker->pid == 0 || (unsigned int)waker->pid == wakee_pid)
		return 0;

	struct task_acc *a = acc_get_or_create(waker, now);

	if (a) {
		a->wakes_issued += 1;
		a->last_seen = now;
	}
	return 0;
}

/*
 * No `sched_process_exit` cleanup: the tracepoint fires from do_exit() BEFORE
 * the final sched_switch, so deleting there discarded the last window and let
 * the switch re-create a zeroed entry (a phantom row that dragged the comm's
 * utilization down). The map is LRU precisely so stale entries evict on their
 * own, and pid reuse is caught in userspace by the comm check.
 */

/*
 * Exec events (moved here from aranicy-events so the collector owns ALL BPF).
 * Classic tracepoint + bpf_get_current_comm: the userspace side resolves the
 * real match name from /proc immediately after the event, which removes the
 * process-exit race a downstream consumer would otherwise hit.
 */
struct exec_event {
	unsigned int pid;
	unsigned char comm_len;
	unsigned char pad[3];
	char comm[TASK_COMM_LEN];
};

struct {
	__uint(type, BPF_MAP_TYPE_RINGBUF);
	__uint(max_entries, 1 << 20);
} exec_events SEC(".maps");

SEC("tp/sched/sched_process_exec")
int handle_exec(void *ctx)
{
	struct exec_event *e;

	e = bpf_ringbuf_reserve(&exec_events, sizeof(*e), 0);
	if (!e)
		return 0; /* overrun: never block the exec path */

	e->pid = (unsigned int)(bpf_get_current_pid_tgid() >> 32);
	e->comm_len = 0;
	e->pad[0] = e->pad[1] = e->pad[2] = 0;
	__builtin_memset(e->comm, 0, sizeof(e->comm));

	if (bpf_get_current_comm(e->comm, sizeof(e->comm)) >= 0) {
#pragma unroll
		for (int i = 0; i < TASK_COMM_LEN; i++) {
			if (e->comm[i] == '\0')
				break;
			e->comm_len = i + 1;
		}
	}

	bpf_ringbuf_submit(e, 0);
	return 0;
}
