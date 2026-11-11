# `aranicy lint` exits 1 on a rules-only checkout because the daemon config file is missing, and reports it at the same severity as a broken rule

- severity: minor
- category: behaviour
- environment: Arch Linux cloud image, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 +
  aranicy-collector 0.3.0-1 (local/pacman), 2 vCPU / 1958 MiB / no display

## What I was trying to do

Lint my rules directory in CI. The rules corpus is the artefact I keep in git; the
daemon's `ananicy.conf` is host configuration that the package owns. So CI runs
`aranicy --config-dir ./rules lint` against a directory that contains rules and
nothing else, and I want the exit code to tell me whether the rules are good.

`aranicy lint --help` promises exactly that: "Validate rules and report problems
without running (exit 1 on errors)".

## Steps to reproduce

```sh
rm -rf ~/repro2 && mkdir -p ~/repro2/rules
printf '%s\n' '{ "name": "x", "nice": 5 }' > ~/repro2/rules/a.rules
aranicy --config-dir ~/repro2 lint; echo "exit=$?"
```

Then break the rule and repeat:

```sh
printf '%s\n' '{ "name": "x", }' > ~/repro2/rules/a.rules
aranicy --config-dir ~/repro2 lint; echo "exit=$?"
```

## Expected

A rules-only directory whose rules are valid should lint clean with exit 0 (the
config defaults are used and said so in the message at most as information), and
a genuinely broken rule should look different from an absent config file — both
in wording and in the summary line.

## Actual

The valid rules-only directory exits 1 with "lint: warnings found" because
`~/repro2/ananicy.conf` does not exist. Once a rule is actually broken, the
output is the same shape: two `warning:` lines and the identical
`lint: warnings found` summary. A missing config file (a normal, benign state)
and a parse error (a real defect) are indistinguishable in kind and severity.

The practical consequence is that the only way to get a green exit code is to
copy the host's `ananicy.conf` into the linted tree — for the daemon config,
which is not what I am linting. As soon as someone works around this with `|| true`
or by shipping a stale copy of the config, the exit code stops meaning anything
(which is also what makes ren-1 dangerous).

## Evidence

```
$ aranicy lint --help | sed -n 1,3p
Validate rules and report problems without running (exit 1 on errors)

Usage: aranicy lint [OPTIONS]

$ aranicy --config-dir ~/repro2 lint
warning: config: config "/home/arch/repro2/ananicy.conf" not found; using defaults
lint: warnings found
exit=1

$ printf '%s\n' '{ "name": "x", }' > ~/repro2/rules/a.rules
$ aranicy --config-dir ~/repro2 lint
warning: config: config "/home/arch/repro2/ananicy.conf" not found; using defaults
warning: /home/arch/repro2/rules/a.rules:1: invalid JSON in span: trailing comma at line 1 column 16
lint: warnings found
exit=1
```

(Minor cosmetic note in the same output: the message reads `config: config "…"`,
the prefix applied twice.)

## What I could not determine

- Whether there is a supported way to say "use defaults, do not look for a config
  file" without copying one in. `--config` has no documented "none" value and
  there is no flag I could find; pointing `--config` at `/dev/null` is a guess I
  did not verify.
- Whether the maintainers intend warnings to be fatal. The help text says errors;
  the behaviour says any warning is fatal. I am reporting the mismatch, not
  deciding which side is right.
