# `aranicy lint` exits 1 on a corpus that contains no `.rules` files, so a types/cgroups-only directory cannot be linted green — the workflow it blocks is linting each corpus directory separately in CI

- severity: minor
- category: enhancement
- environment: Arch Linux cloud image, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 +
  aranicy-collector 0.3.0-1 (build 2026-11-11 09:52 UTC), 2 vCPU / 1958 MiB / no display

## What I was trying to do

Keep the rule corpus in one directory per concern in git — `rules/`, `types/`,
`cgroups/` — and lint each of them in CI before it ships. `lint` is documented
as "Validate rules and report problems without running"; `OK: N rules, N types,
N cgroups` with exit 0 is what I gate on.

A directory that holds only `*.types` (or only `*.cgroups`) has nothing wrong
with it; a directory I have not filled in yet has nothing wrong with it either.
Both are reported as `lint: warnings found` with exit 1.

## Steps to reproduce

```sh
sudo pacman -U --noconfirm ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst \
                            ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst
mkdir -p ~/adv/typesonly
printf '%s\n' '{"type": "myt", "nice": 5}' > ~/adv/typesonly/a.types
cp /etc/ananicy.d/ananicy.conf ~/adv/typesonly/   # so the config warning is not involved
aranicy --config-dir ~/adv/typesonly lint; echo "exit=$?"
```

and the empty case:

```sh
mkdir -p ~/adv/empty; : > ~/adv/empty/a.rules
cp /etc/ananicy.d/ananicy.conf ~/adv/empty/
aranicy --config-dir ~/adv/empty lint; echo "exit=$?"
```

## Expected

`OK: 0 rules, 1 types, 0 cgroups`, exit 0, because every file in the directory
parses and nothing in it is malformed. This is an **enhancement request**, not a
contradiction of the shipped docs: `aranicy lint --help` states the policy
explicitly —

```
$ aranicy lint --help
Validate rules and report problems without running.

Exit 1 on any finding: an error (a rule that would not load) or a warning (a rule that loads but can never match, a file that is never read). A corpus that is green is a corpus that will produce policy.
```

— and my disagreement is with the last sentence: a `*.types` file is policy
input that a `*.rules` file can reference, so a types-only directory *can*
produce policy, and it is not possible to lint such a directory on its own.

## Actual

Exit 1, with a warning that is not about a malformed file, and a final line that
says `warnings found`:

```
warning: 0 rules loaded from /home/arch/adv/typesonly: this corpus will apply nothing
lint: warnings found
exit=1
```

The `types` file itself is loaded and valid — `dump types` proves it:

```
$ aranicy --config-dir ~/adv/typesonly dump types
{
  "myt": {
    "nice": 5
  }
}
exit=0
```

In a directory holding one empty `a.rules` the same thing happens, and the
`0 rules loaded` line is the only warning:

```
$ aranicy --config-dir ~/adv/empty lint
warning: config: config "/home/arch/adv/empty/ananicy.conf" not found; using defaults
warning: 0 rules loaded from /home/arch/adv/empty: this corpus will apply nothing
lint: warnings found
exit=1
```

The previous build of the same version does not do this — it prints no
`0 rules loaded` line and exits 0:

```
--- half
OK: 0 rules, 0 types, 0 cgroups
exit=0
```

so this is new in the current build (`differential.md`, check #3).

## Evidence

Commands and output as pasted above; the environment and both builds are
described in `differential.md`. The CI consequence I measured directly:

```
$ aranicy --config-dir ~/adv/typesonly lint; echo "exit=$?"
warning: 0 rules loaded from /home/arch/adv/typesonly: this corpus will apply nothing
lint: warnings found
exit=1
$ aranicy --config-dir ~/adv/good lint; echo "exit=$?"
OK: 2 rules, 0 types, 0 cgroups
exit=0
```

## What I could not determine

- Whether "a corpus that is green is a corpus that will produce policy" is meant
  to cover a types-only or cgroups-only directory. The daemon's startup warning
  (`WARN no rules loaded from /etc/ananicy.d: … this corpus will apply nothing`)
  is about the *combined* corpus; lint is being asked about one directory.
- Whether an option to separate "this will apply nothing" from "this is
  malformed" is planned. `aranicy lint --help` lists only `--config-dir`,
  `--config` and `--reload`; `--warn-only` is rejected:

  ```
  $ aranicy lint --warn-only
  error: unexpected argument '--warn-only' found
  Usage: aranicy lint [OPTIONS]
  exit=2
  ```
