# Retest of the five round-1 reports

Machine: `su-homelab-r2`, Linux 7.2.11-arch1-1, Arch rolling, 2 vCPU, 1958 MiB.
Packages: `aranicy 0.3.0-1` + `aranicy-collector 0.3.0-1` (same filenames as round
1, different build: the daemon package is now a split package with install
scripts, and now ships `BENCHMARKS.md`/`COMPATIBILITY.md`).

```
$ pacman -Q aranicy aranicy-collector
aranicy 0.3.0-1
aranicy-collector 0.3.0-1
$ uname -r
7.2.11-arch1-1
```

Verdict per report: **gone** (fixed), **different** (the specific defect is gone
but the same workflow still has a hole), **still happens**.

---

## 1. `pacman -R` leaves both daemons running and still changing process priorities — **gone**

Both packages now carry `.INSTALL` scripts that stop and disable their unit
before the files go away:

```
$ bsdtar -xOf aranicy-0.3.0-1-x86_64.pkg.tar.zst .INSTALL
post_upgrade() {
    systemctl --quiet daemon-reload 2>/dev/null || true
}
pre_remove() {
    systemctl --quiet disable --now aranicy.service 2>/dev/null || true
}
post_remove() {
    systemctl --quiet daemon-reload 2>/dev/null || true
}
```

Scenario re-run exactly (install → enable → observable rule → live process →
remove without stopping → spawn again):

```
$ printf '%s\n' '{"name": "sleep", "nice": 5}' | sudo tee /etc/ananicy.d/05-sleep.rules
{"name": "sleep", "nice": 5}
$ sudo systemctl reload aranicy; sleep 2
$ setsid sleep 900 >/dev/null 2>&1 & sleep 3; p1=$(pgrep -nx sleep)
PRE pid=507 nice=  5
units: active active
$ sudo pacman -R --noconfirm aranicy aranicy-collector
...
removing aranicy...
warning: /etc/ananicy.d/ananicy.conf saved as /etc/ananicy.d/ananicy.conf.pacsave
removing aranicy-collector...
...
REMOVE_EXIT=0
LOADSTATE=not-found ACTIVE=inactive
collector LOADSTATE=not-found ACTIVE=inactive
--- pgrep ---
(end pgrep)
--- symlink ---
ls: cannot access '/etc/systemd/system/multi-user.target.wants/aranicy.service': No such file or directory
--- binaries ---
ls: cannot access '/usr/bin/aranicy': No such file or directory
ls: cannot access '/usr/bin/aranicy-collectord': No such file or directory

$ setsid sleep 901 >/dev/null 2>&1 & sleep 3; p2=$(pgrep -nx sleep)
POST pid=788 nice=  0
```

Both units inactive with `LoadState=not-found`, no process alive, enablement
symlink gone, and a process started after the removal keeps nice 0. The
round-1 blocker is fixed.

Residual, filed separately as `issues/ivy-6`: `/var/lib/aranicy/bias-state`
survives the removal and re-applies learned nice deltas on the next install.

---

## 2. A fresh install runs with zero rules and says nothing about it — **different**

The empty case is now loud at startup:

```
$ ls /etc/ananicy.d/
ananicy.conf
$ sudo systemctl enable --now aranicy; sleep 5
$ sudo journalctl -u aranicy -u aranicy-collector --since "-30 seconds" --no-pager -o cat
Started aranicy collector - scheduler-agnostic BPF task statistics.
Starting aranicy - auto-nice daemon...
2026-11-11T09:06:37.080368Z  WARN no rules loaded from /etc/ananicy.d: with an empty rule set only the learned bias changes priorities. The package ships no rule corpus - point ANANICY_CPP_CONFDIR at yours, or write files ending in .rules
2026-11-11T09:06:37.080389Z  WARN kernel does not support latency_nice (sched_setattr LATENCY_NICE); latency_nice policy will be skipped
...
Started aranicy - auto-nice daemon.
```

The other half of the old expectation — "states what it loaded, at minimum the
number of rules" — still does not happen when rules *are* present:

```
$ printf '%s\n' '{"name": "sleep", "nice": 5}' | sudo tee /etc/ananicy.d/00-test.rules
$ sudo systemctl restart aranicy; sleep 3
$ sudo journalctl -u aranicy --since "-15 seconds" --no-pager -o cat | head -20
Starting aranicy - auto-nice daemon...
2026-11-11T09:06:46.062251Z  WARN kernel does not support latency_nice (sched_setattr LATENCY_NICE); latency_nice policy will be skipped
2026-11-11T09:06:46.062545Z  INFO bias: restored history entries=21
Started aranicy - auto-nice daemon.
2026-11-11T09:06:46.063237Z  INFO stats sampler live prev=degraded: starting detail=bound SchedSample via sched_samples at /run/aranicy/collector-stats.sock
```

`systemctl status aranicy` shows the same nothing:

```
$ systemctl status aranicy --no-pager | sed -n "1,14p"
● aranicy.service - aranicy - auto-nice daemon
     Loaded: loaded (/usr/lib/systemd/system/aranicy.service; enabled; preset: disabled)
     Active: active (running) since Wed 2026-11-11 09:29:17 UTC; 9s ago
 Invocation: 8999762f9d154b64842cb88e9ace04e0
       Docs: https://github.com/ZhangDM-520/aranicy
    Process: 4170 ExecReload=/usr/bin/aranicy --reload (code=exited, status=0/SUCCESS)
   Main PID: 4148 (aranicy)
      Tasks: 5 (limit: 2276)
     Memory: 1.4M (peak: 3.4M)
        CPU: 35ms
     CGroup: /system.slice/aranicy.service
             └─4148 /usr/bin/aranicy start
```

Only `reload` prints a count (`INFO reloaded 1 rules; bias reconfigured`). So an
operator can now tell "zero rules" from "some rules", but not "1 rule" from
"400 rules" without running `aranicy dump rules`.

---

## 3. `aranicy lint` exits 1 when it only found warnings — **different**

The help text was rewritten to match the behaviour instead of the other way
round:

```
$ aranicy lint --help
Validate rules and report problems without running.

Exit 1 on any finding: an error (a rule that would not load) or a warning (a rule that loads but can never match, a file that is never read). A corpus that is green is a corpus that will produce policy.
...
```

So the specific contradiction I reported (docs promised "exit 1 on errors") is
gone. The workflow it broke is not: any `--config-dir` corpus that does not
contain its own `ananicy.conf` still exits 1 for a reason that has nothing to
do with the rules.

```
$ mkdir -p ~/testrules && printf '%s\n' '{"name": "sleep", "nice": 5}' > ~/testrules/00-test.rules
$ aranicy lint --config-dir ~/testrules; echo "exit=$?"
warning: config: config "/home/arch/testrules/ananicy.conf" not found; using defaults
lint: warnings found
exit=1

$ cp /etc/ananicy.d/ananicy.conf ~/emptyrules/ananicy.conf
$ aranicy lint --config-dir ~/emptyrules; echo "exit=$?"
OK: 0 rules, 0 types, 0 cgroups
exit=0
```

Same corpus, one file added to the directory, exit flips from 1 to 0 — the
warning is about the directory layout, not the corpus. A script still cannot use the exit status as
"this corpus is broken" while it also cannot use it as "this corpus is fine"
(the empty corpus is green, see `issues/ivy-9`).

---

## 4. The installed documentation is the wrong document for one package and points at files that are not installed — **different**

The "wrong document" half is gone: the two READMEs are now different files, and
`COMPATIBILITY.md`/`BENCHMARKS.md` are shipped.

```
$ sha256sum /usr/share/doc/aranicy/README.md /usr/share/doc/aranicy-collector/README.md
6f4a5080c4f85358e452496f1bae29b460779252f5015b12b9af85ec1a96e95c  /usr/share/doc/aranicy/README.md
e53c726d1a0c28c03c6b18aee0a7ca98d7d86753918e2bc58daf466afdab1f60  /usr/share/doc/aranicy-collector/README.md
$ find /usr/share/doc/aranicy /usr/share/doc/aranicy-collector -type f | sort
/usr/share/doc/aranicy-collector/README.md
/usr/share/doc/aranicy/BENCHMARKS.md
/usr/share/doc/aranicy/COMPATIBILITY.md
/usr/share/doc/aranicy/README.md
```

The links are still dead, because the files were installed without the
directory structure the README links against:

```
$ grep -oE "\]\([^)]*\)" /usr/share/doc/aranicy/README.md | sort -u
](LICENSE)
](docs/BENCHMARKS.md)
](docs/COMPATIBILITY.md)
](tools/storm/)
$ for f in /usr/share/doc/aranicy/docs/BENCHMARKS.md /usr/share/doc/aranicy/docs/COMPATIBILITY.md /usr/share/doc/aranicy/tools/storm /usr/share/doc/aranicy/banner.svg /usr/share/doc/aranicy/LICENSE; do printf "%s -> %s\n" "$f" "$(test -e $f && echo present || echo MISSING)"; done
/usr/share/doc/aranicy/docs/BENCHMARKS.md -> MISSING
/usr/share/doc/aranicy/docs/COMPATIBILITY.md -> MISSING
/usr/share/doc/aranicy/tools/storm -> MISSING
/usr/share/doc/aranicy/banner.svg -> MISSING
/usr/share/doc/aranicy/LICENSE -> MISSING
```

Five of five relative links resolve to nothing. The documents are now present,
just not where the document that references them says they are
(`/usr/share/licenses/aranicy/LICENSE` is where the licence lives).

The collector README is now its own document and does describe the sockets,
cadence and privileges.

---

## 5. Learned bias changes the nice value of processes no rule names, and there is no config key to turn it off — **gone**

`bias_enable` now exists, is documented, and is honoured:

```
$ sed -n "56,60p" /usr/share/doc/aranicy/COMPATIBILITY.md
shipped as its own package (`aranicy-collector`, GPL-2.0-only). aranicy is a
pure client: it loads no BPF, owns no scheduler dependency, and therefore
learns on *every* scheduler, including none (plain EEVDF). Disable with
`bias_enable = false`.
```

Evidence that it stops actuation rather than merely stopping learning — an
existing on-disk bias of 3 was present for `openssl` at the time:

```
$ sudo python3 -c "import json;d=json.load(open('/var/lib/aranicy/bias-state'));print([e for e in d['entries'] if e['name']=='openssl'])"
[{'name': 'openssl', 'score_ema': 15.47675873527012, 'sample_count_ewma': 216.0456548671195, 'class': 'bulk', 'bias': 3, 'last_seen': 1794388780, 'confidence': 0.24655817612333975, 'frozen': False}]

$ printf 'bias_enable = false\n' | sudo tee -a /etc/ananicy.d/ananicy.conf
$ sudo systemctl restart aranicy; sleep 4
2026-11-11T09:10:15.345297Z  INFO bias: restored history entries=55

$ setsid timeout 90 openssl speed -seconds 90 -multi 2 sha256 >/dev/null 2>&1 & sleep 6
   1426   0 openssl
   1427   0 openssl
   1428   0 openssl
--- at +46s ---
   1426   0 openssl
   1427   0 openssl
   1428   0 openssl
```

and a rule applied under the same setting is applied exactly, not
`rule_nice + bias`:

```
$ printf '%s\n' '{"name": "openssl", "nice": 5}' | sudo tee /etc/ananicy.d/10-openssl.rules
$ sudo systemctl restart aranicy; sleep 4
$ setsid timeout 40 openssl speed -seconds 40 -multi 2 sha256 >/dev/null 2>&1 & sleep 6
   1470   5 openssl
   1471   5 openssl
   1472   5 openssl
```

It is a restart-only key and says so instead of pretending:

```
$ sudo systemctl reload aranicy
2026-11-11T09:19:55.642839Z  INFO reloaded 3 rules; bias reconfigured
2026-11-11T09:19:55.642858Z  WARN restart-only keys ignored: bias_enable
```

With the default (`bias_enable` unset = on) the round-1 observation still
reproduces for the record, including the roughly 30 s lag between the bias
step and the live nice:

```
$ sudo journalctl -u aranicy --no-pager -o cat | grep "bias changed"
2026-11-11T09:08:16.063521Z  INFO bias changed name=openssl bias=1 prev=0 class=Bulk cause=Evidence
2026-11-11T09:08:46.062989Z  INFO bias changed name=openssl bias=2 prev=1 class=Bulk cause=Evidence
2026-11-11T09:09:16.063593Z  INFO bias changed name=openssl bias=3 prev=2 class=Bulk cause=Evidence
$ (polling every 10 s while the above ran)
09:08:48 2 2 2
09:09:49 3 3 3
```

The knob I asked for exists; the behaviour it controls is unchanged.
