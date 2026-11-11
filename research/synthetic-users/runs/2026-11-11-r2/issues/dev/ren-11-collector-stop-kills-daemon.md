# `systemctl stop aranicy-collector` stops the daemon too, so the documented "degraded sampler, keeps retrying" state cannot be reached

- severity: minor
- category: docs
- environment: Arch Linux cloud image, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 +
  aranicy-collector 0.3.0-1 (build 2026-11-11 08:59 UTC), 2 vCPU / 1958 MiB / no display

## What I was trying to do

Work out what happens when the measurement half of the pair goes away, because
the exec-event stream and the stats stream come from the collector: if the
collector dies, does the daemon keep applying rules, and how do I tell? The
collector's own README answers this directly, and I wanted to reproduce the two
states it describes — "degraded" and "recovered" — so I could recognise them on
a real machine.

## Steps to reproduce

```sh
systemctl is-active aranicy aranicy-collector
pgrep -f '^/usr/bin/aranicy start'; pgrep -x aranicy-collect
sudo systemctl stop aranicy-collector
sleep 2
systemctl is-active aranicy aranicy-collector
pgrep -f '^/usr/bin/aranicy start' || echo none
```

## Expected

Per `/usr/share/doc/aranicy-collector/README.md`, section **Stopping it**:

> `systemctl stop aranicy-collector` detaches the programs and closes the
> sockets; the daemon reports a degraded sampler and keeps retrying with a
> backoff. `systemctl restart aranicy-collector` is safe at any time.

So: the collector stops, the daemon stays up, logs a degraded sampler and backs
off until the collector returns.

## Actual

Stopping the collector stops the daemon as well, and restarting the collector
restarts the daemon as well. The reason is visible in the daemon's own unit:
`Requires=aranicy-collector.service` plus `After=`, so systemd propagates a stop
to `aranicy` and a restart keeps dependents consistent by restarting them. There
is no state in which the daemon runs against a missing collector, so the
degraded path is only reachable by killing the collector process out of band
(which I did not do; systemd restarts it in 5 s).

## Evidence

```
$ echo "before daemon=$(pgrep -f '^/usr/bin/aranicy start') collector=$(pgrep -x aranicy-collect)"
before daemon=396493 collector=396492
$ sudo systemctl restart aranicy-collector; sleep 6
$ echo "after daemon=$(pgrep -f '^/usr/bin/aranicy start') collector=$(pgrep -x aranicy-collect)"
after daemon=719785 collector=719782
$ sudo journalctl -u aranicy --no-pager -o cat -n 6
Starting aranicy - auto-nice daemon...
2026-11-11T09:40:13.622944Z  WARN kernel does not support latency_nice (sched_setattr LATENCY_NICE); latency_nice policy will be skipped
2026-11-11T09:40:13.623784Z  INFO bias: restored history entries=68
Started aranicy - auto-nice daemon.
2026-11-11T09:40:13.624588Z  INFO stats sampler live prev=degraded: starting detail=bound SchedSample via sched_samples at /run/aranicy/collector-stats.sock
$ sudo systemctl stop aranicy-collector; sleep 2
$ systemctl is-active aranicy aranicy-collector
inactive
inactive
$ echo "after stop daemon=$(pgrep -f '^/usr/bin/aranicy start' || echo none) collector=$(pgrep -x aranicy-collect || echo none)"
after stop daemon=none collector=none
```

(The restart is otherwise harmless — history is restored, 68 entries — so I am
reporting the documentation, not data loss.)

## What I could not determine

- Whether the degraded path works when the collector is killed rather than
  stopped (`kill -9`): `Restart=always`/`RestartSec=5` in
  `aranicy-collector.service` would bring it back before the daemon's
  `collector_wait_ms` budget matters, so I did not chase it.
- Whether "the daemon keeps retrying" is even meaningful under `Requires=`
  (systemd will not start `aranicy` without the collector, so the only retry
  that matters is the collector's own reconnect loop).
- Whether the sentence is meant to describe the `kill` case and the unit
  dependency was added later. Both are shipped as-is, which is why a user
  following the README sees something else.
