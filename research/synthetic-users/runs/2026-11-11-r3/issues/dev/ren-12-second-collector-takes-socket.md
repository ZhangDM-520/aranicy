# A second `aranicy-collectord run` starts without complaint, takes over the socket path, and leaves the running pair unable to restart the daemon

- severity: major
- category: safety
- environment: Arch Linux cloud image, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 +
  aranicy-collector 0.3.0-1 (build 2026-11-11 09:52 UTC), 2 vCPU / 1958 MiB / no display

## What I was trying to do

Debug a machine where I suspected the collector, without stopping the service:
run the collector binary by hand for a few seconds and see what it says. The
README and `aranicy-collectord --help` both present `run` as the normal way to
serve, and `probe` as the diagnostic, but a user who runs `run` gets no warning
that a collector is already serving.

I expected one of: an `EADDRINUSE` refusal, a refusal to run while the unit is
active, or a message that another collector owns `/run/aranicy`. What I got is
a second process claiming the same paths, and a healthy pair that silently
loses its listener when the second one exits.

## Steps to reproduce

```sh
sudo pacman -U --noconfirm ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst \
                            ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst
sudo systemctl enable --now aranicy; sleep 6
systemctl is-active aranicy aranicy-collector           # active / active
sudo stat -c "%n inode=%i" /run/aranicy/collector-stats.sock
sudo timeout 8 /usr/bin/aranicy-collectord run          # the second instance
sudo stat -c "%n inode=%i" /run/aranicy/collector-stats.sock
sudo python3 -c 'import socket
for p in ("/run/aranicy/collector-stats.sock","/run/aranicy/collector-events.sock"):
    s=socket.socket(socket.AF_UNIX); s.settimeout(2)
    try: s.connect(p); print(p,"connected")
    except Exception as e: print(p,"->",type(e).__name__,e)'
sudo kill -9 $(pgrep -f "^/usr/bin/aranicy start")      # the next daemon restart
sleep 20; systemctl is-active aranicy; pgrep -f "^/usr/bin/aranicy start" || echo none
```

## Expected

The second instance refuses to start (address in use, or "another collector
owns this socket"), or it takes the path over *and* the running pair keeps
working. Either way, the daemon's own `Restart=always` must be able to bring
the daemon back, and if it cannot, the error must not tell me to check a
service that is running.

## Actual

1. The second instance starts, logs the same "collector serving …" lines as the
   real one, and runs until I kill it. No warning, no non-zero exit.
2. While it lives it owns the socket path (`inode` changed), so a client
   connecting to `/run/aranicy/collector-stats.sock` reaches *it*.
3. When it exits, both paths exist but refuse connections — they are dead
   socket files. The collector that systemd still reports as `active` is no
   longer reachable through them.
4. The daemon that was already connected keeps applying rules (its connection
   is to the old inode), so nothing looks wrong until it restarts.
5. On the next daemon restart (I used `kill -9`, i.e. exactly what a build box
   does at OOM or a `systemd-oomd` kill), the daemon exits with

   `Error: no collector at /run/aranicy/collector-events.sock after 5s: Connection refused (os error 111) (is aranicy-collector.service running?)`

   `systemctl is-active aranicy-collector` says `active` at that moment.
6. `Restart=always` retries five times in 60 s (`StartLimitBurst=5`), every
   attempt fails the same way, and the unit is then left in `activating`/
   failed with no daemon. Only `systemctl restart aranicy-collector` recovers
   the pair; `systemctl restart aranicy` alone keeps failing.

## Evidence

Healthy pair, socket inode before the second instance:

```
$ systemctl is-active aranicy aranicy-collector
active
active
$ sudo stat -c "%n inode=%i mtime=%y" /run/aranicy/collector-stats.sock /run/aranicy/collector-events.sock
/run/aranicy/collector-stats.sock inode=2117 mtime=2026-11-11 10:22:30.264485588 +0000
/run/aranicy/collector-events.sock inode=2116 mtime=2026-11-11 10:22:30.264485588 +0000
```

Second instance, allowed to run for 8 s:

```
$ sudo timeout 8 /usr/bin/aranicy-collectord run; echo "second_exit=$?"
2026-11-11T10:23:40.986726Z  INFO aranicy_collectord: collector serving sched_samples + exec events socket=/run/aranicy/collector-stats.sock events_socket=/run/aranicy/collector-events.sock cadence_ms=500
2026-11-11T10:23:40.986784Z  INFO aranicy_collectord::events: collector serving exec events socket=/run/aranicy/collector-events.sock
second_exit=124
$ sudo stat -c "%n inode=%i" /run/aranicy/collector-stats.sock
/run/aranicy/collector-stats.sock inode=2149
```

After it exits, nothing can connect, while the service is still `active`:

```
$ sudo python3 - <<PY
import socket
for p in ("/run/aranicy/collector-stats.sock","/run/aranicy/collector-events.sock"):
    try:
        s=socket.socket(socket.AF_UNIX); s.settimeout(2); s.connect(p); print(p,"connected"); s.close()
    except Exception as e: print(p,"->",type(e).__name__,e)
PY
/run/aranicy/collector-stats.sock -> ConnectionRefusedError [Errno 111] Connection refused
/run/aranicy/collector-events.sock -> ConnectionRefusedError [Errno 111] Connection refused
$ systemctl is-active aranicy-collector
active
$ pgrep -x aranicy-collect
1178
```

The next daemon restart cannot recover:

```
$ sudo kill -9 $(pgrep -f "^/usr/bin/aranicy start"); sleep 20
$ systemctl is-active aranicy; pgrep -f "^/usr/bin/aranicy start" || echo none
activating
none
$ sudo journalctl -u aranicy -b --no-pager -o cat | tail -4
aranicy.service: Scheduled restart job, restart counter is at 4.
Starting aranicy - auto-nice daemon...
Error: no collector at /run/aranicy/collector-events.sock after 5s: Connection refused (os error 111) (is aranicy-collector.service running?)
aranicy.service: Main process exited, code=exited, status=1/FAILURE
```

Recovery, for completeness — the daemon alone cannot do it:

```
$ sudo systemctl restart aranicy; sleep 8
Job for aranicy.service failed because the control process exited with error code.
$ systemctl is-active aranicy
activating
$ sudo systemctl restart aranicy-collector; sleep 8
$ systemctl is-active aranicy aranicy-collector
active
active
$ ( ~/renapp 15 >/dev/null 2>&1 & ); sleep 2; ps -eo pid,comm,nice | grep -w renapp | grep -v grep
   1207 renapp            7
```

Contrast: `kill -9` of the daemon on a *clean* pair recovers by itself in 10 s —

```
$ sudo kill -9 $(pgrep -f "^/usr/bin/aranicy start")
t+10s daemon=none col=1178 state=activating
t+15s daemon=1243 col=1178 state=active
$ sudo journalctl -u aranicy-collector -b --no-pager -o cat | tail -2
2026-11-11T10:22:48.783364Z  INFO aranicy_collectord::events: exec-event client disconnected error=Broken pipe (os error 32) dropped=0
2026-11-11T10:22:58.835300Z  INFO aranicy_collectord::events: exec-event client connected
```

so the damage is caused by the second instance, not by `kill -9`.

The documented diagnostic is *not* affected — `probe` while the pair is healthy:

```
$ sudo timeout 30 /usr/bin/aranicy-collectord probe 2>&1 | head -3
--- 0 rows
--- 8 rows
pid=1391    comm=renbulk          prio=23  wait=0      wake=0      rt=331351783    demand=1017  deliv=1017
$ systemctl is-active aranicy aranicy-collector
active
active
```

## What I could not determine

- Whether the second instance binds a fresh socket and unlinks the old path, or
  whether it only replaces the file. I can see the path's inode change and the
  connections fail, but I did not strace it, and I am not guessing at the
  mechanism.
- Whether a *third* copy of the same mistake would have restored service (I did
  not try; the machine was already broken and I needed it).
- Whether the daemon could retry instead of exiting when the socket refuses a
  connection. Per the shipped docs a missing collector is "a loud failure, never
  silent rule-less operation", which is what it does — my complaint is that the
  loud failure points at a service that is running, and that the unit then dies
  permanently instead of staying in the documented "degraded, keeps retrying"
  state.
