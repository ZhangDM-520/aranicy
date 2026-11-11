# Round 2 — retest of last session's five reports

Machine: fresh `su-gamer-r2`, Arch Linux rolling, kernel `7.2.11-arch1-1`, 2 vCPU,
1958 MiB, `aranicy 0.3.0-1` + `aranicy-collector 0.3.0-1` from `~/payload`
(newer build than round 1, same package version string).

All commands below are prefixed `./machine` in my notes; that is my ssh wrapper
into the box. Everything was run on the box.

Summary:

| # | report | verdict |
|---|---|---|
| 1 | daemon re-nices PID 1 / journald by itself | **gone** |
| 2 | learned bias overrides my rule / nices unruled names / no per-executable opt-out | **different** (still overrides; an opt-out exists now but is keyed the wrong way — new issue kaz-6) |
| 3 | packaged install has no quick start | **gone** |
| 4 | collector ships the daemon README, dead links | **different** (README now real; links still dead, still no man page) |
| 5 | no per-process status | **still happens** (mitigated, not fixed) |

---

## 1. `kaz-1-daemon-renices-pid1-and-journald` — gone

Same scenario as last time: install, enable, write two rules (`kazgame`, `openssl`),
leave it running with a CPU-bound workload plus my shell commands, then audit
who got touched.

```
$ ./machine 'sudo systemctl enable --now aranicy; sleep 4; systemctl is-enabled aranicy; systemctl is-active aranicy; systemctl is-active aranicy-collector'
enabled
active
active
```

After ~20 minutes of that, the only `applied rule` lines in the journal name
things I wrote rules for:

```
$ ./machine 'sudo journalctl -u aranicy --no-pager | grep "applied rule" | sed -E "s/^.{0,60}applied/ applied/"'
 applied rule rule="kazdemo" pid=1364 bias=0 route="exec"
 applied rule rule="kazgame" pid=1241 bias=2 route="drift"
 applied rule rule="kazdemo" pid=1339 bias=0 route="drift"
 applied rule rule="kazgame" pid=1241 bias=0 route="drift"
 applied rule rule="systemd" pid=931 bias=0 route="drift"
 applied rule rule="kazdemo2" pid=1476 bias=0 route="drift"
 applied rule rule="systemd" pid=931 bias=0 route="drift"
 applied rule rule="kazsleep3" pid=1627 bias=0 route="exec"
 applied rule rule="kazsleep9" pid=1640 bias=0 route="exec"
 applied rule rule="kazsleep9" pid=1678 bias=0 route="exec"
 applied rule rule="kazgame" pid=1851 bias=0 route="exec"
 applied rule rule="kazauto" pid=1894 bias=0 route="exec"
 applied rule rule="kazcpu" pid=1921 bias=0 route="exec"
 applied rule rule="kazdemo2" pid=1476 bias=0 route="drift"
 applied rule rule="kazauto" pid=1868 bias=0 route="drift"
 applied rule rule="kazgame" pid=1851 bias=1 route="drift"
 applied rule rule="kazbulk" pid=2022 bias=2 route="drift"
 applied rule rule="kazgame" pid=1851 bias=3 route="drift"
 applied rule rule="kazbulk" pid=2022 bias=3 route="drift"
 applied rule rule="kazgame" pid=1851 bias=0 route="drift"
 applied rule rule="kaztype" pid=2193 bias=0 route="exec"
 applied rule rule="kazdrop" pid=2233 bias=0 route="exec"
 applied rule rule="kazgame" pid=1241 bias=0 route="drift"
 applied rule rule="kazgame" pid=1851 bias=1 route="drift"
 applied rule rule="kazbulk" pid=2022 bias=2 route="drift"
 applied rule rule="kazgame" pid=1851 bias=3 route="drift"
 applied rule rule="kazbulk" pid=2022 bias=3 route="drift"
```

(the `systemd` lines at 09:11 and 09:12 are a rule **I** wrote on purpose in the
test below, not the daemon acting alone.)

PID 1 and journald are untouched, 20 minutes in:

```
$ ./machine 'ps -o pid,ni,comm -p 1; J=$(ps -eo pid,comm | awk "\$2==\"systemd-journal\"{print \$1; exit}"); S=$(ps -eo pid,comm | awk "\$2 ~ /^sshd/{print \$1; exit}"); ps -o pid,ni,comm -p $J,$S'
    PID  NI COMMAND
      1   0 systemd
    PID  NI COMMAND
    203   0 systemd-journal
    747   0 sshd
```

The learned axis did not even build a useful entry for them. At the 12-minute
mark the only name whose bias had moved at all was my own CPU-bound stand-in:

```
$ ./machine 'sudo journalctl -u aranicy --no-pager | grep "bias changed" | sed -E "s/.*name=([^ ]+) bias=([0-9-]+).*/\1 \2/" | awk "{if (\$2+0 > m[\$1]+0) m[\$1]=\$2} END {for (n in m) print m[n], n}" | sort -rn | head'
3 kazgame
```

`dump bias` shows entries for `systemd` and `sshd-session` (among others) all at
`bias 0`:

```
$ ./machine 'aranicy dump bias 2>/dev/null | python3 -c "import json,sys; s=json.load(sys.stdin)[\"state\"]; [print(e[\"name\"], e[\"bias\"]) for e in (s or {}).get(\"entries\",[]) if e[\"name\"] in (\"kazgame\",\"systemd\",\"sshd-session\")]"'
kazgame 3
sshd-session 0
systemd 0
```

### The deliberate-rule probe (new this round)

Last session I could not tell whether "PID 1 is never a target" was a rule or an
accident. I wrote a rule for it:

```
$ ./machine 'printf "%s\n" "{\"name\": \"systemd\", \"nice\": 10}" | sudo tee /etc/ananicy.d/80-pid1.rules; echo "before: PID1=$(ps -o ni= -p 1)"; sudo systemctl reload aranicy; sleep 70; ps -o pid,ni,comm -p 1,931'
{"name": "systemd", "nice": 10}
before: PID1=  0
    PID  NI COMMAND
      1   0 systemd
    931  10 systemd
```

PID 1 stays at 0 even with an explicit rule, which the shipped
`COMPATIBILITY.md` says it will ("**PID 1 is never a target**, rule or no rule:
a policy for it always asks for the kernel default (0)"). That part is honest and
it works.

But note `pid 931`: `/usr/lib/systemd/systemd --user`, my **session manager**,
got nice 10 from the same rule. The "PID 1 is never a target" protection is
literally about PID 1, and a rule named `systemd` silently lands on the per-user
manager as well:

```
$ ./machine 'ps -o pid,ni,comm,args -p 931'
    PID  NI COMMAND         COMMAND
    931  10 systemd         /usr/lib/systemd/systemd --user
```

Removing the rule put it back at 0 within one drift pass, so this is
recoverable, and it was my rule. I am not filing it as a bug; I am recording it
because "it can't touch my session daemon" is only true for init, and nothing
warned me that my rule hit the session manager.

---

## 2. `kaz-2-bias-overrides-rule-and-nices-unruled-processes` — different

Three claims to retest: (a) the bias overrides the nice I wrote, (b) names with
no rule get biased, (c) there is no per-executable opt-out.

### (a) The bias still overrides my rule. Same as last time.

Rule `{"name": "kazgame", "nice": -5}`, CPU-bound process started, nothing else
touched:

```
$ ./machine 'ps -o pid,ni,comm -C kazgame; sudo journalctl -u aranicy --no-pager | grep -E "bias changed name=kazgame|rule=\"kazgame\"" | head -4'
    PID  NI COMMAND
   1241  -5 kazgame
Nov 11 09:08:01 su-gamer-r2 aranicy[1165]: 2026-11-11T09:08:01.140277Z  INFO bias changed name=kazgame bias=1 prev=0 class=Bulk cause=Evidence
Nov 11 09:08:31 su-gamer-r2 aranicy[1165]: 2026-11-11T09:08:31.140218Z  INFO bias changed name=kazgame bias=2 prev=1 class=Bulk cause=Evidence
Nov 11 09:08:31 su-gamer-r2 aranicy[1165]: 2026-11-11T09:08:31.640970Z  INFO applied rule rule="kazgame" pid=1241 bias=2 route="drift"
Nov 11 09:09:01 su-gamer-r2 aranicy[1165]: 2026-11-11T09:09:01.140254Z  INFO bias changed name=kazgame bias=3 prev=2 class=Bulk cause=Evidence
$ ./machine 'ps -o pid,ni,etime,comm -C kazgame'
    PID  NI     ELAPSED COMMAND
   1241  -3       02:24 kazgame
```

−5 became −3 in two and a half minutes, then −2. A game *is* CPU-bound, so it is
always `Bulk`, so this is not an edge case for me — it is every game. The
difference from last session is that the docs now say so out loud
(`COMPATIBILITY.md`: "unruled names synthesize a policy when bias ≠ 0",
"target: Bulk → +3").

### (b) Unruled names still get biased — but only ones with real load now.

Last session a sleeping `python3` at nice 3 was the example. This build gates the
demotion on aggregate demand, so:

```
$ ./machine 'python3 -c "import time; time.sleep(6)" & sleep 1; ps -o pid,ni,args -C python3; wait'
    PID  NI COMMAND
   1266   0 python3 -c import time; time.sleep(6)
```

`python3` is now left alone. A CPU-bound process with no rule at all is not:

```
$ ./machine 'aranicy match-test kazbulk; setsid nohup /usr/local/bin/kazbulk speed -seconds 400 rsa2048 >/dev/null 2>&1 </dev/null & sleep 2; ps -o pid,ni,comm -C kazbulk'
match-test: no rule matches "kazbulk"
    PID  NI COMMAND
   2022   0 kazbulk
$ ./machine 'sleep 160; ps -o pid,ni,etime,comm -C kazbulk'
    PID  NI     ELAPSED COMMAND
   2022   3       02:52 kazbulk
```

So (b) still happens, narrowed to workloads that actually burn CPU, which is the
documented intent now. Fine. But the log line for it is a lie that cost me time:

```
$ ./machine 'sudo journalctl -u aranicy --no-pager | grep "rule=\"kazbulk\"" | tail -2; aranicy match-test kazbulk'
Nov 11 09:20:33 su-gamer-r2 aranicy[2125]: 2026-11-11T09:20:33.469660Z  INFO applied rule rule="kazbulk" pid=2022 bias=2 route="drift"
Nov 11 09:21:33 su-gamer-r2 aranicy[2125]: 2026-11-11T09:21:33.471444Z  INFO applied rule rule="kazbulk" pid=2022 bias=3 route="drift"
match-test: no rule matches "kazbulk"
```

`applied rule rule="kazbulk"` is printed for a name that has no rule. Last
session I read that as "there must be an implicit rule"; it is really "learned
policy for comm kazbulk". The word `rule=` is wrong for this case.

### (c) There is a per-executable opt-out now — and it works.

The shipped `/etc/ananicy.d/ananicy.conf` now documents the key I had to guess
last time:

```
$ ./machine 'grep -n -B4 "^bias_protect" /etc/ananicy.d/ananicy.conf'
# Names the learned bias must never move (comm prefixes, space separated;
# comm is capped at 15 bytes, so `systemd` covers systemd-journald/-logind).
# Rules still apply to these names - this gates the learned axis only.
# PID 1 is always protected, list or no list. Set empty to protect nothing
# but init. Example: add your desktop's compositor shell or an editor.
bias_protect = systemd dbus-broker dbus-daemon sshd gdm sddm polkitd elogind
```

I appended my game and reloaded (no restart):

```
$ ./machine 'sudo sed -i "s/^bias_protect = .*/bias_protect = systemd dbus-broker dbus-daemon sshd gdm sddm polkitd elogind kazgame/" /etc/ananicy.d/ananicy.conf; grep -n "^bias_protect" /etc/ananicy.d/ananicy.conf; sudo systemctl reload aranicy; sleep 3; date; ps -o pid,ni,comm -C kazgame'
44:bias_protect = systemd dbus-broker dbus-daemon sshd gdm sddm polkitd elogind kazgame
Wed Nov 11 09:09:28 UTC 2026
    PID  NI COMMAND
   1241  -3 kazgame
$ ./machine 'sleep 40; date; ps -o pid,ni,comm -C kazgame'
Wed Nov 11 09:10:13 UTC 2026
    PID  NI COMMAND
   1241  -5 kazgame
```

−3 → −5, i.e. my rule's value came back, one drift pass after the reload, and the
process was never restarted. **That is the thing I asked for last session and it
now exists.** So (c) is fixed for the normal case.

It is not fixed for the case I actually hit. A rule is matched on
`/proc/<pid>/exe`; `bias_protect` is matched on the 15-byte kernel comm. Same
binary started through a differently-named file gets the rule and not the
protection. A/B, same binary, `bias_protect` containing `kazg2`:

`kazg2` is a copy of openssl with a rule `{"name": "kazg2", "nice": -5}`, and
`bias_protect` contains `kazg2`. `mygame` is a symlink to the same file, started
with the same arguments (full command list in kaz-6):

```
$ ./machine 'ps -o pid,ni,etime,comm -C mygame,kazg2,kazg2-alias'
    PID  NI     ELAPSED COMMAND
   2325  -5       03:24 kazg2          <- protected, rule -5 honoured
   2326  -5       03:24 kazg2-alias    <- protected by prefix accident
   2351  -2       03:09 mygame         <- rule matched kazg2, protection did not
```

Filed as **kaz-6**.

Also: `bias_protect` is not mentioned *anywhere* in the shipped
`COMPATIBILITY.md` reload-liveness table, so I could not tell from the docs
whether editing it needs a restart (it does not). Filed as part of **kaz-9**.

Still rejected, still no rule-level key:

```
$ ./machine 'printf "%s\n" "{\"name\": \"kazgame\", \"nice\": -5, \"bias\": false}" > ~/t1/r.rules; aranicy --config-dir ~/t1 lint'
warning: /home/arch/t1/r.rules:1: unknown key "bias" ignored
lint: warnings found
```

A per-game `"bias": false` in the rule is still the shape I want (one file, the
same file as the nice, travels with the game) and it is still not there.
`bias_protect` is a global root-owned list, which means for me it is "a second
place I have to remember", not "per-executable override". It is enough to make
the software usable; it is not the ergonomics I asked for.

---

## 3. `kaz-3-packaged-install-has-no-quick-start` — gone

The README that ships now has a package section with the actual command:

```
$ ./machine 'awk "/^### From the package/,/^### From the tree/" /usr/share/doc/aranicy/README.md | head -12'
### From the package (Arch)

```sh
sudo pacman -U aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst \
                aranicy-0.3.0-1-x86_64.pkg.tar.zst
sudo systemctl enable --now aranicy      # Requires= pulls the collector in
aranicy lint                             # what the daemon loaded
systemctl status aranicy                 # active (running) + the startup log
```

The package ships **no rule corpus**: rule data from the ananicy ecosystem is
GPL and is not bundled (`provides=ananicy-cpp`, so an old `ananicy-cpp`
```

I followed it verbatim from a clean machine and it reached a working state, first
try, no guessing:

```
$ ./machine 'cd ~/payload && sudo pacman -U --noconfirm aranicy-0.3.0-1-x86_64.pkg.tar.zst aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst 2>&1; echo "EXIT=$?"; echo "--- post-install state ---"; echo "is-enabled=$(systemctl is-enabled aranicy 2>&1) is-active=$(systemctl is-active aranicy 2>&1)"'
loading packages...
resolving dependencies...
looking for conflicting packages...

Packages (2) aranicy-0.3.0-1  aranicy-collector-0.3.0-1

Total Installed Size:  6.24 MiB

:: Proceed with installation? [Y/n] 
checking keyring...
checking package integrity...
loading package files...
checking for file conflicts...
checking available disk space...
:: Processing package changes...
installing aranicy-collector...
installing aranicy...
:: Running post-transaction hooks...
(1/2) Reloading system manager configuration...
(2/2) Arming ConditionNeedsUpdate...
EXIT=0
--- post-install state ---
is-enabled=disabled is-active=inactive
$ ./machine 'sudo systemctl enable --now aranicy 2>&1; echo "EXIT=$?"; sleep 4; systemctl is-enabled aranicy; systemctl is-active aranicy; systemctl is-active aranicy-collector; echo "=== lint ==="; aranicy lint 2>&1; echo "EXIT=$?"'
Created symlink '/etc/systemd/system/multi-user.target.wants/aranicy.service' → '/usr/lib/systemd/system/aranicy.service'.
EXIT=0
enabled
active
active
=== lint ===
OK: 0 rules, 0 types, 0 cgroups
EXIT=0
```

The package still does not enable itself, but the doc now tells me to, in the
one place I would look, and the `aranicy lint` / `status` follow-ups both work. The
`./contrib/install.sh` instructions are still there for tree builds, but they are
now under a separate heading. This report is closed.

One residual, not the same issue: `pacman -Qi aranicy` now says
`Install Script  : Yes`, and the script prints nothing and starts nothing. I
checked only that it did not enable or start the unit (above).

---

## 4. `kaz-4-collector-ships-daemon-readme-and-dead-links` — different

The collector has its own README now, and it is about the collector — sockets,
privileges, how to check it, how to stop it:

```
$ ./machine 'cmp /usr/share/doc/aranicy/README.md /usr/share/doc/aranicy-collector/README.md; echo "EXIT=$?"; ls -l /usr/share/doc/aranicy/ /usr/share/doc/aranicy-collector/'
/usr/share/doc/aranicy/README.md /usr/share/doc/aranicy-collector/README.md differ: char 1, line 1
EXIT=1
/usr/share/doc/aranicy-collector/:
total 4
-rw-r--r-- 1 root root 2642 Nov 11 08:59 README.md

/usr/share/doc/aranicy/:
total 56
-rw-r--r-- 1 root root 25420 Nov 11 08:59 BENCHMARKS.md
-rw-r--r-- 1 root root 18632 Nov 11 08:59 COMPATIBILITY.md
-rw-r--r-- 1 root root  6346 Nov 11 08:59 README.md
```

`BENCHMARKS.md` and `COMPATIBILITY.md` are now shipped too. The "check it works"
command from the collector README is real (see kaz-10 for a snag):

```
$ ./machine 'sudo journalctl -u aranicy-collector --no-pager -n 3'
Nov 11 09:15:26 su-gamer-r2 aranicy-collectord[1826]: 2026-11-11T09:15:26.243367Z  INFO aranicy_collectord::events: exec-event client connected
Nov 11 09:18:33 su-gamer-r2 aranicy-collectord[1826]: 2026-11-11T09:18:33.385641Z  INFO aranicy_collectord::events: exec-event client disconnected error=Broken pipe (os error 32) dropped=0
Nov 11 09:18:33 su-gamer-r2 aranicy-collectord[1826]: 2026-11-11T09:18:33.466352Z  INFO aranicy_collectord::events: exec-event client connected
```

But every relative link in the daemon README is still dead, including the two
files that are now sitting right next to it:

```
$ ./machine 'for p in LICENSE docs/BENCHMARKS.md docs/COMPATIBILITY.md tools/storm/ banner.svg; do printf "%-24s -> " "$p"; ls /usr/share/doc/aranicy/$p 2>&1; done'
LICENSE                  -> ls: cannot access '/usr/share/doc/aranicy/LICENSE': No such file or directory
docs/BENCHMARKS.md       -> ls: cannot access '/usr/share/doc/aranicy/docs/BENCHMARKS.md': No such file or directory
docs/COMPATIBILITY.md    -> ls: cannot access '/usr/share/doc/aranicy/docs/COMPATIBILITY.md': No such file or directory
tools/storm/             -> ls: cannot access '/usr/share/doc/aranicy/tools/storm/': No such file or directory
banner.svg               -> ls: cannot access '/usr/share/doc/aranicy/banner.svg': No such file or directory
```

The first line of the README is still `<img src="banner.svg" ...>`, so the doc
opens with a broken image. And there is still no man page:

```
$ ./machine 'find /usr/share/man -iname "*aranicy*"; echo "EXIT=$?"'
EXIT=0
```

(no output, exit 0 — nothing matched.)

So: the *content* complaint is fixed, the *link* complaint is not. I am leaving
this as "different" rather than "gone", and not re-filing it, because the
remainder is the same defect I already described and the fix is a two-line
change to the package.

---

## 5. `kaz-5-no-per-process-status` — still happens

```
$ ./machine 'aranicy dump proc 2>/dev/null | python3 -c "import json,sys; d=json.load(sys.stdin); print(json.dumps(d[0], indent=2)); print(\"keys:\", sorted(d[0].keys()))"'
{
  "name": "systemd",
  "nice": 0,
  "pid": 1
}
keys: ['name', 'nice', 'pid']
```

Three fields, unchanged. And it is live (I reniced a process by hand and the dump
followed it), so it is a snapshot of `nice`, not an answer:

```
$ ./machine 'sudo renice -n 4 -p 1476 >/dev/null; aranicy dump proc 2>/dev/null | python3 -c "import json,sys; d=json.load(sys.stdin); print([x for x in d if x[\"pid\"]==1476])"'
[{'name': 'kazdemo2', 'nice': 4, 'pid': 1476}]
```

Two things got better:

- `aranicy match-test <name>` answers "which rule would this name get", which is
  half of what I wanted and works unprivileged:

```
$ ./machine 'aranicy match-test kazgame; echo "EXIT=$?"; aranicy match-test python3; echo "EXIT=$?"'
{
  "match": "exact",
  "name": "kazgame",
  "policy": {
    "nice": -5
  }
}
EXIT=0
match-test: no rule matches "python3"
EXIT=1
```

- `log_applied_rule = true` is still the only per-pid attribution, and it is
  still off by default. Turning it on by editing the config and reloading works
  without a restart (it is a "live" key), which is new to me:

```
$ ./machine 'sudo sed -i "s/^log_applied_rule = false/log_applied_rule = true/" /etc/ananicy.d/ananicy.conf; grep -n "^log_applied_rule" /etc/ananicy.d/ananicy.conf; sudo systemctl reload aranicy; sleep 2; setsid nohup /usr/local/bin/kazdemo 20 >/dev/null 2>&1 </dev/null & sleep 2; sudo journalctl -u aranicy --no-pager --since "-1min" | grep "applied rule" | tail -2'
31:log_applied_rule = true
Nov 11 09:07:57 su-gamer-r2 aranicy[1165]: 2026-11-11T09:07:57.244587Z  INFO applied rule rule="kazdemo" pid=1364 bias=0 route="exec"
```

But there is still no command that takes a **pid** and tells me the rule, the
bias and where the number came from; and the dump got *worse* to read, because it
now lists every kernel thread:

```
$ ./machine 'aranicy dump proc 2>/dev/null | python3 -c "import json,sys; d=json.load(sys.stdin); print(\"rows=\",len(d)); print(\"nice=-20 rows=\",sum(1 for x in d if x[\"nice\"]==-20))"'
rows= 131
nice=-20 rows= 42
```

42 of 131 rows are `kworker/R-*` and friends at the kernel's own −20, and my
`kazgame` is one line among them. Last session I read this file with `head -7` and
saw only daemons; now I have to filter it. My five interesting rows:

```
$ ./machine 'ps -eo pid,ni,comm --no-headers | awk "\$2 != 0 && \$1 > 200"'
    279   9 systemd-coredum
    375 -20 kworker/R-cfg80211
   1241  -5 kazgame
   1826 -10 aranicy-collect
   1851  -5 my-competitive-
   2022   3 kazbulk
   2125  -5 aranicy
   2193  -4 kaztype
```

and nothing in the daemon's own output tells me that `1851` is `-5` because a rule
said so minus a +3 bias that was gated by `bias_protect`. I worked that out from
the journal, by hand, again. Still a `minor`, still the thing that made the other
investigations slow.
