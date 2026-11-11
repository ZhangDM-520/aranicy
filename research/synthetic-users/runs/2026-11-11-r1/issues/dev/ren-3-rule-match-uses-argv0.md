# Rules are matched against basename(argv[0]), so `exec -a` both evades a rule and fabricates one — the opposite of what the README says it protects against

- severity: major
- category: behaviour
- environment: Arch Linux cloud image, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 +
  aranicy-collector 0.3.0-1 (local/pacman), 2 vCPU / 1958 MiB / no display

## What I was trying to do

Write rules for processes and know they will be applied to the process I meant.
I also need the reverse: when something is wrong with a build machine, I read
`ps`/`/proc` and expect the name I see to be the name the policy used, so I can
reproduce a decision by hand.

## Steps to reproduce

Install a single rule that is easy to recognise, then run one binary twice with
two different `argv[0]`s.

```sh
sudo tee /etc/ananicy.d/rules/70-argvdemo.rules >/dev/null <<'EOF'
{ "name": "argvdemo", "nice": 14 }
EOF
sudo systemctl reload aranicy; sleep 1
cp /bin/sleep ~/argvdemo

# (1) argv[0] says argvdemo, the executable is /bin/sleep
bash -c 'exec -a argvdemo /bin/sleep 20' & pid=$!
sleep 1; tr '\0' ' ' < /proc/$pid/cmdline; cat /proc/$pid/comm; awk '{print $19}' /proc/$pid/stat

# (2) the executable is named argvdemo, argv[0] says something else
bash -c 'exec -a innocuous ~/argvdemo 20' & pid=$!
sleep 1; tr '\0' ' ' < /proc/$pid/cmdline; cat /proc/$pid/comm; awk '{print $19}' /proc/$pid/stat
```

## Expected

Both cases are the same question — "is this process the one my rule names?" — and
the package's own documentation says the answer comes from `cmdline`
(basename of `argv[0]`), so I expect a rule match in (1) and no match in (2), which
is what happens. What I did not expect is that this makes the name **free data
supplied by the process**: the README justifies skipping `comm` with "`exec -a`
spoofing makes comm-only filtering unsafe", yet `exec -a` sets exactly the field
the daemon does trust.

A rule match should not be settable by the process being matched, or, if it is,
the documentation should not claim the design is spoofing-resistant.

## Actual

(a) A `/bin/sleep` that claims the name `argvdemo` is niced to 14 — any process
can borrow any rule, including one that lowers priority (a negative `nice` would
give it *more* CPU than it is entitled to).

(b) A binary that really is named `argvdemo` gets nothing as soon as it rewrites
`argv[0]` — so a rule silently stops applying to the process it was written for.
This is not exotic: daemons that set a descriptive `argv[0]` (`postgres:
checkpointer`, `nginx: worker process`, `qmgr -l -t unix`) would never match their
own names.

## Evidence

```
$ ~/bench/repro3.sh 2>&1
(1) the real binary, argv[0] rewritten:
  argv: argvdemo 20 
  comm: sleep
  nice: 14
(2) the binary is named argvdemo, argv[0] rewritten to something else:
  argv: innocuous 20 
  comm: argvdemo
  nice: 0
```

(the script exits 143 because it kills the demo process at the end of each case)

The rule that produced this:

```
{ "name": "argvdemo", "nice": 14 }
```

The shipped documentation, for comparison:

```
$ grep -n "exec -a\|basename(argv" /usr/share/doc/aranicy/README.md
23:`/proc/<pid>/cmdline` read (no comm prescreen — `exec -a` spoofing makes

$ aranicy match-test --help | sed -n 1,6p
Show which rule a process name would match, without applying anything

Usage: aranicy match-test [OPTIONS] <NAME>

Arguments:
  <NAME>  Process name as the daemon derives it (basename(argv[0]))
```

I also checked the two-way case directly by renaming a copy of `/bin/sleep`
(`comm` = `sleep`, `argv[0]` = `/home/arch/zzsleep|15|`), which got the
`zzsleep` rule's `nice=12`; and `exec -a innocent` on an executable whose `comm`
was still `zzsleep`, which got `nice=0`.

## What I could not determine

- Whether matching on `comm` would be better than `argv[0]` for this use case —
  that is a design question, not something I can settle from the outside. I am
  reporting that the shipped rationale (avoid `comm` because `exec -a` spoofs it)
  is contradicted by the field the daemon actually reads.
- Whether there is a config key to switch the match field. I looked for one in
  the shipped config and in `--help` and found none.
- Whether `name_regex` has the same property. It matches the same derived name,
  so I expect so, but I only tested exact names.
