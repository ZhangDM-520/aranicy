# session log — dana, aranicy 0.3.0 round 1

Chronological `command -> outcome`. Dead ends and surprises kept in. Machine:
`su-skeptic-r1`, Linux 7.2.11-arch1-1, 2 vCPU, 1958 MiB, Arch rolling.

## Recon

- `uname -a; nproc; free -m; ls ~/payload` -> 2 vCPU / 1958 MiB; both packages present,
  sizes match the manifest.
- `bsdtar -xOf *.pkg.tar.zst .PKGINFO` -> `aranicy` MIT, `aranicy-collector` GPL-2.0-only,
  same pkgbase; aranicy depends on `aranicy-collector=0.3.0`.
- `bsdtar -tf` both packages -> 9 files each; **no rules shipped**, no docs/*.md, no
  `contrib/install.sh`. First surprise: the README I am about to follow links to files
  that do not exist.
- `cat ananicy.conf`, both .service files -> `check_freq=60`, claims to be "delta-based,
  never a full re-apply scan"; daemon unit `Requires=`+`After=` the collector, caps
  CAP_SYS_NICE/SYS_RESOURCE/DAC_OVERRIDE/SYS_PTRACE; collector owns CAP_BPF.

## Naive install

- `pacman -Q ananicy-cpp` -> not installed (clean box, no legacy corpus).
- `sudo pacman -U --noconfirm *.pkg.tar.zst` -> rc=0, both installed.
- `sudo systemctl enable --now aranicy` -> active (running), collector pulled in by
  `Requires=`, both sockets present under `/run/aranicy` (0700 root).
- `getpcaps 1098` -> exactly the four caps in the unit. No CAP_BPF in the daemon. Good.
- journal -> `WARN kernel does not support latency_nice ... will be skipped` (env note),
  `stats sampler live ... bound SchedSample via sched_samples`.
- `aranicy dump rules` -> `{"exact": [], "regex": []}` — a stock install applies nothing.
  Combined with `sched_ext state=disabled`, the only moving part is the bias engine.
- `aranicy --help` / `dump --help` / `match-test --help` -> match-test documents the
  identity as `basename(argv[0])`. Filed that thought for later; it became dana-3.

## Rule engine smoke test

- Wrote `/etc/ananicy.d/test.rules` with a bad `ionice` object -> reload warns
  `ionice: expected integer, got object`, drops that one rule, keeps the other, reports
  success. Acceptable, noted.
- `cp /bin/sleep ~/skeptictest; ~/skeptictest 30` -> `/proc/<pid>/stat` field 19 = 12. The
  core promise works.
- `systemctl reload aranicy` -> `ExecReload=/usr/bin/aranicy --reload`, sends SIGHUP, works.

## Hostile pass

- Unprivileged `aranicy --reload` -> `Error: failed to signal aranicy (pid ...): Operation
  not permitted (os error 1)`, rc=1. Correct.
- Unprivileged `aranicy start` -> refuses, rc=1. Correct. (First attempt piped through
  `head` and I read head's rc — re-ran to get the real 1.)
- Half-written config (`check_freq = `) + reload -> `WARN config: line 1: invalid
  check_freq`, daemon stays ALIVE, old value kept, but `systemctl reload` still reports
  success. Minor, not filed.
- Junk rule corpus (garbage text, truncated JSON, 20 003 rules, duplicate names) ->
  `WARN ... no JSON object span`, `reloaded 20003 rules`, daemon ALIVE, exec policies still
  applied. Robust. Not filed.
- Pathological regex `(a+)+$` -> no CPU spike, no stall; the matcher looks linear. Dead
  end, nothing to report. (My first probe used a name ending in `b`, which cannot match
  `(a+)+$` — wasted a turn, then `match-test` showed me why.)
- Self-renaming process (`prctl(PR_SET_NAME, "skeptictest")` on a process whose cmdline[0]
  is `python3`) -> no rule applied over 65 s. Name resolution is cmdline-based, not comm.
- `kill -9` the collector mid-run -> daemon notices (`stats sampler degraded ... transport
  down detail=write: Broken pipe`), systemd restarts both, ~5 s outage, exec policies
  resume. Good behaviour, not filed.
- `systemctl stop aranicy` while running -> clean deactivation, sockets removed.

## The two surprises that became blockers

1. Broad-rule experiment: I loaded `{"name":"systemd","nice":19}` to see whether a
   plausible bad rule can reach PID 1. It can. But the arrival was not at reload — it was
   at the **first periodic pass ~60 s later**, on a process that has never exec'd under the
   daemon. Confirmed by stopping the daemon (PID 1 stays 0 for 70 s) and starting it
   (PID 1 becomes 19 at t=60 s). Config comment says "never a full re-apply scan".
2. While that rule was loaded, `journalctl` showed `bias changed name=systemd bias=2
   prev=1 class=Bulk cause=Evidence`. PID 1 was being *learned* into the bulk class and
   given +2. `dump bias` showed the persisted entry (class bulk, confidence 0.93), and a
   later restart re-applied it to PID 1 at the first pass. That is the central-claim
   falsification with a body count.

Side observation from the same window: the daemon reniced **itself** (rule `aranicy` -> 19;
bias -> 3), overriding `Nice=-5` from its own unit. Not filed separately; it is in the
blast-radius story.

## Falsification pass

- First attempt at a CPU-vs-latency comparison used `exec -a skepticcpu python3 ...` -> no
  bias entry appeared in 190 s. Cause: the bias engine keys on **comm**, while the rule
  engine keys on **argv[0]**. Two different identities for the same process; noted in
  dana-3. Redone with copied binaries so comm differs.
- `~/skepticcpu` (spinner) vs `~/skepticlat` (10 ms sleeper) for 200 s with no rules ->
  spinner `Bulk` +3 (score 3.9), shell `Bulk` +3 (score 1011), daemon `Bulk` +3 (score
  1177). Sleeper never appeared. Also noticed my own login bash was at nice 3 with its
  parent `sshd-session` at 0 — the daemon had niced my shell, and every command I ran
  inherited that. That cost me a turn chasing a phantom "no rule but nice=2" result.
- `pkill -f skepticcpu` killed my own SSH command (the pattern matched the shell running
  it) -> wrapper exit 255. Don't do that. Re-ran with explicit pids.

## Performance

- Calibration: 2 000 execs of `/bin/true` = 1 048 ms single worker.
- 60 000 execs, one worker: 1 732/s, daemon 2.40%, collector 7.22%, **9.61% of one core**.
- 60 000 execs, two workers: 3 316/s, daemon 3.43%, collector 12.22%, **15.64%**.
- Idle control for the same pids: 0.05% and 0.2%. Filed as dana-5 (doc budget vs shipped
  two-process reality), one sample per cell, stated as such.

## What I did not do

- No `strace` (not installed; brief forbids installing). Attribution for the PID 1 write is
  therefore by stop/start A-B, not by syscall.
- No ananicy-cpp comparison (not installed, not in payload).
- No two-FIFO-spinner starvation demo — on 2 vCPU that risks locking myself out and
  invalidating the rest of the session; the capability is proven with `chrt -p`.
