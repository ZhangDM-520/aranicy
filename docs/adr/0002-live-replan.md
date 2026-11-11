# ADR 0002 — a reload re-plans live processes, and a deleted rule gives back

Status: **accepted**. Date: 2026-11-11. Evidence: round-3 `kaz-8`, `kaz-11`.

## Context

`--reload` swaps the rule tree atomically (`RwLock<Arc<RuleSet>>`). Nothing
re-examines the processes already running, so the new corpus reaches them only
through two narrow paths:

- **drift** (`check_freq`, default 60 s) re-plans every pid, but only acts on
  a `nice` that actually moved (`settle`): a rule edit to `ioclass`, `ionice`,
  `oom_score_adj`, `sched` or `cgroup` never reaches a running process;
- **the learner**, which owns `nice` for a name it has evidence on.

Deleting a rule therefore did two things an operator does not expect: the
rule's dimensions stayed applied (they were never revisited), and the `nice`
the rule had set was replaced by the learned value — measured `-7` → `+3` on a
name the learner liked. "Edit the corpus" was not a way to change a running
system; only "restart everything" was.

## Decision

On a successful reload the daemon runs one reconcile pass over `/proc`:

1. For every live, non-kernel, non-PID-1 process, resolve the name and plan it
   against the **new** corpus (same `plan` the event route uses).
2. **Matched** → apply the full planned policy. It is idempotent, and it is the
   only thing an edit to a non-`nice` dimension can reach. No comparison with
   live state: re-applying is cheaper and more honest than a per-dimension
   read-back that would need its own drift policy.
3. **No longer matched, but matched by the tree being replaced** — or matched
   by both trees while the new rule no longer declares a field the old one
   did ("the field was deleted, the rule stayed" is the same edit from the
   operator's seat) → apply what the old rule must give back: `nice`/`latency_nice` 0, `sched` normal,
   `rtprio` 0, `ioclass` none, `ionice` 0, `oom_score_adj` 0, `cgroup` root,
   `autogroup` 0 — each only when the old rule actually set it and the new
   plan does not. A `nice` the learner now wants survives (the release fills
   the gaps in that plan, it does not overwrite it), and `latency_nice` is
   never filled when the plan carries a `nice` — the documented fallback is
   the value in force, so filling 0 would silently retune a `nice`-only rule.
4. Everything else is left alone.

`cpuset` is **not** released. There is no safe "unset": the process may be
confined by its unit or a container, and widening affinity to every CPU is
worse than leaving a stale set in place. An edit that changes `cpuset` still
lands via (2); a deletion leaves the old affinity, which the journal line and
this ADR make discoverable. The line counts the two facts apart —
`released=N` is processes whose dimensions were actually reset, `kept_cpuset=M`
is processes whose affinity was left in place — so neither number claims work
the other did.

## Consequences and limits

- Reloads are rare; the pass costs one `read_stat` + one plan + a few syscalls
  per process. Counters are throwaway so a reload does not look like an event
  storm in the `apply summary`.
- Startup does **not** reconcile: the previous corpus is not persisted, so a
  daemon that starts with an edited corpus cannot know what the last instance
  applied. Restarting is not "reloading" — the operator gets the reconciling
  behaviour from `--reload`, and the limitation is documented in
  `COMPATIBILITY.md`.
- An external actor changing a non-`nice` dimension between reloads is not
  corrected until the next reload; drift remains the `nice`-only corrector it
  always was.
- Releasing `nice` to 0 is a statement about the kernel default, not about the
  value the process inherited before the rule existed — that history is not
  recorded and would need a per-pid ledger.
