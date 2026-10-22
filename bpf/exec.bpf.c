// SPDX-License-Identifier: GPL-2.0
//
// aranicy exec event source: sched_process_exec only (fork/exit are wasted
// volume per docs/BENCHMARKS.md design consequence 3). No in-kernel filtering:
// the userspace matcher owns semantics.
//
// CO-RE build (clang -g: BTF in the object, kernel BTF resolved at load time).
// Only stable helpers and the raw tracepoint context are used, so there are no
// struct-field relocations; the program is layout-agnostic across kernels by
// construction. Patterns reused from tools/bpf-probe/probe.bpf.c.
#include <linux/bpf.h>
#include <bpf/bpf_helpers.h>

#include "exec_event.h"

char LICENSE[] SEC("license") = "GPL";

/* ~43k buffered samples; at the measured 2k execs/s storms this absorbs
 * multi-hundred-ms consumer stalls without drops. */
struct {
	__uint(type, BPF_MAP_TYPE_RINGBUF);
	__uint(max_entries, 1 << 20);
} events SEC(".maps");

SEC("tp/sched/sched_process_exec")
int handle_exec(void *ctx)
{
	struct exec_event *e;

	e = bpf_ringbuf_reserve(&events, sizeof(*e), 0);
	if (!e)
		return 0; /* overrun: drop silently, never block the scheduler */

	e->pid = bpf_get_current_pid_tgid() >> 32; /* tgid == process id */
	e->comm_len = 0;
	e->pad[0] = 0;
	e->pad[1] = 0;
	e->pad[2] = 0;
	__builtin_memset(e->comm, 0, sizeof(e->comm));

	if (bpf_get_current_comm(e->comm, sizeof(e->comm)) >= 0) {
		/* comm is NUL-padded TASK_COMM_LEN; count usable bytes. */
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
