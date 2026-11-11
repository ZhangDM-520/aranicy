# A rule naming a cgroup that no `.cgroups` file defines loads, passes `lint`, and then does nothing at all — no warning, no log line

- severity: minor
- category: behaviour
- environment: Linux 7.2.11-arch1-1 (Arch rolling, cloud image), aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Put the download box's churners in their own cgroup with a CPU quota, the way I
do it with ananicy-cpp: a `.cgroups` file defines the group, rules move
processes into it. A typo in the group name (or a `.cgroups` file that did not
land, which is the same failure on a freshly deployed box) must not leave me
believing the quota is in force.

## Steps to reproduce

```
sudo pacman -U --noconfirm ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst \
                         ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst
sudo systemctl enable --now aranicy
printf '%s\n' '{"name": "xz", "cgroup": "nosuchgroup"}' | sudo tee /etc/ananicy.d/50-x.rules
aranicy lint; echo "LINT_EXIT=$?"
sudo systemctl restart aranicy; sleep 3
setsid xz -9 -k -f ~/work/big.bin >/dev/null 2>&1 & sleep 3
xp=$(pgrep -nx xz); cat /proc/$xp/cgroup
test -d /sys/fs/cgroup/nosuchgroup && echo created || echo "not created"
sudo journalctl -u aranicy --since "-10 seconds" --no-pager -o cat | grep -iE "applied|cgroup"
```

(any file works for the workload; I used a 40 MB file of `/dev/urandom`.)

## Expected

Either the rule is rejected at load/lint time — "cgroup `nosuchgroup` is not
defined by any `.cgroups` file" — or the apply path logs that it could not move
the process. A rule that is loaded but can never do anything is the one thing I
must not have on an unattended box.

## Actual

`lint` is green, `dump rules` shows the rule as loaded and active, and at exec
time the process is left exactly where it was. Nothing is written to the
journal — not even the `applied rule` line that `log_applied_rule = true`
produces — and no cgroup directory is created. The only way to find out is to
inspect `/proc/<pid>/cgroup` on a live process.

This is distinct from a *partial* apply: when the rule has other fields those
are applied and the `applied rule` line is printed (demonstrated for `cpuset` in
`ivy-8`), so the log claims the whole rule landed while the field that could not
be applied is dropped silently.

## Evidence

```
$ printf '%s\n' '{"name": "xz", "cgroup": "nosuchgroup"}' | sudo tee /etc/ananicy.d/50-x.rules
{"name": "xz", "cgroup": "nosuchgroup"}

$ aranicy lint; echo "LINT_EXIT=$?"
OK: 1 rules, 0 types, 0 cgroups
LINT_EXIT=0

$ aranicy dump rules
{
  "exact": [
    {
      "cgroup": "nosuchgroup",
      "match": "exact",
      "name": "xz"
    }
  ],
  "regex": []
}

$ setsid xz -9 -k -f ~/work/big.bin >/dev/null 2>&1 & sleep 3
$ xp=$(pgrep -nx xz); cat /proc/$xp/cgroup
0::/user.slice/user-1000.slice/session-38.scope
$ test -d /sys/fs/cgroup/nosuchgroup && echo yes || echo no
no

$ sudo journalctl -u aranicy --since "-10 seconds" --no-pager -o cat
2026-11-11T09:28:33.441693Z  WARN kernel does not support latency_nice (sched_setattr LATENCY_NICE); latency_nice policy will be skipped
# nothing else. log_applied_rule was true in this run (it prints for rules whose
# other fields do get applied, see ivy-8), so the absence of a line here is the
# whole policy being dropped without a word.
```

For contrast, a *defined* group is applied correctly. Two runs from the same
session, both with a `.cgroups` file present:

```
$ printf '%s\n' '{"cgroup": "media2", "CPUQuota": 50}' | sudo tee /etc/ananicy.d/91-media.cgroups
$ sudo systemctl restart aranicy; sleep 3
$ aranicy dump cgroups
{
  "media2": {
    "cpu_quota_pct": 50
  }
}

# the earlier run, where the rule said cgroup "media" and the file defined it:
$ cat /proc/<xz pid>/cgroup
0::/media
$ cat /sys/fs/cgroup/media/cgroup.procs
1092
```

## What I could not determine

- Whether a rule may legitimately reference a group that is defined later in
  load order (a `.cgroups` file that sorts after the `.rules` file). My rule
  files sorted before the only `.cgroups` file in the failing run, so ordering
  could be part of it; I did not re-run with the definition in `00-`.
  suspicion: the reference is resolved at apply time against the loaded cgroup
  table, so ordering should not matter — but I did not prove it.
- Whether an undefined cgroup is meant to be auto-created. COMPATIBILITY.md says
  rules target "named cgroup (created from `*.cgroups`)", which reads as "must
  be defined", and there is no config key to change that.
