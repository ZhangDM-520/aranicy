# Load makes the daemon raise its own nice from the unit's −5 to +3, and `systemctl` keeps reporting −5

- severity: major
- category: safety
- environment: Arch Linux cloud image, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 +
  aranicy-collector 0.3.0-1 (build 2026-11-11 08:59 UTC), 2 vCPU / 1958 MiB / no display

## What I was trying to do

Run the package as delivered and rely on the priority the unit file asks for.
`aranicy.service` sets `Nice=-5` (and the collector `Nice=-10`); I read that as a
deliberate decision, the same one ananicy-cpp made, so that the thing applying
policy does not get starved by the load it is supposed to be managing. I wanted
to confirm `/proc` agrees with the unit while a build saturates the machine,
because a policy daemon that is itself demoted under load applies rules late.

## Steps to reproduce

```sh
# a 150 s fork/exec storm; any sustained exec activity does it
cat > ~/storm.py <<'PY'
import os, time
dl = time.monotonic() + 150
while time.monotonic() < dl:
    pid = os.posix_spawn("/bin/true", ["/bin/true"], {"PATH": "/usr/bin:/bin"})
    os.waitpid(pid, 0)
PY

sudo pacman -U --noconfirm ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst \
                            ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst
sudo systemctl start aranicy; sleep 4
D=$(pgrep -f '^/usr/bin/aranicy start')
awk '{print "nice=" $19}' /proc/$D/stat          # -5, as the unit asks
nohup setsid python3 ~/storm.py &
for i in 1 2 3 4 5; do
    sleep 30
    echo "t+$((i*30))s nice=$(awk '{print $19}' /proc/$D/stat) unit=$(systemctl show -p Nice --value aranicy)"
done
sudo journalctl -u aranicy --no-pager -o cat | grep 'name=aranicy '
```

## Expected

The daemon keeps the priority its unit assigns it, or, if the learned engine is
allowed to move the daemon, the unit does not claim a nice it will not keep.
Either way `/proc/<pid>/stat` and `systemctl show -p Nice` should tell the same
story to someone auditing the machine.

## Actual

The daemon's own comm (`aranicy`) is not in `bias_protect`, so the learned
engine classifies it, walks its bias 0 → 3 (three `bias changed` lines 30 s
apart, `class=Bulk`), and then applies it — overwriting the unit's −5 with +3.
Eight nice levels, in the direction that makes it lose the CPU exactly during
the load it exists for.

`systemctl show -p Nice aranicy` keeps printing −5, because systemd records the
value it set at start; nothing re-reads `/proc`. So the two answers a user can
get disagree, and the one from systemd is the wrong one.

## Evidence

Fresh install, clean start, then a 150 s exec storm. (Full transcript in
`session-log.md`.)

```
$ systemctl show -p Nice aranicy
Nice=-5
$ D=396493; awk '{print "nice=" $19}' /proc/$D/stat
nice=-5
$ for i in 1 2 3 4 5; do sleep 30; echo "t+$((i*30))s nice=$(awk '{print $19}' /proc/$D/stat) unit=$(systemctl show -p Nice --value aranicy)"; done
t+30s nice=-5 unit=-5
t+60s nice=3 unit=-5
t+90s nice=3 unit=-5
t+120s nice=3 unit=-5
t+150s nice=3 unit=-5
```

The jump is one step here only because the history restored at start already
held `aranicy bias=3` (`INFO bias: restored history entries=67`); on a machine
that has never run the package the same growth is visible step by step:

```
$ sudo journalctl -u aranicy --no-pager -o cat | grep 'name=aranicy '
2026-11-11T09:09:56.806057Z  INFO bias changed name=aranicy bias=1 prev=0 class=Bulk cause=Evidence
2026-11-11T09:10:26.305697Z  INFO bias changed name=aranicy bias=2 prev=1 class=Bulk cause=Evidence
2026-11-11T09:10:56.306361Z  INFO bias changed name=aranicy bias=3 prev=2 class=Bulk cause=Evidence
```

Its own JSON view agrees with `/proc` and not with the unit:

```
$ aranicy dump proc | python3 -c 'import json,sys;print([e for e in json.load(sys.stdin) if e["name"]=="aranicy"])'
[{'name': 'aranicy', 'nice': 2, 'pid': 396493}, {'name': 'aranicy', 'nice': 2, 'pid': 719662}]
```

(The collector is *not* affected: its comm is `aranicy-collect` and it stays at
the unit's −10 — `pid=1168 comm=aranicy-collect nice=-10`. So the protection is
not systematic; it is simply that nobody put `aranicy` on the list.)

## What I could not determine

- Whether the intent is "the daemon is fair game like any other process". The
  shipped config's example line is about a compositor shell and an editor, and
  the unit has an explicit `Nice=-5`, so the two shipped artefacts point in
  opposite directions.
- How much latency the self-demotion actually costs. My rule-application probes
  were too coarse to separate "daemon is 8 nice levels lower" from "2 vCPU box
  is saturated"; I am reporting the state, not a measured apply delay.
- Whether release brings it back to −5 within a session. With 1 step per 300 s
  it would take ~25 min of idleness, and it will keep re-learning itself
  whenever there is load. (`cause=Relax` lines for `aranicy` do appear:
  `bias changed name=aranicy bias=2 prev=3` at 09:32:43, `bias=1 prev=2` at
  09:37:43.)
