# COMPATIBILITY.md says the cgroup CPU quota is written with a 1 s `cpu.max` period; the daemon leaves the kernel's 100 ms period in place

- severity: nit
- category: docs
- environment: Linux 7.2.11-arch1-1 (Arch rolling, cloud image), aranicy 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Size a `CPUQuota` for the download box by hand from the shipped documentation,
because that document is the only reference I have on a headless machine with no
network.

## Steps to reproduce

```
sudo pacman -U --noconfirm ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst \
                         ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst
printf '%s\n' '{"cgroup": "media2", "CPUQuota": 25}' | sudo tee /etc/ananicy.d/91-media.cgroups
sudo systemctl restart aranicy; sleep 3
cat /sys/fs/cgroup/media2/cpu.max
```

## Expected

The quota in `cpu.max` matches what the document says it computes: 25 % of all
CPUs, period 1 s → `500000 1000000` on this 2-vCPU box.

## Actual

`50000 100000` — the same percentage, but over the kernel's default 100 ms
period. The percentage semantics in the document are right (I checked the
endpoint: `CPUQuota: 100` gives `200000 100000`, i.e. both cores), the stated
period is not, and the difference matters if you ever read or set `cpu.max` by
hand alongside the daemon.

## Evidence

```
$ grep -n -A2 "Cgroup definitions" /usr/share/doc/aranicy/COMPATIBILITY.md
40:Cgroup definitions: `{"cgroup": name, "CPUQuota": N}` — N = percent of *all*
41-CPUs (v2 `cpu.max`, period 1 s). Duplicates: later-loaded `name` replaces
42-outright. Load order: sorted file paths (deterministic superset over

$ printf '%s\n' '{"cgroup": "media2", "CPUQuota": 25}' | sudo tee /etc/ananicy.d/91-media.cgroups
{"cgroup": "media2", "CPUQuota": 25}
$ sudo systemctl restart aranicy; sleep 3
$ cat /sys/fs/cgroup/media2/cpu.max
50000 100000

$ printf '%s\n' '{"cgroup": "media3", "CPUQuota": 100}' | sudo tee /etc/ananicy.d/92-media3.cgroups
$ sudo systemctl restart aranicy; sleep 2
$ cat /sys/fs/cgroup/media3/cpu.max
200000 100000

$ aranicy dump cgroups
{
  "media2": {
    "cpu_quota_pct": 50
  }
}
# (that dump was taken in the run where the definition said CPUQuota: 50; the
# field name confirms the daemon stores a percentage and computes the quotient)
```

Related, same paragraph: `CPUQuota` is validated as `0..=100`, so an
ananicy-cpp corpus carrying a bigger number is warned about rather than applied:

```
$ printf '%s\n' '{"cgroup": "media", "CPUQuota": 200}' | sudo tee /etc/ananicy.d/91-media.cgroups
$ sudo systemctl reload aranicy
2026-11-11T09:19:12.074066Z  WARN /etc/ananicy.d/91-media.cgroups:1: CPUQuota: 200 out of range 0..=100
```

That is consistent with "percent of all CPUs" (100 = everything) and is fine;
only "period 1 s" is wrong.

## What I could not determine

- Whether the period is intended to be configurable. There is no key for it in
  the documented key list, and `cpu.max` shows the kernel default.
