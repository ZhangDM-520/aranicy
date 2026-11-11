# No command reports which rule file produced a decision, so a rules corpus cannot be audited from the tool's output

- severity: minor
- category: enhancement
- environment: Arch Linux cloud image, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 +
  aranicy-collector 0.3.0-1 (local/pacman), 2 vCPU / 1958 MiB / no display

## What I was trying to do

Workflow this blocks: keep the rules in a git repository, review a diff that
changes a priority, and later answer "why does this process have this nice?" —
from the tool's own output, without the corpus open in an editor. I can answer
"which name matched and what the resulting policy is"; I cannot answer "which
file and line said so", which is the question a code review asks.

## Steps to reproduce

```sh
rm -rf ~/repro && mkdir -p ~/repro/rules && cp /etc/ananicy.d/ananicy.conf ~/repro/ananicy.conf
printf '%s\n' '{ "name": "zzsleep", "nice": 15 }' > ~/repro/rules/10-a.rules
printf '%s\n' '{ "name": "zzsleep", "nice": 2 }'  > ~/repro/rules/90-z.rules
aranicy --config-dir ~/repro dump rules
aranicy --config-dir ~/repro match-test zzsleep
aranicy --help            # any subcommand whose output names a source file?
```

## Expected

`dump rules` (or `match-test`) naming the originating files (`10-a.rules:1`,
`90-z.rules:1`), and, for the duplicate case, saying which definition is in
effect. `dump proc` naming, per pid, the rule (or the bias entry) that produced
the current nice.

## Actual

`dump rules` is a flat list of match/policy objects with no provenance, and
`match-test` stops at the merged policy. The duplicate silently resolves to the
later file, so the only way to know which of two contradictory lines applies is
to delete one and re-run.

```
$ aranicy --config-dir ~/repro dump rules
{
  "exact": [
    {
      "match": "exact",
      "name": "zzsleep",
      "nice": 2
    }
  ],
  "regex": []
}
$ aranicy --config-dir ~/repro match-test zzsleep
{
  "match": "exact",
  "name": "zzsleep",
  "policy": {
    "nice": 2
  }
}
```

`dump proc` is a live `/proc` reflection only (`name`, `nice`, `pid`), so it
cannot say whether a nice came from a rule, from `bias`, or from the process
setting it itself — which matters as soon as the bias engine starts moving
things (see ren-7 for an example where PID 1's nice came from bias).

## Evidence

```
$ aranicy dump rules
{
  "exact": [
    {
      "match": "exact",
      "name": "zzsleep",
      "nice": 2
    }
  ],
  "regex": []
}
$ aranicy match-test cc1plus
{
  "match": "exact",
  "name": "cc1plus",
  "policy": {}
}
$ aranicy dump proc | head -8
[
  {
    "name": "systemd",
    "nice": 1,
    "pid": 1
  },
```

(That last one is a nice that no rule set: the daemon's own bias produced it, and
nothing in the output says so.)

## What I could not determine

- Whether the daemon keeps provenance internally and only omits it from `dump`.
  `dump rules` has no field for it, so from the outside the information is simply
  absent.
- Whether the bias contribution for a running process can be inspected anywhere
  other than the aggregate `/var/lib/aranicy/bias-state` name list; `dump proc`
  shows the final nice only.
