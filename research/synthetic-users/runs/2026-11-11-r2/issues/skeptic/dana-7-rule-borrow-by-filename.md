# A rule that grants `nice -20` and `SCHED_FIFO` is still claimable by an unprivileged user: matching is on the *name* of the executed file, and the user supplies the file

- severity: major
- category: safety
- environment: Linux 7.2.11-arch1-1 (Arch rolling), aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB; test process uid 1000, no extra groups, no capabilities

## What I was trying to do

Re-run round 1's escalation (dana-3) against the new build. `COMPATIBILITY.md`
now argues the identity question explicitly:

> aranicy keys it on `basename(/proc/<pid>/exe)` ... The executable is the only
> one of the three the task cannot rewrite: `exec -a` sets `argv[0]` and
> `prctl(PR_SET_NAME)` sets `comm`, so either one would let a process choose
> which rule it is matched against — borrowing a rule it was never meant to
> have, or evading its own.

I wanted to know whether "borrowing a rule it was never meant to have" is
actually prevented, using a rule that carries the two dangerous knobs.

## Steps to reproduce

As the unprivileged login user (uid 1000), with a rule whose *name* is a file I
create in my own home directory:

```
cp /bin/sleep ~/skepticrt
echo '{"name": "skepticrt", "sched": "fifo", "rtprio": 10, "nice": -20}' \
  | sudo tee /etc/ananicy.d/rt.rules
sudo aranicy --reload
setsid --fork /home/arch/skepticrt 20 </dev/null >/dev/null 2>&1
p=$(pgrep -x skepticrt | head -1)
awk '{print "nice="$19" rtprio="$40" policy="$41}' /proc/$p/stat
chrt -p $p
grep -E '^Uid' /proc/$p/status
```

Only the `sudo tee` of the rule and the reload are privileged; the process that
receives the policy is unprivileged, and I never ran anything as root to start it.

## Expected

Either a `nice -20`/`sched: fifo` rule is bound to something the requesting user
does not control (the file's identity, a configured path, or a root-owned
directory), or the documentation should not claim that "borrowing a rule it was
never meant to have" is what the exe-based key prevents. The two sentences are
in tension: the exe is unforgeable, but its *name* is a string the caller picks
when it creates the file.

## Actual

The unprivileged process receives `nice -20`, `SCHED_FIFO`, rtprio 10. It needed
no capability, no `exec -a`, no argv trickery — only a file in `$HOME` with the
rule's basename. The round-1 mechanism (a bare string) now costs a `cp`; the
outcome is identical.

## Evidence

```
$ cp /bin/sleep ~/skepticrt; echo '{"name": "skepticrt", "sched": "fifo", "rtprio": 10, "nice": -20}' | sudo tee /etc/ananicy.d/rt.rules >/dev/null; sudo aranicy --reload; sleep 1; setsid --fork /home/arch/skepticrt 20 </dev/null >/dev/null 2>&1; sleep 2; p=$(pgrep -x skepticrt | head -1); echo "pid=$p comm=$(cat /proc/$p/comm)"; awk '{print "nice="$19" rtprio="$40" policy="$41}' /proc/$p/stat; chrt -p $p; grep -E '^Uid' /proc/$p/status
sent SIGHUP to aranicy (pid 1291)
pid=1387 comm=skepticrt
nice=-20 rtprio=10 policy=1
pid 1387's current scheduling policy: SCHED_FIFO
pid 1387's current scheduling priority: 10
Uid:	1000	1000	1000	1000
```

For contrast, the same test in round 1's shape — a rule borrowed through
`argv[0]` with no file of my own — now fails, which is the fix working:

```
--- A: real skeptictest, argv0=divider ---
pid=1330 comm=skeptictest nice=12 exe=/home/arch/skeptictest cmdline=[divider 20 ]
--- B: real innocent, argv0=skeptictest ---
pid=1343 comm=innocent nice=0 exe=/home/arch/innocent cmdline=[skeptictest 20 ]
```

## What I could not determine

- Whether a rule corpus in the wild contains `nice: -20` or `sched: fifo` rules
  for names that do not already exist in a root-owned directory. I wrote this
  rule myself; I did not have a corpus to test (the package ships none, and
  fetching one is outside this session). The finding is about the matching
  rule, not about a particular corpus entry.
- Whether adding the binary's real path, uid or a root-owned-path check would
  break the documented ananicy-compatibility surface (wrapper execs, symlinks);
  I did not attempt a fix.
- I did not attempt to demonstrate starvation with two FIFO spinners: on 2 vCPUs
  that risks making the box unreachable, which would invalidate the rest of the
  session. The grant is proven by `chrt -p`; the denial-of-service is inferred,
  not measured.
