# The collector README's "honest check" (`aranicy-collectord probe`) fails with a BPF/kernel error when run as the user the surrounding commands are written for

- severity: minor
- category: docs
- environment: Arch Linux rolling, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Check that the collector half is actually working, using the collector's own
README, which offers one command as the ground truth:

```
## Checking it works

```sh
systemctl status aranicy-collector
journalctl -u aranicy-collector -n 20    # "exec-event client connected" when the daemon attached
aranicy-collectord probe                 # one batch of rows, printed and exited
```

`probe` is the honest check: if it prints rows, the BPF programs are attached
and the accounting hooks are firing on this kernel.
```

## Steps to reproduce

```
systemctl status aranicy-collector
journalctl -u aranicy-collector -n 20
aranicy-collectord probe
```

## Expected

Either the command works as my normal user (the other two in the same block do —
`systemctl status` and `journalctl` are readable without privileges), or the doc
marks it as needing root, the way it explains why the *unit* runs as root.

## Actual

The two commands around it work unprivileged. `probe` does not, and the error is
a libbpf/kernel message, not a permission message, so the first thing I read is
"this kernel does not support BPF":

```
libbpf: Failed to bump RLIMIT_MEMLOCK (err = -EPERM), you might need to do it explicitly!
libbpf: Error in bpf_object__probe_loading(): -EPERM. Couldn't load trivial BPF program. Make sure your kernel supports BPF (CONFIG_BPF_SYSCALL=y) and/or that RLIMIT_MEMLOCK is set to big enough value.
Error: failed to load collector BPF object

Caused by:
    Operation not permitted (os error 1)
EXIT=1
```

With `sudo` it prints rows and is exactly the check the doc promises.

## Evidence

```
$ ./machine 'echo "== probe as user =="; aranicy-collectord probe; echo "EXIT=$?"'
== probe as user ==
libbpf: Failed to bump RLIMIT_MEMLOCK (err = -EPERM), you might need to do it explicitly!
libbpf: Error in bpf_object__probe_loading(): -EPERM. Couldn't load trivial BPF program. Make sure your kernel supports BPF (CONFIG_BPF_SYSCALL=y) and/or that RLIMIT_MEMLOCK is set to big enough value.
Error: failed to load collector BPF object

Caused by:
    Operation not permitted (os error 1)
EXIT=1
$ ./machine 'sudo timeout 20 aranicy-collectord probe 2>&1 | head -3'
--- 0 rows
--- 8 rows
pid=2351    comm=mygame           prio=15  wait=0      wake=0      rt=1296546      demand=1024  deliv=408
```

The doc block, for comparison:

```
$ ./machine 'sed -n "35,43p" /usr/share/doc/aranicy-collector/README.md; echo "EXIT=$?"'
## Checking it works

```sh
systemctl status aranicy-collector
journalctl -u aranicy-collector -n 20    # "exec-event client connected" when the daemon attached
aranicy-collectord probe                 # one batch of rows, printed and exited
```

`probe` is the honest check: if it prints rows, the BPF programs are attached
EXIT=0
```

## What I could not determine

- Whether `probe` is *meant* to be runnable as a user with some capability
  (e.g. `CAP_BPF`) that this image does not grant, or whether `sudo` is simply
  missing from the line. The rest of the doc says the collector "runs as root
  because eBPF loading needs it", so `sudo` on the command is the consistent
  reading.
- Whether the "0 rows / 8 rows" pair (`--- 0 rows` then `--- 8 rows`) is a
  per-run header and a batch, or two batches; it is cosmetic either way.

`workflow this blocks`: "run the one command the doc says is the honest check" —
it produces a scary kernel error instead of rows.
