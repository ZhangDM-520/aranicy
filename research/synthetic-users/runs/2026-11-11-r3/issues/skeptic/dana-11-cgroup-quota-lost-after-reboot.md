# After a reboot the configured cgroup CPUQuota is not in effect: the cgroup is created with `cpu.max = max 100000` and one startup WARN is the only sign

- severity: major
- category: behaviour
- environment: Linux 7.2.11-arch1-1 (Arch rolling), aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB, `check_freq = 60`, shipped `ananicy.conf`

## What I was trying to do

Exercise the cgroup rule surface through the shipped documentation
(`COMPATIBILITY.md`: `{"cgroup": name, "CPUQuota": N}` — N = percent of all
CPUs), then reboot the machine in the middle of the work and see what came
back. The cgroup moved processes correctly before the reboot; after it, the
quota silently stopped existing.

## Steps to reproduce

```
# as root, with the shipped config (check_freq = 60)
printf '{"cgroup": "skepticcg", "CPUQuota": 50}\n' | sudo tee /etc/ananicy.d/cg.cgroups
printf '{"name": "skepticcg2", "cgroup": "skepticcg"}\n' | sudo tee /etc/ananicy.d/cgrule.rules
sudo aranicy --reload
cat /sys/fs/cgroup/skepticcg/cpu.max            # 100000 100000  (correct)
sudo reboot
# after the machine is back:
journalctl -b -u aranicy | grep cgroup          # one WARN, expected "not created"
cat /sys/fs/cgroup/skepticcg/cpu.max            # max 100000      (quota gone)
sudo systemctl restart aranicy
cat /sys/fs/cgroup/skepticcg/cpu.max            # 100000 100000  (back)
```

## Expected

A rule whose whole purpose is a CPU limit either applies after boot or says,
every time it is consulted, that it did not apply. `aranicy dump cgroups` is
the tool's own view of what it loaded, so it should not report a quota that is
not in the kernel.

## Actual

At boot the daemon logs exactly one line:

```
WARN cgroup "skepticcg" not created: Permission denied (os error 13)
```

At t+20 s after boot the cgroup directory does not exist at all, and by t+80 s
it exists with `cpu.max = max 100000`, i.e. **unlimited**. No further line is
ever written; `aranicy dump cgroups` keeps reporting `cpu_quota_pct: 50`, and
`/proc/<pid>/cgroup` shows the process inside `/skepticcg`, so every
user-visible surface claims the limit exists. `systemctl restart aranicy`
recreates the cgroup correctly (`100000 100000`) and the value then held for
the 200 s I watched it.

The failure is specific to boot; `rmdir /sys/fs/cgroup/skepticcg` followed by
`systemctl restart` re-applies the quota within one second.

## Evidence

```
$ cat /sys/fs/cgroup/skepticcg/cpu.max      # after --reload, before reboot
100000 100000
$ sudo reboot
--- boot-time one-shot sampler (After=aranicy.service, sleeps 20 s) ---
Wed Nov 11 10:24:03 AM UTC 2026
cpu.max=cat: /sys/fs/cgroup/skepticcg/cpu.max: No such file or directory
WARN lines: 1
--- sampled every 10 s from t+80 s ---
t=10s cpu.max=max 100000 daemon_nice=-5
t=20s cpu.max=max 100000 daemon_nice=-5
...
t=100s cpu.max=max 100000 daemon_nice=-5
$ sudo journalctl -b -u aranicy --no-pager | grep -i cgroup
Nov 11 10:23:43 su-skeptic-r3 aranicy[323]: 2026-11-11T10:23:43.087844Z  WARN cgroup "skepticcg" not created: Permission denied (os error 13)
$ aranicy dump cgroups
{
  "skepticcg": {
    "cpu_quota_pct": 50
  }
}
$ sudo systemctl restart aranicy
t=1 s EXISTS cpu.max=100000 100000
t=2 s EXISTS cpu.max=100000 100000
... (unchanged through t=200 s)
$ sudo rmdir /sys/fs/cgroup/skepticcg; sudo systemctl restart aranicy
t=1 s EXISTS cpu.max=100000 100000
```

## What I could not determine

- *Why* boot differs from a restart. The daemon reports `Permission denied` for
  a write that succeeds a minute later as root, so something about early boot
  ordering is involved; I did not strace the daemon (not installed) and I am
  not going to guess at the mechanism.
- Whether the quota would be re-applied eventually by a later pass without a
  restart. I watched 100 s post-boot (and 200 s after a restart in a separate
  run) and it never recovered by itself; `check_freq = 60` means at least one
  drift pass had run inside that window.
- One observation I could not reproduce and therefore do not claim: at 10:23:41,
  46 s after a manual `systemctl restart` that had written `100000 100000`,
  `cat cpu.max` read `max 100000` again with no reboot in between. A 200 s
  watch in the following run never showed that reversion.
- Whether this affects `cpuset`/`io` style directives too — I only tested
  `CPUQuota`.
