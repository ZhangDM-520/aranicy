# Rules are matched on argv[0], so any unprivileged user can take a rule's priority (observed: nice -20 and SCHED_FIFO 10) on arbitrary code, or dodge its own rule

- severity: major
- category: safety
- environment: Linux 7.2.11-arch1-1, aranicy 0.3.0-1, 2 vCPU, 1958 MiB; test user uid 1000, no extra groups, no capabilities

## What I was trying to do

Check what the daemon treats as a process's identity, because the README justifies its
design with identity: "no comm prescreen — `exec -a` spoofing makes comm-only filtering
unsafe". That sentence claims argv[0]/cmdline is the trustworthy field and comm is the
spoofable one. I tested both directions: does a process escape its rule by renaming
argv[0], and does an unrelated process attract a rule by claiming its name?

## Steps to reproduce

As the unprivileged user (the wrapper's login user, uid 1000), with
`/etc/ananicy.d/t.rules` = `{"name": "skeptictest", "nice": 12}`:

```
cp /bin/sleep ~/skeptictest; cp /bin/sleep ~/innocent
bash -c 'exec -a divider /home/arch/skeptictest 15' &   # escape attempt
bash -c 'exec -a skeptictest /home/arch/innocent 15' &  # attraction attempt
for p in $(pgrep -x skeptictest) $(pgrep -x innocent); do
  echo "pid=$p comm=$(cat /proc/$p/comm) nice=$(awk '{print $19}' /proc/$p/stat) cmdline=[$(tr '\0' ' ' </proc/$p/cmdline)]"
done
```

Then with `{"name":"skepticrt","sched":"fifo","rtprio":10,"nice":-20}` loaded and
reloaded, the unprivileged user runs `exec -a skepticrt ~/innocent2 20` and checks
`/proc/<pid>/stat` fields 19 (nice), 40 (rt_priority), 41 (policy) and `chrt -p`.

## Expected

The policy should follow the program (comm / the executed file), and an unprivileged
process should not be handed `nice -20` or a realtime class by a root daemon just because
it chose a string.

## Actual

The policy follows argv[0] exactly and only. The real `skeptictest` binary escapes its
rule by being exec'd as `divider`; an unrelated `innocent` binary attracts it by being
exec'd as `skeptictest`. And because a rule can carry `nice: -20` with `sched: fifo`,
an unprivileged user obtains `nice -20`, `SCHED_FIFO`, rtprio 10 on any code they like.
The daemon runs as root with `CAP_SYS_NICE` and does this without any check.

## Evidence

```
--- case A: real file skeptictest, argv[0]=divider ---
  pid=1302 comm=skeptictest nice=0 cmdline=[divider 15 ]
--- case B: real file innocent, argv[0]=skeptictest ---
  pid=1309 comm=innocent nice=12 cmdline=[skeptictest 15 ]
--- case D: real file innocent, argv[0]=/usr/bin/skeptictest (path) ---
  pid=1316 comm=innocent nice=12 cmdline=[/usr/bin/skeptictest 15 ]
--- case E: real file skeptictest, plain ---
  pid=1327 comm=skeptictest nice=12 cmdline=[/home/arch/skeptictest 15 ]
```

Realtime grant to uid 1000 (`/etc/ananicy.d/rt.rules` = `{"name": "skepticrt", "sched": "fifo", "rtprio": 10, "nice": -20}`):

```
$ aranicy dump rules
{"match": "exact", "name": "skepticrt", "nice": -20, "rtprio": 10, "sched": "fifo"}
$ bash -c 'exec -a skepticrt /home/arch/innocent2 20' &
pid=1453 comm=innocent2
Uid:	1000	1000	1000	1000
pol=1 nice=-20 rtprio=10
pid 1453's current scheduling policy: SCHED_FIFO
pid 1453's current scheduling priority: 10
```

`match-test --help` states the identity plainly: `Process name as the daemon derives it
(basename(argv[0]))`. The bias engine disagrees and keys on comm instead: a python3 binary
started as `exec -a skepticcpu python3 ...` got no `skepticcpu` bias entry at all in 190 s,
while `cp /usr/bin/python3 ~/skepticcpu` (distinct comm) was classified within 200 s.

## What I could not determine

- Whether this is a regression or the intended ananicy model. ananicy-cpp is not installed
  here and I did not install it (not in the payload, and the brief forbids third-party
  installs), so I cannot compare. What I can defend is that the shipped README *argues*
  from the opposite premise, so read literally the documentation states the reason for
  choosing the more spoofable field.
- Whether a `sched: fifo` rule is reachable from a realistic rule corpus; I wrote the rule
  myself. The evasion half of the finding (case A) needs only the benign rule I wrote.
- I did not attempt to demonstrate starvation with two FIFO spinners: on 2 vCPUs that
  risks making the box unreachable, which would invalidate the rest of the session. The
  capability is proven by `chrt -p`; the DoS is inferred, not measured.
