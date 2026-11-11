# With no rules at all, the learned bias demotes PID 1 (systemd) and core system services

- severity: major
- category: safety
- environment: Arch Linux cloud image, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 +
  aranicy-collector 0.3.0-1 (local/pacman), 2 vCPU / 1958 MiB / no display

## What I was trying to do

Run the package as delivered — shipped `ananicy.conf`, no rules of my own — and
watch what the dynamic bias does before trusting it with a work machine. The
README describes bias as "bulk work nudged nicer, latency-sensitive work less
nice". I did not expect init to be in either category.

## Steps to reproduce

```sh
sudo pacman -U --noconfirm ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst \
    ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst
sudo systemctl enable --now aranicy
ls /etc/ananicy.d/rules      # empty: the package ships no rules
# leave the machine alone for ~6-7 minutes, then:
awk '{print "comm="$2" nice="$19}' /proc/1/stat
systemctl show -p Nice systemd
sudo journalctl -u aranicy --no-pager -o cat | grep "bias changed"
```

## Expected

The bias engine either ignores PID 1 and the core system services, or it is
governed by a user rule / allow-deny list that, by default, keeps them out. Nicing
init is not a latency trade-off I asked for, and the shipped config contains
nothing about it (its only bias-related key is `bias_group.build`).

## Actual

The engine classified `systemd` as `Bulk` and walked PID 1's nice up in 30 s
steps: +1, +2, +3, all from `cause=Evidence`. `systemd-journald` and
`systemd-userwork` were demoted the same way, and `dbus-broker` was *raised*
(nice −1) as `Responsive`. PID 1 sits at nice 1–2 depending on how long the
daemon has been up — it reached 1 first, then 2 with an empty ruleset at the end
of the session — and the tool reports it in its own `dump proc`
(`{"name": "systemd", "nice": 1, "pid": 1}`).

I can see the decision but not the reason it is allowed: there is no rule, no
config key and no log line that says "PID 1 is exempt" or "PID 1 is in scope".

## Evidence

```
$ sudo journalctl -u aranicy --no-pager -o cat | grep "bias changed" | head -12
2026-11-11T08:18:49.421643Z  INFO bias changed name=bigbuild bias=1 prev=0 class=Bulk cause=Evidence
2026-11-11T08:18:54.417003Z  INFO bias changed name=latprobe bias=1 prev=0 class=Bulk cause=Evidence
2026-11-11T08:19:19.421365Z  INFO bias changed name=bigbuild bias=2 prev=1 class=Bulk cause=Evidence
2026-11-11T08:20:49.481635Z  INFO bias changed name=systemd bias=1 prev=0 class=Bulk cause=Evidence
2026-11-11T08:21:19.481885Z  INFO bias changed name=systemd bias=2 prev=1 class=Bulk cause=Evidence
2026-11-11T08:21:49.133453Z  INFO bias changed name=systemd-journal bias=1 prev=0 class=Bulk cause=Evidence
2026-11-11T08:21:54.479890Z  INFO bias changed name=systemd-userwor bias=1 prev=0 class=Bulk cause=Evidence
2026-11-11T08:21:59.480635Z  INFO bias changed name=systemd bias=3 prev=2 class=Bulk cause=Evidence
2026-11-11T08:22:19.479166Z  INFO bias changed name=dbus-broker bias=-1 prev=0 class=Responsive cause=Evidence
2026-11-11T08:22:24.478853Z  INFO bias changed name=systemd-journal bias=2 prev=1 class=Bulk cause=Evidence
2026-11-11T08:22:29.478888Z  INFO bias changed name=systemd-userwor bias=2 prev=1 class=Bulk cause=Evidence
2026-11-11T08:38:31.102458Z  INFO bias changed name=true bias=1 prev=0 class=Bulk cause=Evidence
```

```
$ awk '{print "pid1 comm="$2" nice="$19}' /proc/1/stat
pid1 comm=(systemd) nice=1

$ systemctl show -p Nice systemd
Nice=1

$ aranicy dump proc | head -8
[
  {
    "name": "systemd",
    "nice": 1,
    "pid": 1
  },
  {
    "name": "kthreadd",
```

The persisted state agrees (`/var/lib/aranicy/bias-state`):

```
  spinonly           bias=+1 class=bulk conf=0.74
  systemd            bias=+1 class=bulk conf=0.67
  true               bias=+3 class=bulk conf=0.72
```

It is not a leftover of my test rules: with every rule file removed at the end of
the session, the ruleset is empty and PID 1 is still being walked up.

```
$ aranicy dump rules
{
  "exact": [],
  "regex": []
}
$ sudo journalctl -u aranicy --no-pager -o cat | grep "bias changed name=systemd" | tail -2
2026-11-11T08:42:01.102555Z  INFO bias changed name=systemd bias=1 prev=0 class=Bulk cause=Evidence
2026-11-11T08:42:36.102461Z  INFO bias changed name=systemd bias=2 prev=1 class=Bulk cause=Evidence
$ awk '{print "pid1 nice="$19}' /proc/1/stat
pid1 nice=2
```

## What I could not determine

- Whether the effect on PID 1 is transient: the level moved +1 → +3 and later
  back to +1 across restarts, so I could not establish whether it keeps climbing
  under different load. The level is not the point; the scope is.
- What `systemd`'s own score was that justified `Bulk`. `dump bias` exposes
  `score_ema` and `class` but not the threshold comparison, so I can see the
  verdict, not the arithmetic.
- I did not record `/proc/1/stat`'s nice before installing the package, so I
  cannot prove it was 0 beforehand by measurement on this box (it is the default
  for PID 1). Everything above is the tool's own output: it says it changed the
  bias for `systemd`, and PID 1's nice is 1 afterwards.
- Whether kernel threads are exempt in practice. `dump bias` has no kthread
  entries and `dump proc` shows `kthreadd` at nice 0, so the exemption looks like
  it works for those — it just does not cover PID 1 or the system services.
