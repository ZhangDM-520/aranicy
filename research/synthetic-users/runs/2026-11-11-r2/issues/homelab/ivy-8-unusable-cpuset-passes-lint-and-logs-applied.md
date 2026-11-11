# A `cpuset` value the kernel can never honour (an alias or a CPU list wider than the machine) passes `lint`, and the daemon logs `applied rule` anyway

- severity: minor
- category: behaviour
- environment: Linux 7.2.11-arch1-1 (Arch rolling, cloud image), aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Pin the transcoder to the performance cores, the way my ananicy-cpp corpus did,
and pin a benchmark to one core. I copied a rule out of an old corpus that used
`turbo-cores`, and I also typo'd a range (`99-100`). Both must be visible
failures on a box I do not log into for a month.

## Steps to reproduce

```
sudo pacman -U --noconfirm ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst \
                         ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst
sudo systemctl enable --now aranicy
printf '%s\n' '{"name": "openssl", "nice": 4, "cpuset": "turbo-cores"}' | sudo tee /etc/ananicy.d/60-turbo.rules
aranicy lint; echo "LINT_EXIT=$?"
sudo systemctl restart aranicy; sleep 3
setsid timeout 20 openssl speed -seconds 20 sha256 >/dev/null 2>&1 & sleep 4
op=$(pgrep -nx openssl); ps -o ni= -p $op; taskset -pc $op
sudo journalctl -u aranicy --since "-10 seconds" --no-pager -o cat | grep applied
# and the same with 99-100:
printf '%s\n' '{"name": "openssl", "nice": 4, "cpuset": "99-100"}' | sudo tee /etc/ananicy.d/60-turbo.rules
aranicy lint; echo "LINT_EXIT=$?"
```

## Expected

`lint` reports the rule ("cpuset `turbo-cores` cannot be applied on any kernel")
and the daemon says at apply time that the affinity was not set. At minimum the
log line must not claim the whole rule was applied when one of its fields was
dropped.

## Actual

`lint` exits 0 for both rules. At apply time the `nice` is applied, the
affinity is untouched, and the daemon prints `INFO applied rule`, which reads as
"the whole policy landed". Nothing in the journal, in `dump rules`, or in the
`lint` output says the `cpuset` part was discarded.

The documentation does say `turbo-cores` is rejected at apply time, so the
rejection is intended; the silence is what costs me. `99-100` is not mentioned
anywhere.

## Evidence

```
$ printf '%s\n' '{"name": "openssl", "nice": 4, "cpuset": "turbo-cores"}' | sudo tee /etc/ananicy.d/60-turbo.rules
{"name": "openssl", "nice": 4, "cpuset": "turbo-cores"}
$ aranicy lint; echo "LINT_EXIT=$?"
OK: 1 rules, 0 types, 0 cgroups
LINT_EXIT=0

$ setsid timeout 20 openssl speed -seconds 20 sha256 >/dev/null 2>&1 & sleep 4
$ op=$(pgrep -nx openssl); echo "pid=$op nice=$(ps -o ni= -p $op) affinity=$(taskset -pc $op | sed 's/.*: //')"
pid=4264 nice=  4 affinity=0,1

$ sudo journalctl -u aranicy --since "-10 seconds" --no-pager -o cat | grep -i applied
2026-11-11T09:30:11.093785Z  INFO applied rule rule="openssl" pid=4264 bias=0 route="exec"

$ printf '%s\n' '{"name": "openssl", "nice": 4, "cpuset": "99-100"}' | sudo tee /etc/ananicy.d/60-turbo.rules
$ aranicy lint; echo "LINT_EXIT=$?"
OK: 1 rules, 0 types, 0 cgroups
LINT_EXIT=0
```

For contrast, the same corpus with a `cpuset` this kernel *can* honour is
applied and visible (earlier run, same machine, rule was
`{"name": "openssl", "nice": 4, "cpuset": "0"}`):

```
$ setsid timeout 25 openssl speed -seconds 25 -multi 1 sha256 >/dev/null 2>&1 & sleep 4
pid=1137 nice=  7 affinity=0
# nice 7 because a stored learned bias of 3 was active for openssl in that run:
# the cpuset half is what matters here, and it was applied
```

## What I could not determine

- Whether the out-of-range case (`99-100` on a 2-CPU box) is meant to be a lint
  error rather than a runtime rejection. The two cases look identical from
  outside.
- Whether any log level prints the rejected field. I only ran at `loglevel =
  info` with `log_applied_rule = true`.
