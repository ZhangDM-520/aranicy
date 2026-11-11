# `aranicy lint` exits 1 when it only found warnings, although its help text promises exit 1 only on errors

- severity: minor
- category: behaviour
- environment: Linux 7.2.11-arch1-1 (Arch rolling, cloud image), aranicy 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Wire rule validation into my Ansible run: deploy the corpus, then run
`aranicy lint` and fail the play only when the corpus is *wrong*, not when it
is merely unusual. I checked the help text first and built the task around it.

## Steps to reproduce

```
mkdir -p ~/testrules
printf '%s\n' '{"name": "sleep", "nice": 5}' > ~/testrules/00-test.rules
aranicy lint --config-dir ~/testrules; echo "exit=$?"
```

## Expected

The help text says "Validate rules and report problems without running
(exit 1 on errors)". With one valid rule and no errors, I expect exit 0 and
the warnings printed; a non-zero exit should mean "this corpus is broken".

## Actual

Exit status 1 with the summary line `lint: warnings found`. The only warning
is that the per-directory config file is absent — which is the normal state
for anyone using `--config-dir` or `ANANICY_CPP_CONFDIR` to point at a rules
directory that is not `/etc/ananicy.d`. That makes the documented "exit 1 on
errors" check useless as a gate: every such lint run fails.

## Evidence

```
$ aranicy lint --help | head -1
Validate rules and report problems without running (exit 1 on errors)

$ aranicy lint --config-dir ~/testrules; echo "exit=$?"
warning: config: config "/home/arch/testrules/ananicy.conf" not found; using defaults
lint: warnings found
exit=1

# same binary, default directory, four valid rules, no warnings:
$ aranicy lint; echo "exit=$?"
OK: 4 rules, 0 types, 0 cgroups
exit=0
```

The exit status is not, however, sensitive only to warnings: a genuinely
broken corpus is also exit 1 with a per-line diagnostic, so the two cases are
indistinguishable to a script:

```
$ aranicy lint
warning: /etc/ananicy.d/99-bad.rules:1: nice: 999 out of range -20..=19
warning: /etc/ananicy.d/99-bad.rules:2: invalid JSON in span: key must be a string at line 1 column 3
lint: warnings found
LINT_EXIT=1
```

(That run also loaded 4 other valid rules, so `--config-dir` was fine; the
broken file was mine and was removed afterwards.)

## What I could not determine

- Whether "warnings are failures" is a deliberate choice for CI use. If it is,
  the help text is what is wrong and the `--config-dir`-without-config case
  needs to stop being a warning.
- Whether there is an undocumented flag to make warnings non-fatal. `--help`
  for `lint` lists only the three global options (`--config-dir`, `--config`,
  `--reload`), so I found none.
