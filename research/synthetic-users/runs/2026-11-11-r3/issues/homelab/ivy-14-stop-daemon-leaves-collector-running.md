# `systemctl stop aranicy` leaves `aranicy-collectord` running, sampling, with its BPF programs attached

- severity: minor
- category: packaging
- environment: Linux 7.2.11-arch1-1 (Arch rolling, cloud image), aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Turn the policy off for an hour without uninstalling it: `systemctl stop
aranicy`, do the work, `systemctl start aranicy`. I expect the whole thing to
go quiet, because that is what stopping "the service" means for a daemon whose
statistics collector only exists to feed it.

## Steps to reproduce

```
sudo pacman -U --noconfirm ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst \
                         ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst
sudo systemctl enable --now aranicy
sudo systemctl stop aranicy
sleep 2
systemctl is-active aranicy aranicy-collector
sudo journalctl -u aranicy -u aranicy-collector --since "-10 seconds" --no-pager -o cat
```

## Expected

Both units inactive (the collector has nothing to feed), or a documented
statement that the collector is independent and keeps running. The
`aranicy.service` unit declares `Requires=`/`After=aranicy-collector.service`,
so the pair is already one logical service from my side.

## Actual

The collector stays active; only the daemon stops. The collector notices the
event-socket client leaving and keeps running:

```
$ sudo systemctl stop aranicy
$ sleep 2
$ systemctl is-active aranicy aranicy-collector
inactive
active
$ systemctl is-enabled aranicy aranicy-collector
enabled
disabled
$ sudo journalctl -u aranicy -u aranicy-collector --since "-10 seconds" --no-pager -o cat
Stopping aranicy - auto-nice daemon...
aranicy.service: Deactivated successfully.
Stopped aranicy - auto-nice daemon.
2026-11-11T10:31:42.565074Z  INFO aranicy_collectord::events: exec-event client disconnected error=Broken pipe (os error 32) dropped=0
```

The reverse direction works (stopping the collector stops the daemon, via
`Requires=` in `aranicy.service`):

```
$ sudo systemctl stop aranicy-collector
$ systemctl is-active aranicy aranicy-collector
inactive
inactive
Stopping aranicy - auto-nice daemon...
aranicy.service: Deactivated successfully.
Stopped aranicy - auto-nice daemon.
Stopping aranicy collector - scheduler-agnostic BPF task statistics...
aranicy-collector.service: Deactivated successfully.
Stopped aranicy collector - scheduler-agnostic BPF task statistics.
```

So the relationship is one-way, which is surprising for a pair that is
documented and packaged as one thing (`aranicy` depends on
`aranicy-collector=0.3.0`).

## Evidence

`raw/stop-asymmetry.txt` (the whole run, `bash -x`):

```
+ sudo systemctl stop aranicy
+ sleep 2
+ systemctl is-active aranicy aranicy-collector
inactive
active
+ systemctl is-enabled aranicy aranicy-collector
enabled
disabled
+ sudo journalctl -u aranicy -u aranicy-collector --since '-10 seconds' --no-pager -o cat
Stopping aranicy - auto-nice daemon...
aranicy.service: Deactivated successfully.
Stopped aranicy - auto-nice daemon.
2026-11-11T10:31:42.565074Z  INFO aranicy_collectord::events: exec-event client disconnected error=Broken pipe (os error 32) dropped=0
```

The unit files make the one-way nature explicit (`Requires=` is on
`aranicy.service`; the collector has no `PartOf=`/`BindsTo=` back to the
daemon):

```
$ grep -n "Requires\|After\|PartOf\|BindsTo" /usr/lib/systemd/system/aranicy.service /usr/lib/systemd/system/aranicy-collector.service
/usr/lib/systemd/system/aranicy.service:4:After=local-fs.target aranicy-collector.service
/usr/lib/systemd/system/aranicy.service:5:Requires=aranicy-collector.service
/usr/lib/systemd/system/aranicy.service:31:# Lifecycle: systemd owns the collector; aranicy summons it via Requires=/
/usr/lib/systemd/system/aranicy.service:32:# After= above and waits (collector_wait_ms) for its sockets at startup.
/usr/lib/systemd/system/aranicy-collector.service:4:After=local-fs.target
```

## What I could not determine

- The cost of the idle collector (it samples at `cadence_ms=500` and holds the
  tracepoints). I did not measure it: `pidstat`/`bpftool` are not installed
  and I did not want to install tools on a box I am auditing. My complaint is
  that it runs at all after I stopped "the service", not a measured CPU figure.
- Whether a `PartOf=aranicy.service` on the collector would be the intended
  fix, or whether the daemon should own the collector's lifetime some other
  way.
