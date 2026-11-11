# A fresh install runs with zero rules and says nothing about it, in the journal or in `systemctl status`

- severity: major
- category: behaviour
- environment: Linux 7.2.11-arch1-1 (Arch rolling, cloud image), aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Install the packages the way an Ansible task would (`pacman -U` then
`systemctl enable --now aranicy`), then read the journal to see what the
daemon decided, or to see it state that there was nothing to decide. My rule
corpus is deployed separately, so I need the unit to tell me when it has zero
rules — a corpus that did not land is exactly the failure I must not miss.

## Steps to reproduce

```
sudo pacman -U --noconfirm ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst \
                         ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst
ls /etc/ananicy.d/                 # only ananicy.conf; the package ships no rules
sudo systemctl enable --now aranicy
sleep 4
sudo journalctl -u aranicy -u aranicy-collector --since "-8 seconds" --no-pager -o cat
aranicy dump rules
```

## Expected

At startup the daemon states what it loaded — at minimum the number of rules
and their directory — and warns when that number is zero, so that
`systemctl status aranicy` and `journalctl -u aranicy` distinguish "running
with my policy" from "running with nothing".

## Actual

The unit is `active (running)`, nothing in the journal mentions rules at all,
and `dump rules` shows an empty rule set. Applying a policy is not happening,
and the only evidence of that is a separate command I have to know about.
A `systemctl reload` *does* log a rule count (`reloaded 4 rules`), so the
information exists at startup but is not printed there.

## Evidence

```
$ ls /etc/ananicy.d/
ananicy.conf

$ systemctl is-active aranicy aranicy-collector
active
active

$ sudo journalctl -u aranicy -u aranicy-collector --since "-8 seconds" --no-pager -o cat
# (the three stop records of the previous instance are elided)
Started aranicy collector - scheduler-agnostic BPF task statistics.
Starting aranicy - auto-nice daemon...
2026-11-11T08:29:36.035423Z  WARN kernel does not support latency_nice (sched_setattr LATENCY_NICE); latency_nice policy will be skipped
2026-11-11T08:29:36.049925Z  INFO aranicy_collectord::events: collector serving exec events socket=/run/aranicy/collector-events.sock
2026-11-11T08:29:36.049926Z  INFO aranicy_collectord: collector serving sched_samples + exec events socket=/run/aranicy/collector-stats.sock events_socket=/run/aranicy/collector-events.sock cadence_ms=500
2026-11-11T08:29:36.086800Z  INFO bias: restored history entries=119
Started aranicy - auto-nice daemon.
2026-11-11T08:29:36.087673Z  INFO stats sampler live prev=degraded: starting detail=bound SchedSample via sched_samples at /run/aranicy/collector-stats.sock
2026-11-11T08:29:36.090271Z  INFO aranicy_collectord::events: exec-event client connected

$ aranicy dump rules
{
  "exact": [],
  "regex": []
}

$ setsid sleep 402 & sleep 2; pid=$(pgrep -nx sleep); awk '{print "nice=" $19}' /proc/$pid/stat
nice=0
```

The same startup is equally silent when rules *are* present — here after a
restart with four rules loaded:

```
$ sudo systemctl restart aranicy; sleep 3
$ sudo journalctl -u aranicy --no-pager -o cat --since "-40 seconds"
Starting aranicy - auto-nice daemon...
2026-11-11T08:21:13.392666Z  WARN kernel does not support latency_nice (sched_setattr LATENCY_NICE); latency_nice policy will be skipped
2026-11-11T08:21:13.393875Z  INFO bias: restored history entries=63
Started aranicy - auto-nice daemon.
2026-11-11T08:21:13.394989Z  INFO stats sampler live prev=degraded: starting detail=bound SchedSample via sched_samples at /run/aranicy/collector-stats.sock
2026-11-11T08:21:13.401065Z  INFO applied rule rule="sleep" pid=2243 bias=0 route="exec"

$ aranicy dump rules    # four rules present
{ "exact": [ {"ioclass": "best-effort", "ionice": 7, "match": "exact", "name": "ffmpeg", "nice": 10}, ... ] }

# whereas reload does report it:
2026-11-11T08:21:56.444842Z  INFO reloaded 4 rules; bias reconfigured
```

For completeness: a rules directory that does not exist *is* reported
(`Error: cannot load config/rules: No such file or directory (os error 2)`,
exit 1), so only the present-but-empty case is silent.

## What I could not determine

- Whether a per-apply log line is expected at `loglevel = info`. With the
  shipped default `log_applied_rule = false` the daemon logs nothing when it
  changes a nice value; I confirmed the knob works by setting it to `true`
  (`INFO applied rule rule="sleep" pid=2117 bias=0 route="exec"`). I do not
  know whether the silence I am reporting is meant to be covered by that knob
  or by a startup summary.
- Whether the packages are intended to ship a rule corpus at all. The README
  states rule data is GPL and never bundled, which I accept — that is exactly
  why the daemon must say out loud when it has none.
