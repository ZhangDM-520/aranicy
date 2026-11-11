# aranicy lint prints "OK" and exits 0 for rules that can never apply, and for files it never reads

- severity: major
- category: behaviour
- environment: Arch Linux cloud image, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 +
  aranicy-collector 0.3.0-1 (local/pacman), 2 vCPU / 1958 MiB / no display

## What I was trying to do

Keep my rules in a version-controlled directory and gate merges on `aranicy lint`
in a pipeline, treating exit 0 as "this corpus will produce policy". A corpus that
is green but does nothing is worse than a red build, because I stop looking.

## Steps to reproduce

A fresh rules directory plus a copy of the shipped daemon config (required, see
ren-2), then four separate single-rule corpora.

```sh
rm -rf ~/repro && mkdir -p ~/repro/rules && cp /etc/ananicy.d/ananicy.conf ~/repro/ananicy.conf
```

**(a) a glob in `name`** (the obvious way to write "all the cc1 variants"):

```sh
printf '%s\n' '{ "name": "cc1*", "nice": 5 }' > ~/repro/rules/a.rules
aranicy --config-dir ~/repro lint
aranicy --config-dir ~/repro match-test cc1plus
```

**(b) an empty name**:

```sh
printf '%s\n' '{ "name": "", "nice": 5 }' > ~/repro/rules/a.rules
aranicy --config-dir ~/repro lint
aranicy --config-dir ~/repro dump rules
```

**(c) the same name in two files** (what happens when a corpus grows and a rule
gets redefined):

```sh
printf '%s\n' '{ "name": "zzsleep", "nice": 15 }' > ~/repro/rules/10-a.rules
printf '%s\n' '{ "name": "zzsleep", "nice": 2 }'  > ~/repro/rules/90-z.rules
aranicy --config-dir ~/repro lint
aranicy --config-dir ~/repro match-test zzsleep
```

**(d) a file with the wrong extension** (a restored `.bak`, or a corpus that
happens to be named `.json`):

```sh
printf '%s\n' '{ "name": "x", "nice": 5 }' > ~/repro/rules/a.json
aranicy --config-dir ~/repro lint
aranicy --config-dir ~/repro dump rules
```

## Expected

For (a) and (b): a diagnostic with file and line, or at least a non-zero exit —
nothing in the package documents globs or empty names as meaningful. For (c): a
warning that the name is defined twice (the shipped config parser *does* warn on
duplicate keys, line and all), or a report of which definition won. For (d): a
warning that a file in the rules directory was skipped, since the alternative is
a corpus that silently contains nothing.

## Actual

All four print `OK: …` and exit 0. (a) stores `cc1*` as a literal exact name that
matches nothing. (b) loads a rule whose name can never be a process name. (c)
silently keeps the later file's value with no mention of the override. (d) reads
no rule at all and still reports success.

## Evidence

```
### (a) glob in name
$ aranicy --config-dir /home/arch/repro lint
OK: 1 rules, 0 types, 0 cgroups
exit=0

$ aranicy --config-dir /home/arch/repro match-test cc1plus
match-test: no rule matches "cc1plus"
exit=1

### (b) empty name
$ aranicy --config-dir /home/arch/repro lint
OK: 1 rules, 0 types, 0 cgroups
exit=0

$ aranicy --config-dir /home/arch/repro dump rules
{
  "exact": [
    {
      "match": "exact",
      "name": "",
      "nice": 5
    }
  ],
  "regex": []
}
exit=0

### (d) file named a.json (not .rules)
$ aranicy --config-dir /home/arch/repro lint
OK: 0 rules, 0 types, 0 cgroups
exit=0

$ aranicy --config-dir /home/arch/repro dump rules
{
  "exact": [],
  "regex": []
}
exit=0

### (e) same name in two files
$ aranicy --config-dir /home/arch/repro lint
OK: 1 rules, 0 types, 0 cgroups
exit=0

$ aranicy --config-dir /home/arch/repro match-test zzsleep
{
  "match": "exact",
  "name": "zzsleep",
  "policy": {
    "nice": 2
  }
}
exit=0
```

The same hole exists at runtime and is even quieter: a rule that references a
cgroup that was never defined is accepted, counted, and then does nothing —
while an undefined *type* reference at least warns.

```
$ sudo tee /etc/ananicy.d/rules/60-cg.rules >/dev/null <<EOF
{ "name": "zzsleep", "cgroup": "zzgroup" }
EOF
$ aranicy lint; echo "LINT=$?"
warning: /etc/ananicy.d/rules/10-test.rules:2: unknown type "Heavy" referenced; rule keeps its own fields
lint: warnings found
LINT=1
$ sudo systemctl reload aranicy; sleep 1
$ sudo journalctl -u aranicy --no-pager -o cat -n 3
sent SIGHUP to aranicy (pid 1798)
2026-11-11T08:24:52.961822Z  WARN /etc/ananicy.d/rules/10-test.rules:2: unknown type "Heavy" referenced; rule keeps its own fields
2026-11-11T08:24:52.962114Z  INFO reloaded 5 rules; bias reconfigured
$ ~/zzsleep 30 & pid=$!; sleep 1.5; cat /proc/$pid/cgroup
0::/user.slice/user-1000.slice/session-71.scope
```

No line in the journal mentions `zzgroup`; the process was not moved, and the
only complaint on reload concerns a different file. Once I *did* define the
cgroup, the very same rule moved the process:

```
$ sudo tee /etc/ananicy.d/cgroups/zz.cgroups >/dev/null <<EOF
{ "cgroup": "zzgroup", "cpuset": "0" }
EOF
$ aranicy dump cgroups
{
  "zzgroup": {
    "cpu_quota_pct": null
  }
}
$ ~/zzsleep 20 & pid=$!; sleep 1.5; cat /proc/$pid/cgroup
0::/zzgroup
```

So the rule was correct, the reference resolution silently returned nothing, and
`lint` never said so.

## What I could not determine

- Which extensions are actually read. `.rules` is read, `.json` and `.rules.bak`
  are not; nothing in the package states the rule.
- Whether `*` in `name` is intended to be a glob at all. `match-test` proves it is
  treated as a literal, but no shipped document describes `name` matching.
- Whether a duplicate name is meant to be an override mechanism (last file wins).
  If it is, `dump rules` and `match-test` do not say which file won, so I cannot
  audit the corpus (see ren-7).
