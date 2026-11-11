# The learned-bias engine classified PID 1 as `Bulk`, stepped it to +2, persisted it, and re-applies it to PID 1 at every start

- severity: blocker
- category: safety
- environment: Linux 7.2.11-arch1-1, aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU, 1958 MiB, `/sys/kernel/sched_ext/state` = `disabled`

## What I was trying to do

Falsify the central claim — that decisions come from observed behaviour — by watching
what the bias engine does to processes it can never have been configured for. PID 1 is
the sharpest case: no rule mentions it, and it is the process whose priority must never
move.

## Steps to reproduce

```
sudo systemctl enable --now aranicy
echo '{"name": "systemd", "nice": 19}' | sudo tee /etc/ananicy.d/pid1.rules
sudo systemctl reload aranicy
sleep 120
sudo journalctl -u aranicy --no-pager | grep 'bias changed'
aranicy dump bias | python3 -c 'import json,sys; d=json.load(sys.stdin); print([e for e in d["state"]["entries"] if e["name"]=="systemd"])'
# then remove the rule and restart; the bias survives and is re-applied
sudo rm /etc/ananicy.d/pid1.rules
sudo systemctl stop aranicy
sudo rm -f /var/lib/aranicy/bias-state      # note: the stop flushed it again, see below
sudo renice -n 0 -p 1
sudo systemctl start aranicy
# poll /proc/1/stat for 60-70 s
```

## Expected

Either PID 1 is out of scope for the bias engine, or a bias step for PID 1 is refused
and logged. Nothing about `systemd` is bulk work in any sense I recognise.

## Actual

The bias engine sampled PID 1, labelled it `Bulk`, and stepped its bias 0 -> 1 -> 2,
logging `cause=Evidence`. The step is applied to PID 1's nice. The entry is written to
`/var/lib/aranicy/bias-state` and survives a restart: after a restart with the rule gone,
PID 1 climbs 0 -> 2 again at the first pass (~60 s), with no rule loaded at all.

## Evidence

```
$ sudo journalctl -u aranicy --no-pager -n 30 | tail -20
Nov 11 08:21:57 su-skeptic-r1 aranicy[1627]: 2026-11-11T08:21:57.139584Z  INFO bias changed name=systemd bias=2 prev=1 class=Bulk cause=Evidence
```

```
$ aranicy dump bias | python3 -c ...
systemd entry: {'bias': 2, 'class': 'bulk', 'confidence': 0.9331872023938622, 'frozen': False, 'name': 'systemd', 'sample_count_ewma': 8.344030016398962, 'score_ema': 699.3392319288143}
total entries 75
non-zero bias entries 1
systemd 2 bulk
```

Re-application after restart (bias file removed, rule file removed, PID 1 reset to 0):

```
$ sudo systemctl restart aranicy; echo "pid1=$(awk '{print $19}' /proc/1/stat)"
pid1=0
t=5s pid1=0 ... t=55s pid1=0
t=60s pid1=2
t=70s pid1=2
...
t=150s pid1=2
```
(`sudo journalctl -u aranicy -n 4` at that start shows `bias: restored history entries=76`,
i.e. the `rm` before `restart` was undone by the stop-time flush.)

A control run with the state file removed **while the daemon was stopped** showed no such
step at all in 300 s (`pid1=0` at every 10 s sample, no `bias changed` lines), so the
value in the first run came from restored history, not from fresh learning in that run.

## What I could not determine

- What produced the original `systemd` samples and why they scored 699 (Bulk). I could not
  reproduce the *first* step from a wiped state on a quiet box in 300 s; I can only show
  that the step happened (`cause=Evidence`), that it was applied, and that it persists and
  is re-applied at every start. In the run where it happened I had just loaded rules and
  was generating exec load, so I cannot exclude that the samples came from PID 1's normal
  service activity being picked up by the collector.
- Whether the bias is applied to PID 1 on a host with no sched_ext scheduler by design or
  by accident; the README says the sampler binds a `sched_ext` stats surface, and this box
  has none (`disabled`), yet samples and bias steps were produced. See issue dana-6.
