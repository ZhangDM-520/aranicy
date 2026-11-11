# A corpus entry named after the daemon re-nices the daemon itself from the unit's Niceness=-5 to 19, with no log line; it happens on every boot while that rule exists

- severity: major
- category: safety
- environment: Linux 7.2.11-arch1-1 (Arch rolling), aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB, `check_freq = 60`, `log_applied_rule = false`

## What I was trying to do

Blast-radius pass: the unit pins the daemon at `Nice=-5`, `OOMScoreAdjust=-999`
and the collector at `Nice=-10`, which tells a reader the tool keeps itself
responsive on purpose. I wrote a corpus entry with the daemon's own name to see
whether it is exempt from its own policy.

## Steps to reproduce

```
printf '{"name": "aranicy", "nice": 19}\n' | sudo tee /etc/ananicy.d/self.rules
sudo aranicy --reload
awk '{print $19}' /proc/$(pgrep -x aranicy | head -1)/stat      # -5
sleep 65
awk '{print $19}' /proc/$(pgrep -x aranicy | head -1)/stat      # 19
sudo journalctl -u aranicy --since "-2min" | grep -c aranicy    # only reload lines
```

## Expected

Either the daemon is exempt from its own corpus (anything that ships a
self-protection posture usually is), or the write is recorded — a daemon that
drops its own priority by 24 nice levels is exactly the event an operator wants
in the log. What is not acceptable is neither.

## Actual

The daemon re-nices itself to 19 at the first `check_freq` pass (60 s) after the
rule is loaded, and again after every boot as long as the rule exists. Nothing
is logged: `grep -c` for anything naming `aranicy` plus nice/applied in the
journal is 0 across the whole window; the only journal lines are the `--reload`
ones. `systemd`'s `Nice=-5` is still what `systemctl show -p Nice` reports, so
the unit and the process disagree and there is no record of why.

The collector is *not* reachable by its process name (`aranicy-collect`, because
its comm is truncated) but is by its executable name (`aranicy-collectord`); I
did not drive that path to a state change in this round.

This is the same silent-drift-write mechanism round 2 filed for `sshd`
(dana-8), aimed at the tool's own process. It matters more here because the
value the unit sets is deliberate and because the daemon is the thing that has
to wake up in microseconds at every exec.

## Evidence

```
$ awk '{print $19}' /proc/$(pgrep -x aranicy)/stat
-5
$ printf '{"name": "aranicy", "nice": 19}\n' | sudo tee /etc/ananicy.d/self.rules
{"name": "aranicy", "nice": 19}
$ sudo aranicy --reload
sent SIGHUP to aranicy (pid 2319)
$ sleep 65; awk '{print $19}' /proc/2319/stat; systemctl show -p Nice --value aranicy
19
-5
$ sudo journalctl -u aranicy --no-pager --since "10:02:00" | grep -cE "aranicy.*nice|applied"
0
```

Independent run, the same test inside a fixed battery on both shipped builds
(`B15`), showing it is not a one-off:

```
-- B15 daemon applies the corpus to itself --
daemon nice=19 (unit Nice=-5)        # current build
-- B15 daemon applies the corpus to itself --
daemon nice=19 (unit Nice=-5)        # previous build
```

And across a reboot with the rule in place (the rule survives in
`/etc/ananicy.d`, the daemon is fresh, the result is the same — this is 71 s
after the boot at 10:07:50):

```
=== what the daemon/collector look like after boot ===
daemon nice=19 collector nice=-10 (unit Nice=-5/-10)
```

Also worth recording from the same probe: the `--reload` client is itself
matched by the same rule and is re-niced to 19 mid-flight (`aranicy dump proc`
showed a third row, `{"name": "aranicy", "nice": 19, "pid": 2597}` while the
reload client was alive). That one is harmless; it is included only because it
proves the match is on the executable name and not on the unit.

## What I could not determine

- Whether the maintainers consider the daemon in scope for its own corpus. The
  new "Trust model" section in `docs/COMPATIBILITY.md` says the corpus is
  root's policy and is applied to "anything that matches" — if that is meant to
  include the daemon, the entry above is working as designed and this issue
  reduces to "the unit's Niceness= is silently overridden, and nothing logs
  it". I can defend the second half either way.
- Whether the same happens with a regex entry (`name_regex`) that happens to
  match `aranicy`. I only used an exact `name`, which is the narrowest possible
  case; a regex would reach more.
