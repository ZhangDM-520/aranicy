# The learned classifier gives every workload I ran the same verdict — `Bulk`, bias +3 — including an interactive shell and a CPU spinner two orders of magnitude apart in score

- severity: major
- category: behaviour
- environment: Linux 7.2.11-arch1-1, aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU, 1958 MiB, sched_ext `disabled`, no rules loaded during this test

## What I was trying to do

Attack the central claim directly: "bulk work nudged nicer, latency-sensitive work less
nice". If that is true, a CPU-bound spinner and a process that sleeps 10 ms in a loop
should land in different classes, or at least not both at the top of the nice ramp.

## Steps to reproduce

```
sudo rm -f /etc/ananicy.d/*.rules && sudo systemctl reload aranicy
cp /usr/bin/python3 ~/skepticcpu; cp /usr/bin/python3 ~/skepticlat
printf 'while True:\n    pass\n' > ~/spin.py          # CPU-bound
printf 'import time\nwhile True:\n    time.sleep(0.01)\n' > ~/lat.py   # latency-shaped
setsid --fork ~/skepticcpu ~/spin.py </dev/null >/dev/null 2>&1
setsid --fork ~/skepticlat ~/lat.py  </dev/null >/dev/null 2>&1
sleep 200
aranicy dump bias
ps -eo pid,ni,comm,args --no-headers | grep skeptic
```

## Expected

Different classes or different bias for the spinner and the sleeper; and an interactive
`bash` — the process a human waits on — should not be pushed further into the background
than a compiler.

## Actual

After 200 s: the spinner is `Bulk` with bias +3 (score 3.9); the interactive shell is
`Bulk` with bias +3 (score 1011); the daemon itself is `Bulk` with bias +3 (score 1177);
`python3` is `Bulk` +3. Scores span 3.9 to 1177 — nearly three orders of magnitude — with
an identical verdict. The 10 ms-sleeper never even got an entry in 200 s.

Because bias is applied to the shell, everything the shell starts inherits nice 3 —
including, on this box, my test binaries (`ps` shows `ni=3` on both, which is inheritance
from the niced shell, not a per-process decision).

## Evidence

```
aranicy bias 3 bulk conf 0.59 score 1177.0 samples 25.5
bash bias 3 bulk conf 0.25 score 1011.4 samples 3.0
python3 bias 3 bulk conf 0.25 score 141.5 samples 3.5
skepticcpu bias 3 bulk conf 0.88 score 3.9 samples 112.0
skeptictest bias 0 bulk conf 0.21 score 1015.8 samples 1.0
=== live ===
 125043   3 skepticcpu      /home/arch/skepticcpu /home/arch/spin.py
 125045   3 skepticlat      /home/arch/skepticlat /home/arch/lat.py
```

Escalation to the clamp, one step per ~30 s, from the journal:

```
Nov 11 08:39:59 INFO bias changed name=aranicy bias=2 prev=1 class=Bulk cause=Evidence
Nov 11 08:40:19 INFO bias changed name=bash bias=1 prev=0 class=Bulk cause=Evidence
Nov 11 08:40:29 INFO bias changed name=aranicy bias=3 prev=2 class=Bulk cause=Evidence
Nov 11 08:40:49 INFO bias changed name=bash bias=2 prev=1 class=Bulk cause=Evidence
Nov 11 08:41:19 INFO bias changed name=bash bias=3 prev=2 class=Bulk cause=Evidence
```

My own login shell, at the end of that sequence:

```
self=124880 nice=3 comm=bash
parent=124879 nice=0 comm=sshd-session
```

`bash` was exec'd into nice 0 (inherited) and put at 3 by the daemon while idle between
commands.

## What I could not determine

- The threshold the classifier uses for `Bulk` on this host, and whether the scores I see
  are comparable across names (per-second-of-own-activity vs per-window). I did not
  invert the classifier; I only observe that two orders of magnitude of score produce the
  same label and the same +3.
- Whether a *compiler* would be classified differently from the spinner: gcc exists on the
  box but I did not run a build (time budget), so I cannot claim the classifier fails on
  the workload the feature was designed for — only that it does not separate the ones I
  ran, and that it treats an interactive shell as bulk.
- Whether the sleeper's absence of an entry is sampling (it burns almost no CPU so it may
  not appear in the sample window) or a filter. Either way I could not get it classified.
