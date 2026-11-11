# Learned bias changes the nice value of processes no rule names, and there is no config key to turn it off

- severity: minor
- category: enhancement
- environment: Linux 7.2.11-arch1-1 (Arch rolling, cloud image), aranicy 0.3.0-1 + aranicy-collector 0.3.0-1 (sched_ext state `disabled`, no `scx_*` binary present), 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Ship one identical, inspectable policy to three boxes from Ansible. I accept
that the packages ship no rule corpus and that rules are the policy. What I
need to know is whether a box can deviate from that policy on its own, and if
it can, how I disable that so "same config on every box" means "same
priorities on every box".

## Steps to reproduce

```
sudo pacman -U --noconfirm ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst \
                         ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst
sudo systemctl enable --now aranicy
aranicy dump rules                       # empty: no rule can name openssl
setsid timeout 140 openssl speed -seconds 140 -multi 2 sha256 &
sleep 3;  ps -eo pid,ni,comm --no-headers | grep openssl
sleep 75; ps -eo pid,ni,comm --no-headers | grep openssl
```

Then try to turn the feature off:

```
printf 'bias_enabled = false\nlog_applied_rule = flurble\ntotally_unknown_key = 1\n' \
  | sudo tee -a /etc/ananicy.d/ananicy.conf
sudo systemctl start aranicy; sleep 3
sudo journalctl -u aranicy -b --since "-30 seconds" -o cat | head -20
```

## Expected

Either every nice value on the box is traceable to a rule I deployed, or
there is a documented config switch that stops the daemon from changing nice
values on its own, so that a box behaves the same way months apart and across
three machines.

## Actual

With an empty rule set, `openssl` processes are placed at nice 2 within three
seconds of exec and moved to nice 3 about a minute later; the only trace is a
bias log line. The shipped config file documents the feature (`bias_group.build`)
but has no on/off key, and the key name I guessed is rejected as unknown, so
the only way to stop it is to stop the daemon.

## Evidence

```
$ aranicy dump rules
{
  "exact": [],
  "regex": []
}

$ setsid timeout 140 openssl speed -seconds 140 -multi 2 sha256 & sleep 3
$ ps -eo pid,ni,comm --no-headers | grep openssl
   1504   2 openssl
   1505   2 openssl
   1506   2 openssl

$ sleep 75; ps -eo pid,ni,comm --no-headers | grep openssl
   1504   3 openssl
   1505   3 openssl
   1506   3 openssl

$ sudo journalctl -u aranicy --since "-90 seconds" --no-pager -o cat | grep -iE "bias|openssl"
2026-11-11T08:30:36.089477Z  INFO bias changed name=openssl bias=3 prev=2 class=Bulk cause=Evidence
```

Other names picked up the same way on an idle box, none of them named by any
rule in `/etc/ananicy.d` (verbatim `bias changed` lines; `journalctl -u
aranicy -o cat | grep "bias changed"`):

```
2026-11-11T08:22:28.266092Z  INFO bias changed name=systemd-userwor bias=1 prev=0 class=Bulk cause=Evidence
2026-11-11T08:22:58.312761Z  INFO bias changed name=openssl bias=1 prev=0 class=Bulk cause=Evidence
2026-11-11T08:23:03.264926Z  INFO bias changed name=systemd-userwor bias=2 prev=1 class=Bulk cause=Evidence
2026-11-11T08:25:51.138904Z  INFO bias changed name=python3 bias=1 prev=0 class=Bulk cause=Evidence
```

Attempting to disable it:

```
$ printf 'bias_enabled = false\nlog_applied_rule = flurble\ntotally_unknown_key = 1\n' | sudo tee -a /etc/ananicy.d/ananicy.conf
$ sudo systemctl start aranicy; sleep 3
$ sudo journalctl -u aranicy -b --since "-30 seconds" -o cat | head -20
Starting aranicy - auto-nice daemon...
2026-11-11T08:27:21.496368Z  WARN config: line 39: unknown key "bias_enabled"
2026-11-11T08:27:21.496442Z  WARN config: line 40: invalid boolean "flurble"
2026-11-11T08:27:21.496448Z  WARN config: line 41: unknown key "totally_unknown_key"
2026-11-11T08:27:21.496477Z  WARN kernel does not support latency_nice (sched_setattr LATENCY_NICE); latency_nice policy will be skipped
2026-11-11T08:27:21.550928Z  INFO bias: restored history entries=118
2026-11-11T08:27:21.554013Z  INFO stats sampler live prev=degraded: starting detail=bound SchedSample via sched_samples at /run/aranicy/collector-stats.sock
Started aranicy - auto-nice daemon.
```

(The validation itself is good — unknown keys and bad values are reported with
line numbers; there is simply no key that does what I need.)

## What I could not determine

- Whether the bias would ever reach a name I must not lose, such as
  `systemd-journald`. Under a journal flood plus two busy cores
  (`seq 1 3000 | logger` in a loop, `openssl speed -multi 2`), journald was
  classified `bulk` with confidence 0.935 but its nice stayed 0 for the whole
  run, while `sshd`, `systemd-resolve` and every kernel thread I sampled also
  stayed at their original nice. So I have no reproduction of the daemon
  touching them on this box — but I also cannot tell what stops it, which is
  why this is filed as an enhancement and not as a safety bug.
- Whether a `bias_*` key exists under another name. The shipped config lists no
  such key and there is no `--help` entry; I did not try to enumerate the
  accepted key set beyond `bias_enabled`.
