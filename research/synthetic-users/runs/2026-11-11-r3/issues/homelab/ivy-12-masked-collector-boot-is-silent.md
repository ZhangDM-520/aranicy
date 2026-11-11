# With the collector masked, a boot leaves `aranicy` inactive (dead) with no failure state and nothing in the journal

- severity: major
- category: behaviour
- environment: Linux 7.2.11-arch1-1 (Arch rolling, cloud image), aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Run the box without the collector for one boot (the case is the same as a
collector that fails to start on an unattended upgrade: missing BPF, a masked
unit, a broken dependency). I want that to be visible — `systemctl status` says
failed and why, or the journal says the pair could not come up — because the
alternative is a box that silently applies no policy for weeks.

## Steps to reproduce

```
sudo pacman -U --noconfirm ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst \
                         ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst
sudo systemctl enable --now aranicy
sudo systemctl mask aranicy-collector
sudo systemctl reboot
# after the machine is back:
systemctl is-active aranicy aranicy-collector
systemctl is-enabled aranicy aranicy-collector
systemctl is-failed aranicy aranicy-collector
systemctl status aranicy
sudo journalctl -b -u aranicy -u aranicy-collector --no-pager -o cat
```

## Expected

Either `aranicy.service` is in `failed` with the reason (`Unit
aranicy-collector.service is masked`, or a dependency error), or the journal
has a line saying the daemon could not start and why. My monitoring already
alerts on `is-failed`; it also greps the journal for `aranicy`.

## Actual

```
$ systemctl is-active aranicy aranicy-collector
inactive
inactive
$ systemctl is-enabled aranicy aranicy-collector
enabled
masked
$ systemctl is-failed aranicy aranicy-collector
inactive
inactive
$ systemctl status aranicy
○ aranicy.service - aranicy - auto-nice daemon
     Loaded: loaded (/usr/lib/systemd/system/aranicy.service; enabled; preset: disabled)
     Active: inactive (dead)
       Docs: https://github.com/ZhangDM-520/aranicy
$ sudo journalctl -b -u aranicy -u aranicy-collector --no-pager -o cat
(no output)
```

The unit is indistinguishable from one I stopped on purpose. `is-active` is
false (so a naive check catches it), but `is-failed` is false as well and the
journal says nothing. Nothing on the box connects the state to the masked
dependency.

Starting it by hand *is* clear, so the information exists — it only never
reaches the boot path:

```
$ sudo systemctl restart aranicy
Failed to restart aranicy.service: Unit aranicy-collector.service is masked.
[exit=1]
```

And the daemon's own readiness check is loud once it runs:

```
$ sudo timeout 20 /usr/bin/aranicy start
...
Error: no collector at /run/aranicy/collector-events.sock after 5s: No such file or directory (os error 2) (is aranicy-collector.service running?)
[exit=1]
```

## Evidence

The block above is from `raw/reb3-check.txt`, taken on the boot with
`boot_id 2ce4cc9e-3a1e-4691-9f0b-bb5f80df4a6a`:

```
$ cat /proc/sys/kernel/random/boot_id
2ce4cc9e-3a1e-4691-9f0b-bb5f80df4a6a
$ uptime -p
up 1 minute
$ systemctl is-active aranicy aranicy-collector
inactive
inactive
$ systemctl is-enabled aranicy aranicy-collector
enabled
masked
$ systemctl is-failed aranicy aranicy-collector
inactive
inactive
$ systemctl status aranicy
○ aranicy.service - aranicy - auto-nice daemon
     Loaded: loaded (/usr/lib/systemd/system/aranicy.service; enabled; preset: disabled)
     Active: inactive (dead)
       Docs: https://github.com/ZhangDM-520/aranicy
$ sudo journalctl -b -u aranicy -u aranicy-collector --no-pager -o cat
(no output)
```

The package itself is fine in that boot — the only thing missing was the
start. After I unmasked the collector and started the daemon by hand, the same
boot applies the rule again:

```
$ ps -o pid,ni,comm -p 540    # after unmask + start, same boot
    PID  NI COMMAND
    540  19 xz
```

For contrast, the normal boot of the same build starts both units within
seconds of boot:

```
$ systemctl is-active aranicy aranicy-collector
active
active
$ systemctl status aranicy | head -4
● aranicy.service - aranicy - auto-nice daemon
     Loaded: loaded (/usr/lib/systemd/system/aranicy.service; enabled; preset: disabled)
     Active: active (running) since Wed 2026-11-11 10:24:00 UTC; 1min 0s ago
```

## What I could not determine

- Whether a `failed` state is reachable at all under `Requires=` when the
  required unit is masked at boot. A manual `systemctl start` fails the job,
  but the boot transaction appears to be dropped without a unit result. If
  systemd cannot mark the dependent unit failed here, then the alternative
  would be a `ConditionPathExists`/`ExecStartPre` that fails loudly, or a
  `Wants=`+self-check shape — that is a packaging decision, not something I
  can settle from outside.
- Whether the collector, when it fails at boot for a *different* reason (a
  kernel without BPF), does produce a unit failure. I did not simulate a
  kernel without BPF; the image supports it.
