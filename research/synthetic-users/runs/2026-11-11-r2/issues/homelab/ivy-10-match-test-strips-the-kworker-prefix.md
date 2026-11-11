# `match-test` and `dump proc` report kernel-thread names with the `kworker/` prefix stripped, so a name containing `/` cannot be matched or grepped

- severity: minor
- category: behaviour
- environment: Linux 7.2.11-arch1-1 (Arch rolling, cloud image), aranicy 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Find out what the daemon thinks a given process is called, so I can write rules
against it — that is exactly what `match-test` and `dump proc` are for, and the
compatibility document promises the derived name is `basename(/proc/<pid>/exe)`,
then `basename(argv[0])`, then **the kernel comm**. The kernel comm of a
workqueue thread is `kworker/0:1-events`, i.e. it contains a `/`.

## Steps to reproduce

```
sudo pacman -U --noconfirm ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst \
                         ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst
cat /proc/37/comm           # any kworker will do; take a pid from ps
aranicy dump proc | grep -A2 37
aranicy match-test "$(cat /proc/37/comm)"
```

## Expected

The name the daemon derives for a kernel thread is the kernel comm, as
documented, so `match-test 'kworker/0:1-events'` either matches a rule for that
comm or reports the whole name it looked up.

## Actual

The `basename()` rule that exists to normalise `/usr/bin/openssl` is also applied
to the comm, so the name the daemon reports — and the name it looks up — has
everything up to the last `/` removed: `kworker/u8:2-btrfs-endio-write` becomes
`u8:2-btrfs-endio-write`, and `match-test kworker/0:1-events` says
`no rule matches "0:1-events"`.

In practice kernel threads are excluded from the learned axis anyway, so this is
a diagnostics defect, not a safety one — but it is the tool I would use to prove
the exclusion, and it prints a name that is not the process's name.

## Evidence

```
$ cat /proc/37/comm
kworker/u8:2-btrfs-endio-write
$ ps -o pid,comm -p 37
    PID COMMAND
     37 kworker/u8:2-btrfs-endio-write

$ aranicy dump proc | python3 -c "import json,sys; d=json.load(sys.stdin); [print(e) for e in d if e['pid'] in (10,37)]"
{'name': '0:1-events', 'nice': 0, 'pid': 10}
{'name': 'u8:2-btrfs-endio-write', 'nice': 0, 'pid': 37}

$ aranicy match-test "kworker/0:1"
match-test: no rule matches "0:1"
$ aranicy match-test /usr/bin/openssl
{
  "match": "exact",
  "name": "openssl",
  "policy": {
    "nice": 7
  }
}
```

(The second command shows why the basename call is there — it is useful for
paths. The two behaviours are indistinguishable from outside.)

## What I could not determine

- Whether a rule written as `"name": "kworker/0:1-events"` would match a live
  kernel thread. I did not write one: a rule that matches a kernel thread is
  something I do not want on the box even for an experiment, and the learned
  axis is documented as skipping kernel threads via the `PF_KTHREAD` flag.
- Whether `dump proc` is meant to report the comm or the *executable* name for
  every pid; for a kernel thread there is no executable, so the comm is the only
  candidate.
