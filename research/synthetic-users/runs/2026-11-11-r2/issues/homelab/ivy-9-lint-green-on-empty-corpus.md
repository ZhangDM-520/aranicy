# `aranicy lint` exits 0 on a corpus with zero rules, although its own help says a green corpus is one that will produce policy

- severity: minor
- category: behaviour
- environment: Linux 7.2.11-arch1-1 (Arch rolling, cloud image), aranicy 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Gate my Ansible play on the corpus: deploy the rule files, then run `aranicy
lint` and fail the play if it says anything. The corpus lives in a git checkout
that the play renders into a directory, so there is a real chance of deploying
an empty directory (a template that rendered nothing, a checkout that failed,
a `copy` from the wrong path). That is exactly the case I need the gate to
catch.

## Steps to reproduce

```
sudo pacman -U --noconfirm ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst \
                         ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst
mkdir -p ~/emptyrules
cp /etc/ananicy.d/ananicy.conf ~/emptyrules/ananicy.conf
aranicy lint --config-dir ~/emptyrules; echo "exit=$?"
```

## Expected

A non-zero exit (or a visible warning), because a corpus with zero rules is not
a corpus that produces policy. The tool's own first paragraph says so.

## Actual

`OK: 0 rules, 0 types, 0 cgroups`, exit 0. The play passes, `systemctl restart
aranicy` succeeds, and the box runs with no policy at all. The daemon *does*
warn at startup for this exact state, so the information exists on one side and
not the other.

## Evidence

```
$ aranicy lint --help
Validate rules and report problems without running.

Exit 1 on any finding: an error (a rule that would not load) or a warning (a rule that loads but can never match, a file that is never read). A corpus that is green is a corpus that will produce policy.
...

$ mkdir -p ~/emptyrules; cp /etc/ananicy.d/ananicy.conf ~/emptyrules/ananicy.conf
$ aranicy lint --config-dir ~/emptyrules; echo "exit=$?"
OK: 0 rules, 0 types, 0 cgroups
exit=0

# the daemon, started from exactly that directory, warns:
$ sudo journalctl -u aranicy -b -o cat | grep -i "no rules"
2026-11-11T09:30:25.942857Z  WARN no rules loaded from /etc/ananicy.d: with an empty rule set only the learned bias changes priorities. The package ships no rule corpus - point ANANICY_CPP_CONFDIR at yours, or write files ending in .rules
```

## What I could not determine

- Whether `lint` is deliberately "validate what is there" rather than "validate
  that policy will exist". The help text I quoted says otherwise, which is why I
  filed this rather than an enhancement.
- Whether an empty corpus is supposed to be a warning at all, given that the
  daemon already warns at startup and the warning would then fire twice.
