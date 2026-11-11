# A rule with `cpuset: turbo-cores` still passes `lint` and then does nothing — no warning, no log line, not even with `log_applied_rule = true`

- severity: minor
- category: behaviour
- environment: Linux 7.2.11-arch1-1 (Arch rolling, cloud image), aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Pin the download box's unpackers to "the fast cores" with the documented alias.
The document says `turbo-cores` parses for lint parity but is rejected at apply
time; what I need is that rejection to be *visible* — I deploy from Ansible and
will never notice an alias that silently does nothing.

This is the same defect I filed last round (round-2 issue 8); I am re-filing
it because I retested it on both builds and it is unchanged. One thing *has*
changed: the daemon no longer logs a false `applied` line for it (with
`log_applied_rule = true` it logs nothing at all for this rule), so the
"claims success" half is gone.

## Steps to reproduce

```
sudo pacman -U --noconfirm ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst \
                         ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst
sudo systemctl enable --now aranicy
printf '%s\n' '{"name": "tar", "cpuset": "turbo-cores"}' | sudo tee /etc/ananicy.d/ivy-53-tar.rules
aranicy lint; echo "[lint exit=$?]"
sudo sed -i 's/^log_applied_rule = false/log_applied_rule = true/' /etc/ananicy.d/ananicy.conf
sudo systemctl restart aranicy; sleep 3
setsid tar -cf /dev/null /usr >/dev/null 2>&1 &
TP=$(pgrep -nx tar)
grep Cpus_allowed_list /proc/$TP/status
sudo journalctl -u aranicy --since "-30 seconds" --no-pager -o cat
```

## Expected

`lint` warns (`cpuset: turbo-cores is not usable on this host`) and exits
non-zero, or the apply path logs one line saying the cpuset was rejected. The
shipped document already says the alias is rejected at apply time, so the
information exists.

## Actual

`lint` is green:

```
$ aranicy lint
OK: 1 rules, 0 types, 0 cgroups
[exit=0]
```

The process is not moved, and the journal says nothing — including with
`log_applied_rule = true`, which does log a normal rule on the same box:

```
$ grep Cpus_allowed_list /proc/3130/status
Cpus_allowed_list:	0-1
$ sudo journalctl -u aranicy --since "-30 seconds" --no-pager -o cat
(no output)
```

Positive control on the same configuration (rule that does apply):

```
$ sudo journalctl -u aranicy --since "$T2" --no-pager -o cat
...
2026-11-11T10:28:28.384859Z  INFO applied rule rule="xz" pid=1559 bias=0 route="exec"
```

So the `log_applied_rule` machinery works; the rejected cpuset is simply not on
any log path.

Only `aranicy match-test` tells the truth about the policy object:

```
$ aranicy match-test --config-dir /home/arch/lr tar
{
  "match": "exact",
  "name": "tar",
  "policy": {
    "cpuset": {
      "kind": "alias",
      "value": "turbo-cores"
    }
  }
}
[exit=0]
```

## Evidence

Current build, `raw/focus-cur.txt`:

```
$ aranicy lint --config-dir ~/lr
OK: 1 rules, 0 types, 0 cgroups
[exit=0]
...
$ tar affinity while running
Cpus_allowed_list:	0-1
$ journal since restart
Stopping aranicy - auto-nice daemon...
...
Started aranicy - auto-nice daemon.
2026-11-11T10:27:03.794485Z  INFO stats sampler live prev=degraded: starting detail=bound SchedSample via sched_samples at /run/aranicy/collector-stats.sock
```

Previous build, `raw/focus-prev.txt`, identical `OK: 1 rules …` / empty
journal / `Cpus_allowed_list: 0-1`.

## What I could not determine

- Whether `turbo-cores` is meant to be a hard lint error or only an apply-time
  rejection. Either is fine for me; what I cannot use is silence on both paths.
- Whether a `cpuset` failure is counted in the periodic `apply summary` line I
  have seen for cgroup failures (`dims=cgroup:failed=2`). My runs produced no
  summary at all, so I could not tell if the cpuset dimension is tracked.
