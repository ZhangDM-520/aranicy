# A rule named `systemd` is applied to PID 1 by the periodic pass, silently, 60 s after it is loaded

- severity: blocker
- category: safety
- environment: Linux 7.2.11-arch1-1 (Arch rolling), aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB, scx_lavd not running (`/sys/kernel/sched_ext/state` = `disabled`)

## What I was trying to do

Establish the blast radius of the rule engine: with rules loaded, does it ever write to
the scheduling priority of PID 1? The shipped config says the periodic pass is
"Delta-based: only processes whose policy drifted are touched, never a full re-apply
scan", which I read as "it only touches processes it has applied a policy to".

## Steps to reproduce

```
sudo pacman -U --noconfirm ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst
sudo systemctl enable --now aranicy
echo '{"name": "systemd", "nice": 19}' | sudo tee /etc/ananicy.d/pid1.rules
sudo systemctl reload aranicy
# attribution: prove nothing else does it
sudo systemctl stop aranicy aranicy-collector
sudo renice -n 0 -p 1
sleep 70        # PID 1 stays at 0 with aranicy stopped
sudo systemctl start aranicy
# poll for 70 s
awk '{print $19}' /proc/1/stat
```

## Expected

PID 1 is never reprioritised (my acceptance rule for a production box: any write to
PID 1 is a blocker however rare), or at minimum the tool refuses the rule and says so.
PID 1 has not exec'd since the daemon started, so no exec-time policy can exist for it.

## Actual

At the first periodic pass after start (~60 s) PID 1's nice changes 0 -> 19. No log line,
no warning, no refusal. With the daemon stopped the value stays 0 for the full 70 s, so
the writer is aranicy. `dump proc` shows the daemon resolves PID 1 as `systemd`
(cmdline[0] is `/usr/lib/systemd/systemd`), so a rule with that name is applied to PID 1
as if it were an ordinary process.

## Evidence

```
$ sudo systemctl stop aranicy aranicy-collector; sudo renice -n 0 -p 1 >/dev/null 2>&1; echo "stopped; pid1=$(awk '{print $19}' /proc/1/stat)"
stopped; pid1=0
$ sleep 70; echo "after 70s with aranicy STOPPED: pid1=$(awk '{print $19}' /proc/1/stat)"
after 70s with aranicy STOPPED: pid1=0
$ sudo systemctl start aranicy; for i in $(seq 1 14); do sleep 5; echo "t=$((i*5))s pid1=$(awk '{print $19}' /proc/1/stat)"; done
t=5s pid1=0
t=10s pid1=0
...
t=55s pid1=0
t=60s pid1=19
t=65s pid1=19
t=70s pid1=19
```

Repeated earlier with the same rule file loaded and a plain reload (08:15-08:22 window):

```
$ for q in 1 $(pgrep -x sshd); do echo "pid=$q comm=$(cat /proc/$q/comm) nice=$(awk '{print $19}' /proc/$q/stat)"; done
pid=1 comm=systemd nice=19
pid=743 comm=sshd nice=0
```
(`/etc/ananicy.d/blast.rules` held `{"name": "systemd", "nice": 19}` at the time.)

Journal at reload — the rule is loaded and accepted, and nothing at all is logged when
it is applied to PID 1:

```
$ sudo journalctl -u aranicy --no-pager -n 6
... reloaded 6 rules; bias reconfigured
```

## What I could not determine

- Whether the pass walks all of /proc each `check_freq` or only a cached set; I only know
  it reached a process that never exec'd under the daemon. I could not strace the daemon
  (`strace` is not installed on this box and the brief forbids installing packages), so I
  cannot show the syscall itself, only the before/after `/proc/1/stat`.
- Whether kernel threads are reachable the same way. I tried `{"name":"0","nice":19}`
  (the name `dump proc` reports for `ksoftirqd/0`) and `{"name":"ksoftirqd/0","nice":19}`
  and saw no change on any kthread over 70 s — kernel threads have an empty cmdline, so
  they appear to be unreachable. PID 1 is not.
- sshd was never touched (`-D` argv rewriting makes its derived name `sshd: /usr/bin/sshd -D ...`),
  which I read as luck of name derivation, not protection.
