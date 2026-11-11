# The only shipped document links to three files that are not in the package, and the rule-file format itself is documented nowhere

- severity: minor
- category: docs
- environment: Arch Linux cloud image, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 +
  aranicy-collector 0.3.0-1 (local/pacman), 2 vCPU / 1958 MiB / no display

## What I was trying to do

Write a rules file I can review, version and lint, without reading a source tree
(there is none on this machine). I need the rule schema — which keys exist, what
a `type` is, how a cgroup is defined, which files are read from
`/etc/ananicy.d`.

## Steps to reproduce

```sh
ls /usr/share/doc/aranicy/
ls /usr/share/doc/aranicy/docs
grep -n "COMPATIBILITY.md\|BENCHMARKS.md\|install.sh" /usr/share/doc/aranicy/README.md
```

Then try the most standard ananicy rule there is, a `type` reference, with no
type pack shipped:

```sh
rm -rf ~/repro3 && mkdir -p ~/repro3/rules ~/repro3/types
cp /etc/ananicy.d/ananicy.conf ~/repro3/ananicy.conf
printf '%s\n' '{ "name": "cc1plus", "type": "Heavy" }' > ~/repro3/rules/a.rules
aranicy --config-dir ~/repro3 lint; echo "exit=$?"
aranicy --config-dir ~/repro3 match-test cc1plus
```

## Expected

The package that `Provides: ananicy-cpp` and `Conflicts: ananicy-cpp` should let
me carry over an existing ananicy rules corpus and tell me what it accepts:
either the referenced documents are installed, or the README stops pointing at
them. Failing that, `aranicy lint` (or `--help`, or a man page) should describe
the keys, the file naming rule and the type/cgroup definition shape.

## Actual

`/usr/share/doc/aranicy/` contains only `README.md`, and every detailed
reference in it is a dead link: `docs/COMPATIBILITY.md`, `docs/BENCHMARKS.md`
and `contrib/install.sh` (including the documented `--uninstall` path) are not
in either package. The README's own "Compatibility surface" and "measurement
protocol" sections therefore cannot be read at all.

With no type pack shipped, the canonical ananicy rule `{ "name": "cc1plus",
"type": "Heavy" }` loads as an **empty policy**: the rule is counted, `nice`
stays untouched, and the only signal is a warning that says the rule "keeps its
own fields" — which is accurate but does not tell me what a type is or what the
type file should look like. I had to find the definition shape by trial and
error (`{"type": "Heavy", "nice": 19}` works, `{"name": "Heavy", "nice": 19}`
does not), and the same is true for cgroup definitions, whose only recognised key
turned out to be `cpu_quota_pct` (`cpuset` in a cgroup definition is rejected as
an unknown key).

## Evidence

```
$ ls /usr/share/doc/aranicy/
README.md
$ ls /usr/share/doc/aranicy/docs
ls: cannot access '/usr/share/doc/aranicy/docs': No such file or directory
$ grep -n "COMPATIBILITY.md\|BENCHMARKS.md\|install.sh" /usr/share/doc/aranicy/README.md
13:[docs/BENCHMARKS.md](docs/BENCHMARKS.md)). The measured Rust candidate
38:([docs/COMPATIBILITY.md](docs/COMPATIBILITY.md)).
46:[docs/BENCHMARKS.md](docs/BENCHMARKS.md) for the full measurement protocol
49:- Compatibility surface: [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md)
50:- Measurements and acceptance target: [docs/BENCHMARKS.md](docs/BENCHMARKS.md)
91:./contrib/install.sh                  # build if needed, install, enable --now
92:./contrib/install.sh --dry-run        # show actions only
93:./contrib/install.sh --keep-ananicy-cpp --no-enable
94:./contrib/install.sh --uninstall      # remove binary + unit, keep config
```

```
$ aranicy --config-dir ~/repro3 lint
warning: /home/arch/repro3/rules/a.rules:1: unknown type "Heavy" referenced; rule keeps its own fields
lint: warnings found
exit=1
$ aranicy --config-dir ~/repro3 match-test cc1plus
{
  "match": "exact",
  "name": "cc1plus",
  "policy": {}
}
exit=0
```

```
$ echo type-definition: { "type": "Heavy", "nice": 19 }
$ aranicy --config-dir ~/repro3 lint
OK: 1 rules, 1 types, 0 cgroups
exit=0
$ aranicy --config-dir ~/repro3 match-test cc1plus
{
  "match": "exact",
  "name": "cc1plus",
  "policy": {
    "nice": 19
  }
}
exit=0
$ echo alternative type-definition: { "name": "Heavy", "nice": 19 }
warning: /home/arch/repro3/rules/a.rules:1: unknown type "Heavy" referenced; rule keeps its own fields
lint: warnings found
exit=1
```

## What I could not determine

- Which keys are legal, beyond the ones I probed. Accepted: `name`,
  `name_regex`, `type`, `nice`, `ioclass`, `ionice`, `sched`, `cgroup`,
  `latency_nice`, `oom_score_adj`, `cpuset`. Rejected as unknown: `parent`,
  `cmdline`, `latnice`. There is no command that prints the schema and no
  document that lists it, so this list is what I happened to try.
- Whether the missing `docs/` and `contrib/` files exist somewhere else in the
  distribution. `pacman -Ql aranicy aranicy-collector` lists no other paths.
- Whether a rule that references a `type` inherits all of the type's fields or
  only those it does not set itself; no document says, and my rules never had a
  field collision to test it with.
