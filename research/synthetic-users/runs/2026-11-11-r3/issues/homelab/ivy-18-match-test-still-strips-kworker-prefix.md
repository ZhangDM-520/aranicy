# `match-test` now matches kernel-thread names (fixed), but its "no rule matches" message still prints a stripped name (`0:1`), not the name I asked about

- severity: nit
- category: behaviour
- environment: Linux 7.2.11-arch1-1 (Arch rolling, cloud image), aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Check a rule written against a kernel-thread name before deploying it. Last
round I reported that `match-test 'kworker/0:1'` answered `no rule matches
"0:1"` and that a name containing `/` could not be matched at all. I retested
that on both builds: **the matching half is fixed in this build** — a rule
keyed on `kworker/0:1-events` is now found exactly. What is left is the
message: when nothing matches, `match-test` prints a name with everything up
to the first `/` removed, so it reports on a process name I never typed.

## Steps to reproduce

```
sudo pacman -U --noconfirm ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst \
                         ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst
mkdir -p ~/lrw; cp /etc/ananicy.d/ananicy.conf ~/lrw/
printf '%s\n' '{"name": "kworker/0:1-events", "nice": 19}' > ~/lrw/ivy-k.rules
aranicy match-test --config-dir ~/lrw 'kworker/0:1-events'; echo "[exit=$?]"
aranicy match-test --config-dir ~/lrw 'kworker/0:1';        echo "[exit=$?]"
```

## Expected

The success case reports the rule (it does). The failure case should echo the
name I passed — `no rule matches "kworker/0:1"` — or say it normalised the
name. A message that silently names a different string makes me doubt the
input I typed (my first reaction last round was that the rule itself was
unusable).

## Actual

```
$ aranicy match-test --config-dir ~/lrw 'kworker/0:1-events'
{
  "match": "exact",
  "name": "kworker/0:1-events",
  "policy": {
    "nice": 19
  }
}
[exit=0]

$ aranicy match-test --config-dir ~/lrw 'kworker/0:1'
match-test: no rule matches "0:1"
[exit=1]
```

The name I passed was `kworker/0:1`; the message reports `0:1`.

For contrast, the previous build cannot match the rule at all — this is the
part that got fixed:

```
$ aranicy match-test --config-dir ~/lrw 'kworker/0:1-events'
match-test: no rule matches "0:1-events"
[exit=1]
$ aranicy match-test --config-dir ~/lrw 'kworker/R-rcu_gp'
match-test: no rule matches "R-rcu_gp"
[exit=1]
```

## Evidence

Current build:

```
$ aranicy lint --config-dir ~/lrw
OK: 2 rules, 0 types, 0 cgroups
[exit=0]
$ aranicy match-test --config-dir ~/lrw "kworker/0:1-events"
{
  "match": "exact",
  "name": "kworker/0:1-events",
  "policy": {
    "nice": 19
  }
}
[exit=0]
$ aranicy match-test --config-dir ~/lrw "kworker/R-rcu_gp"
{
  "match": "exact",
  "name": "kworker/R-rcu_gp",
  "policy": {
    "nice": 19
  }
}
[exit=0]
$ aranicy match-test --config-dir ~/lrw "kworker/0:1"
match-test: no rule matches "0:1"
[exit=1]
```

Previous build, same config directory:

```
$ aranicy lint --config-dir ~/lrw
OK: 2 rules, 0 types, 0 cgroups
[exit=0]
$ aranicy match-test --config-dir ~/lrw "kworker/0:1-events"
match-test: no rule matches "0:1-events"
[exit=1]
$ aranicy match-test --config-dir ~/lrw "kworker/0:1"
match-test: no rule matches "0:1"
[exit=1]
$ aranicy match-test --config-dir ~/lrw kworker
match-test: no rule matches "kworker"
[exit=1]
```

`dump proc` also shows the full name in this build (58 `kworker` lines), which
is consistent with the matcher keeping the name intact.

## What I could not determine

- Whether the stripped string in the message is the daemon's internal display
  name or a `basename()` applied to the input on the error path only. The
  success case proves the matcher sees the full name.
- Whether this is worth a change at all: it is cosmetic. I filed it because I
  mis-read it as a matching bug last round, and the next person will too.
