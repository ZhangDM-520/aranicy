# `bias_protect` matches the 15-byte kernel comm while rules match the executable, so a game started under a different name gets the rule and none of the protection

- severity: major
- category: behaviour
- environment: Arch Linux rolling, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Do the one thing I actually want from this daemon: leave the automatic bias on,
pin my game with a rule, and add that game to `bias_protect` so nothing learned
can move it. Then launch the game the way I launch it — through a
differently-named entry point, which is what a launcher/wrapper does — and check
that it kept the nice I gave it.

## Steps to reproduce

```
sudo cp /usr/bin/openssl /usr/local/bin/kazg2
sudo ln -sf /usr/local/bin/kazg2 /usr/local/bin/mygame
printf "%s\n" "{\"name\": \"kazg2\", \"nice\": -5}" | sudo tee /etc/ananicy.d/98-kazg2.rules
sudo sed -i 's/^bias_protect = .*/bias_protect = systemd dbus-broker dbus-daemon sshd gdm sddm polkitd elogind kazgame kazg2/' /etc/ananicy.d/ananicy.conf
sudo systemctl reload aranicy
setsid nohup /usr/local/bin/kazg2  speed -seconds 300 rsa2048 >/dev/null 2>&1 </dev/null &
setsid nohup /usr/local/bin/mygame speed -seconds 280 rsa2048 >/dev/null 2>&1 </dev/null &
sleep 160
ps -o pid,ni,etime,comm -C mygame,kazg2
```

(`kazg2` is `openssl` renamed: a CPU-bound binary with a game-like name. Both
processes are the same file; one is started under its own name, one through a
symlink with a different name.)

## Expected

`bias_protect` is documented in `/etc/ananicy.d/ananicy.conf` as

```
# Names the learned bias must never move (comm prefixes, space separated;
# comm is capped at 15 bytes, so `systemd` covers systemd-journald/-logind).
# Rules still apply to these names - this gates the learned axis only.
```

The name I protect is the name I wrote the rule for. Both processes are the same
executable, so both should keep the rule's −5, and neither should be movable by
the learned axis.

## Actual

The one started as `kazg2` keeps −5. The one started as `mygame` — same file,
same rule — is walked from −5 to −2 by the bias, +1 every 30 s, with
`bias_protect` containing `kazg2`.

```
$ ./machine 'ps -o pid,ni,etime,comm -C mygame,kazg2,kazg2-alias'
    PID  NI     ELAPSED COMMAND
   2325  -5       03:24 kazg2
   2326  -5       03:24 kazg2-alias
   2351  -2       03:09 mygame
```

The rule matched (`applied rule rule="kazg2"` is logged for that pid's own
process), the protection did not: the value that was applied to it came from a
*different* history entry, the one keyed on the comm.

## Evidence

```
$ ./machine 'grep -n "^bias_protect" /etc/ananicy.d/ananicy.conf'
44:bias_protect = systemd dbus-broker dbus-daemon sshd gdm sddm polkitd elogind kazg2
$ ./machine 'ps -o pid,ni,comm,args -C mygame'
    PID  NI COMMAND         COMMAND
   2351  -5 mygame          /usr/local/bin/mygame speed -seconds 280 rsa2048
```

(the `-5` above is 3 s after start; below is 160 s later:)

```
$ ./machine 'ps -o pid,ni,etime,comm -C mygame,kazg2,kazg2-alias'
    PID  NI     ELAPSED COMMAND
   2325  -5       03:24 kazg2
   2326  -5       03:24 kazg2-alias
   2351  -2       03:09 mygame
```

```
$ ./machine 'sudo journalctl -u aranicy --no-pager | grep "kazg2" | head -12'
Nov 11 09:22:07 su-gamer-r2 aranicy[2125]: 2026-11-11T09:22:07.830065Z  INFO applied rule rule="kazg2" pid=2325 bias=0 route="exec"
Nov 11 09:22:07 su-gamer-r2 aranicy[2125]: 2026-11-11T09:22:07.832501Z  INFO applied rule rule="kazg2" pid=2326 bias=0 route="exec"
Nov 11 09:22:22 su-gamer-r2 aranicy[2125]: 2026-11-11T09:22:22.757079Z  INFO applied rule rule="kazg2" pid=2351 bias=0 route="exec"
Nov 11 09:23:03 su-gamer-r2 aranicy[2125]: 2026-11-11T09:23:03.466665Z  INFO bias changed name=kazg2 bias=1 prev=0 class=Bulk cause=Evidence
Nov 11 09:23:08 su-gamer-r2 aranicy[2125]: 2026-11-11T09:23:08.467585Z  INFO bias changed name=kazg2-alias bias=1 prev=0 class=Bulk cause=Evidence
Nov 11 09:23:33 su-gamer-r2 aranicy[2125]: 2026-11-11T09:23:33.466684Z  INFO bias changed name=kazg2 bias=2 prev=1 class=Bulk cause=Evidence
Nov 11 09:23:33 su-gamer-r2 aranicy[2125]: 2026-11-11T09:23:33.474432Z  INFO applied rule rule="kazg2" pid=2351 bias=1 route="drift"
Nov 11 09:23:38 su-gamer-r2 aranicy[2125]: 2026-11-11T09:23:38.466666Z  INFO bias changed name=kazg2-alias bias=2 prev=1 class=Bulk cause=Evidence
Nov 11 09:24:03 su-gamer-r2 aranicy[2125]: 2026-11-11T09:24:03.467464Z  INFO bias changed name=kazg2 bias=3 prev=2 class=Bulk cause=Evidence
Nov 11 09:24:08 su-gamer-r2 aranicy[2125]: 2026-11-11T09:24:08.466738Z  INFO bias changed name=kazg2-alias bias=3 prev=2 class=Bulk cause=Evidence
Nov 11 09:24:33 su-gamer-r2 aranicy[2125]: 2026-11-11T09:24:33.477439Z  INFO applied rule rule="kazg2" pid=2351 bias=3 route="drift"
$ ./machine 'sudo journalctl -u aranicy --no-pager | grep -E "name=(mygame|kazg2)" | tail -8'
Nov 11 09:23:08 su-gamer-r2 aranicy[2125]: 2026-11-11T09:23:08.467585Z  INFO bias changed name=kazg2-alias bias=1 prev=0 class=Bulk cause=Evidence
Nov 11 09:23:13 su-gamer-r2 aranicy[2125]: 2026-11-11T09:23:13.466687Z  INFO bias changed name=mygame bias=1 prev=0 class=Bulk cause=Evidence
Nov 11 09:23:33 su-gamer-r2 aranicy[2125]: 2026-11-11T09:23:33.466684Z  INFO bias changed name=kazg2 bias=2 prev=1 class=Bulk cause=Evidence
Nov 11 09:23:38 su-gamer-r2 aranicy[2125]: 2026-11-11T09:23:38.466666Z  INFO bias changed name=kazg2-alias bias=2 prev=1 class=Bulk cause=Evidence
Nov 11 09:23:43 su-gamer-r2 aranicy[2125]: 2026-11-11T09:23:43.466904Z  INFO bias changed name=mygame bias=2 prev=1 class=Bulk cause=Evidence
Nov 11 09:24:03 su-gamer-r2 aranicy[2125]: 2026-11-11T09:24:03.467464Z  INFO bias changed name=kazg2 bias=3 prev=2 class=Bulk cause=Evidence
Nov 11 09:24:08 su-gamer-r2 aranicy[2125]: 2026-11-11T09:24:08.466738Z  INFO bias changed name=kazg2-alias bias=3 prev=2 class=Bulk cause=Evidence
Nov 11 09:24:13 su-gamer-r2 aranicy[2125]: 2026-11-11T09:24:13.466714Z  INFO bias changed name=mygame bias=3 prev=2 class=Bulk cause=Evidence
```

`pid=2351` is `mygame`; the line reading `rule="kazg2" pid=2351 bias=1` is the
rule for one name being applied with the bias of another. `pid=2325` (`kazg2`
itself) never gets a drift line at all — protected.

Note that `kazg2` also reached `bias=3` but its applied nice stayed −5 — so the
protection does work when the comm matches. The three names are the same
executable and behave three different ways:

```
$ ./machine 'ls -l /usr/local/bin/mygame /usr/local/bin/kazg2'
-rwxr-xr-x 1 root root 976472 Nov 11 09:22 /usr/local/bin/kazg2
lrwxrwxrwx 1 root root     20 Nov 11 09:22 /usr/local/bin/mygame -> /usr/local/bin/kazg2
$ ./machine 'ps -o pid,ni,comm,args -C mygame'   # 3 s after start
    PID  NI COMMAND         COMMAND
   2351  -5 mygame          /usr/local/bin/mygame speed -seconds 280 rsa2048
```

The rule that was applied to that pid is `rule="kazg2"` (the exe-derived name);
the comm the bias used is `mygame`.

`kazg2-alias` stayed at −5 only because "kazg2" is a *prefix* of
"kazg2-alias" — accidental protection, not intended.

Same result with a second pair earlier in the session (a symlink named
`my-competitive-game` pointing at a copy of openssl called `kazgame`, rule
`{"name": "kazgame", "nice": -5}`, `kazgame` in `bias_protect`):

```
$ ./machine 'ps -o pid,ni,comm,args -C my-competitive-game,kazgame,kazbulk'
    PID  NI COMMAND         COMMAND
   1241  -5 kazgame         /usr/local/bin/kazgame speed -seconds 420 rsa2048
   1851  -4 my-competitive- /usr/local/bin/my-competitive-game speed -seconds 60 rsa2048
   2022   2 kazbulk         /usr/local/bin/kazbulk speed -seconds 400 rsa2048
```

(`kazbulk` is unrelated — a third CPU hog running at the time; I left it in the
output rather than trim the command's result.)

## What I could not determine

- Whether this is intended. `COMPATIBILITY.md` does say the two namespaces are
  different ("History keyed by the **kernel comm** ...; rule matching uses the
  separate executable-derived name"), and the config comment says "comm
  prefixes". So a careful reader can see it. What I cannot see is any way to
  protect by executable name, or any warning that a name I protected is not the
  name the bias will use. Nothing told me that adding a game to `bias_protect`
  had no effect on that game.

  `suspicion:` the check is a plain prefix test on `comm` where the rule path
  uses `basename(/proc/<pid>/exe)`, so the two only agree when the launch name
  equals the file name.

- What fraction of real games this hits. Two shapes I know of, neither testable
  here: a launcher that renames the process (`prctl(PR_SET_NAME)`) and any
  wrapper/symlink with a different name. I reproduced the second.

- Whether 15-byte truncation makes it worse or better. Prefix matching means a
  long name still matches its own truncated comm, so truncation itself is
  handled; the mismatch is the name, not the length.

`workflow this blocks`: "leave the automatic tuning on, pin one game, be sure
that one program is untouched" — the second half silently does not hold for the
executable I started under another name.

`related`: kaz-2 (round 1 — this is the opt-out it asked for, working only when
the launch name equals the file name).
