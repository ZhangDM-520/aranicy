# There is no per-process status: `dump proc` gives name/pid/nice only, the only "why" is an off-by-default log line, and `dump bias` is a file that can be a minute stale

- severity: minor
- category: behaviour
- environment: Arch Linux rolling, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Confirm, for one specific process, that the thing I asked for is what it got — and when
it is not, find out *why* without reading source. Concretely: my game showed nice −2 and
my rule says −5, and I wanted the daemon to tell me what happened to it.

## Steps to reproduce

```
./machine 'aranicy dump proc'
./machine 'aranicy dump bias'
./machine 'grep -n log_applied_rule /etc/ananicy.d/ananicy.conf'
./machine 'sudo journalctl -u aranicy --no-pager | grep "applied rule" | tail -3'
```

## Expected

One command that answers "what is this pid running at, which rule decided it, and how much
of that is the learned bias". I am happy with a JSON blob; I just want the attribution
next to the number.

## Actual

`dump proc` is three fields — `name`, `nice`, `pid` — so it tells me the *result* and
nothing about the input. There is no command that maps a running pid to a rule, and no
command that shows the bias currently applied to a name (only a whole-state dump).

The single line that does explain an application is a log line that is off by default
(the config comments call it "noisy"):

```
# One line per applied rule (noisy; off by default).
log_applied_rule = false
```

With it on, it is good — `rule=... pid=... bias=... route=...` is exactly what I wanted.
But I only found it after the fact, and it is per-event, so for a process that has been
running a while I have to grep the journal and reason about the order of the lines to work
out the current state (see kaz-2: `bias=0 route="exec"` at start, then `bias=2 route="drift"`
four minutes later).

`dump bias` is not the daemon's state either — it reads `/var/lib/aranicy/bias-state` and
says so:

```
"note": "on-disk snapshot is debounced; live bias may be up to ~60 s newer"
```

so the number I get back for "why is my game at −2 right now" can be a step behind.

One more thing that cost me time: the same process can appear under two different names in
the two dumps, because `dump proc`'s `name` is read later from `/proc/<pid>/cmdline` while
the matching name was taken at exec. I chased a `sshd-session` that `dump proc` called
`sshd-session: arch [postauth]` while `dump bias` had the entry as `sshd-session`:

```
{'name': 'sshd -D [listener] 0 of 10-100 startups', 'nice': 0, 'pid': 744}
{'name': 'sshd-session: arch [postauth]', 'nice': 3, 'pid': 6917}
{'name': 'sshd-session: arch@notty', 'nice': 3, 'pid': 6920}
{'name': 'bash', 'nice': 3, 'pid': 6921}
```

## Evidence

```
$ ./machine 'aranicy dump proc 2>/dev/null | python3 -c "import json,sys; d=json.load(sys.stdin); print(json.dumps(d[0], indent=2)); print(\"keys:\", sorted(d[0].keys()))"'
{
  "name": "systemd",
  "nice": 0,
  "pid": 1
}
keys: ['name', 'nice', 'pid']
```

```
$ ./machine 'aranicy dump bias 2>/dev/null | head -5'
{
  "error": null,
  "note": "on-disk snapshot is debounced; live bias may be up to ~60 s newer",
  "path": "/var/lib/aranicy/bias-state",
  "state": {
```

```
$ ./machine 'grep -n "One line per applied rule" /etc/ananicy.d/ananicy.conf; sed -n "30,31p" /etc/ananicy.d/ananicy.conf'
30:# One line per applied rule (noisy; off by default).
# One line per applied rule (noisy; off by default).
log_applied_rule = false
```

```
$ ./machine 'sudo journalctl -u aranicy --no-pager | grep "applied rule" | tail -3'
Nov 11 08:26:36 su-gamer-r1 aranicy[7012]: 2026-11-11T08:26:36.216139Z  INFO applied rule rule="kazgame" pid=7056 bias=3 route="drift"
Nov 11 08:27:35 su-gamer-r1 aranicy[7012]: 2026-11-11T08:27:35.691180Z  INFO applied rule rule="python3" pid=7180 bias=3 route="exec"
Nov 11 08:27:47 su-gamer-r1 aranicy[7012]: 2026-11-11T08:27:47.547168Z  INFO applied rule rule="python3" pid=7199 bias=3 route="exec"
```

## What I could not determine

- Whether the drift-corrected value is ever re-reported after the initial `route="exec"`
  line short of another step — I did not find a "current state of pid N" anywhere.
- I did not test whether `dump proc` is live from `/proc` or cached; the numbers matched
  `ps` every time I checked, so I assume live.

This is a *minor* on its own — the information exists, in the journal — but it is what
made kaz-1 and kaz-2 take me half an hour to pin down instead of two minutes.
