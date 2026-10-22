// Minimal falsification probe: does tp/sched/sched_process_exec work on this kernel?
// Counter-only; no CO-RE struct access needed for the base probe.
#include <linux/bpf.h>
#include <bpf/bpf_helpers.h>

struct {
	__uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
	__uint(max_entries, 1);
	__type(key, __u32);
	__type(value, __u64);
} exec_count SEC(".maps");

SEC("tp/sched/sched_process_exec")
int probe_handle_exec(void *ctx)
{
	__u32 key = 0;
	__u64 *val;

	val = bpf_map_lookup_elem(&exec_count, &key);
	if (val)
		__sync_fetch_and_add(val, 1);
	return 0;
}

char LICENSE[] SEC("license") = "GPL";
