/* SPDX-License-Identifier: MIT
 *
 * Wire format of one sched_process_exec ringbuf sample.
 *
 * Single source of truth for the C side. The Rust side mirrors these values in
 * crates/aranicy-events/src/raw.rs with const assertions + unit tests; a
 * mismatch corrupts decoding, so both sides assert their own layout against
 * the same numbers below. Keep in sync.
 */
#ifndef ARANICY_EXEC_EVENT_H
#define ARANICY_EXEC_EVENT_H

#ifndef TASK_COMM_LEN
#define TASK_COMM_LEN 16
#endif

#define EXEC_EVENT_SIZE 24
#define EXEC_EVENT_PID_OFFSET 0
#define EXEC_EVENT_COMM_LEN_OFFSET 4
#define EXEC_EVENT_PAD_OFFSET 5
#define EXEC_EVENT_COMM_OFFSET 8

struct exec_event {
	__u32 pid;			     /* tgid of the exec'ing task */
	__u8 comm_len;			     /* usable bytes in comm (NUL-truncated) */
	__u8 pad[3];			     /* zeroed; keeps comm 8-byte aligned */
	__u8 comm[TASK_COMM_LEN];	     /* NUL-padded kernel comm */
};

/* C-side layout guards (mirrored 1:1 in raw.rs). */
_Static_assert(sizeof(struct exec_event) == EXEC_EVENT_SIZE,
	       "exec_event size must match EXEC_EVENT_SIZE");
_Static_assert(__builtin_offsetof(struct exec_event, pid) == EXEC_EVENT_PID_OFFSET,
	       "exec_event.pid offset mismatch");
_Static_assert(__builtin_offsetof(struct exec_event, comm_len) == EXEC_EVENT_COMM_LEN_OFFSET,
	       "exec_event.comm_len offset mismatch");
_Static_assert(__builtin_offsetof(struct exec_event, comm) == EXEC_EVENT_COMM_OFFSET,
	       "exec_event.comm offset mismatch");

#endif /* ARANICY_EXEC_EVENT_H */
