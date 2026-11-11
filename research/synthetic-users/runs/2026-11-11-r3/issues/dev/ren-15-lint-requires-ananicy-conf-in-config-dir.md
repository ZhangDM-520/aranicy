# `aranicy lint --config-dir <rules>` exits 1 whenever that directory has no `ananicy.conf`, even though the config file is optional and the rules are valid

- severity: minor
- category: config
- environment: Arch Linux cloud image, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 +
  aranicy-collector 0.3.0-1 (build 2026-11-11 09:52 UTC), 2 vCPU / 1958 MiB / no display

## What I was trying to do

Lint a rules directory that is checked into git, in CI, without copying the
machine's `/etc/ananicy.d/ananicy.conf` next to it. `--config-dir` is the
documented way to point the tool at a corpus; the config file is optional by
contract (`$ANANICY_RS_CONF` → `$ANANICY_CPP_CONF` → `<rules-root>/ananicy.conf`,
and the daemon falls back to built-in defaults when it is absent).

I expected a missing, optional config file to be reported as a note, or not at
all, and the exit code to describe the corpus. It describes the environment.

## Steps to reproduce

```sh
sudo pacman -U --noconfirm ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst \
                            ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst
mkdir -p ~/adv/good
printf '%s\n' '{"name": "renapp", "nice": 7, "ioclass": "idle"}' \
              '{"name_regex": "^renre.*", "nice": 6}' > ~/adv/good/zz-test.rules
aranicy --config-dir ~/adv/good lint; echo "exit=$?"
cp /etc/ananicy.d/ananicy.conf ~/adv/good/
aranicy --config-dir ~/adv/good lint; echo "exit=$?"
aranicy --config-dir ~/adv/good --config /etc/ananicy.d/ananicy.conf lint; echo "exit=$?"
```

## Expected

First invocation: `OK: 2 rules, 0 types, 0 cgroups`, exit 0, or exit 0 with a
note that defaults were used. Second invocation: the same, since the rule file
did not change.

This one *is* a contradiction of the shipped documentation. `aranicy lint
--help` defines the exit code:

```
$ aranicy lint --help
Exit 1 on any finding: an error (a rule that would not load) or a warning (a rule that loads but can never match, a file that is never read). A corpus that is green is a corpus that will produce policy.
```

A config file that is not there is neither "an error (a rule that would not
load)" nor "a file that is never read" — and the config is optional by contract,
with built-in defaults, which the daemon itself demonstrates.

## Actual

Adding or removing an optional file changes the exit code of a check about rule
files:

```
$ aranicy --config-dir ~/adv/good lint          # rules only
warning: config: config "/home/arch/adv/good/ananicy.conf" not found; using defaults
lint: warnings found
exit=1
$ cp /etc/ananicy.d/ananicy.conf ~/adv/good/
$ aranicy --config-dir ~/adv/good lint
OK: 2 rules, 0 types, 0 cgroups
exit=0
```

The same rules that are "OK" with a config beside them are "warnings found"
without it. There is no `--config` I can pass to say "use the system config",
except by naming the absolute path — and even then the directory warning is
produced first:

```
$ aranicy --config-dir ~/adv/good --config /etc/ananicy.d/ananicy.conf lint
OK: 2 rules, 0 types, 0 cgroups
exit=0
```

(that works, but it means every CI job must know where the config lives, or
ship one, to lint a rule file that does not depend on it.)

## Evidence

As pasted above. The warning text is the same in the previous build of the same
version (`differential.md`, check #5), so this is not a regression — it is a
long-standing exit-code policy I am now reporting because it breaks the
"lint the rules repo in CI" workflow the tool otherwise supports well.

## What I could not determine

- Whether the intent is "a corpus without a config is incomplete". The shipped
  README says the package ships no rules and that you point `ANANICY_CPP_CONFDIR`
  at yours — which describes exactly the config-less directory I am linting.
- What the intended exit-code policy is. The daemon hits the identical missing
  file and treats it as a warning and carries on:

  ```
  $ sudo mv /etc/ananicy.d/ananicy.conf /root/ananicy.conf.bak
  $ sudo systemctl restart aranicy; sleep 5
  $ systemctl is-active aranicy
  active
  $ sudo journalctl -u aranicy -b --no-pager -o cat | tail -5
  Starting aranicy - auto-nice daemon...
  2026-11-11T10:29:54.856618Z  WARN config: config "/etc/ananicy.d/ananicy.conf" not found; using defaults
  2026-11-11T10:29:54.856637Z  WARN kernel does not support latency_nice (sched_setattr LATENCY_NICE); latency_nice policy will be skipped
  2026-11-11T10:29:54.856787Z  INFO bias: restored history entries=43
  Started aranicy - auto-nice daemon.
  ```

  so the two entry points disagree about whether this condition is tolerable.
