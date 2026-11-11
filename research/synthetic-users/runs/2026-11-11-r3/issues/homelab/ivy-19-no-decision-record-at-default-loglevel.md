# At the default `loglevel = info`, a successful apply leaves no record at all; the periodic `apply summary` only appears when something failed

- severity: minor
- category: enhancement
- environment: Linux 7.2.11-arch1-1 (Arch rolling, cloud image), aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Answer "was the policy actually being applied during that transcode?" months
after the fact, from `journalctl -o json`. The brief I work to is: logs say
what was decided, or explicitly say there was nothing to decide. A box that
applied 200 rules and a box with a broken event source look identical in the
journal today.

## Steps to reproduce

```
sudo pacman -U --noconfirm ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst \
                         ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst
sudo systemctl enable --now aranicy
printf '%s\n' '{"name": "xz", "nice": 19}' | sudo tee /etc/ananicy.d/ivy-50-xz.rules
sudo systemctl reload aranicy; sleep 2
T=$(date '+%Y-%m-%d %H:%M:%S')
for i in $(seq 1 10); do setsid xz -9 -c /home/arch/work/big.bin >/dev/null 2>&1 & sleep 0.1; done
sleep 8
sudo journalctl -u aranicy --since "$T" --no-pager -o cat
sudo journalctl -b -u aranicy --no-pager -o cat | grep -i "apply summary"
```

## Expected

Either a per-apply line (that is what `log_applied_rule = true` gives, but it is
off by default because it is noisy) or a periodic summary at `info` saying
"applied N of M exec events since the last summary", so the journal answers
"was it working" without turning on the noisy mode.

## Actual

Nothing at all for ten successful applies, and no summary line in that boot:

```
$ sudo journalctl -u aranicy --since "$T" --no-pager -o cat
(no output)
$ sudo journalctl -b -u aranicy --no-pager -o cat | grep -i "apply summary"
[grep exit=0]
```

The only decision record I have seen is a `WARN` summary emitted when a
dimension failed, for example when a rule named an undefined cgroup:

```
2026-11-11T10:15:09.081977Z  WARN apply summary events=8 matched=2 unmatched=6 unresolved=0 drift_seen=37 drift_managed=0 drift_unmatched=33 dropped=0 dims=cgroup:failed=2 drift_dims=
```

So the summary mechanism exists and is useful; it just does not fire when
everything works. (`log_applied_rule = true` does produce per-rule lines; this
is not a request to turn that on by default.)

## Evidence

`raw/adv6.txt`, section G2:

```
$ journal lines since the 10 execs
(no output)

$ apply summary lines in this boot
[grep exit=0]

$ any decision line at default loglevel
2
```

(the `2` is the count of `applied rule|apply summary|decided` lines in the
whole boot — both from the earlier run that had a failing cgroup rule, none
from the ten successful execs).

Positive control with `log_applied_rule = true` (`raw/focus-cur.txt`):

```
2026-11-11T10:28:28.384859Z  INFO applied rule rule="xz" pid=1559 bias=0 route="exec"
```

## What I could not determine

- Whether the summary is deliberately failure-only (the name `apply summary`
  and the `dims=…failed=…` field suggest it is meant to be a health line, not
  a success line).
- What interval the summary uses; I only saw one per run, and I did not
  isolate whether it is emitted every N seconds or per accumulated failure.
- Whether the bias engine already emits a periodic line that would serve the
  same purpose (I saw none at `info`; bias lines are `bias changed`-style and
  only when a step is taken, which did not happen on this box in 60 s).
