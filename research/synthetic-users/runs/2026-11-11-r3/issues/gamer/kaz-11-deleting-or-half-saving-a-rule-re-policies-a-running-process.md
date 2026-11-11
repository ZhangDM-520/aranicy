# Deleting or half-saving a rule file silently re-policies the process that was running under it, walking a game pinned at −7 up to +3

- severity: major
- category: behaviour
- environment: Arch Linux rolling, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Edit the rule for a running game — the normal way I tune things: the game is
open, I change the file, `systemctl reload aranicy`, look at the process. I did
it two ways a real editor does it: (a) remove the line and reload, (b) save a
file that got cut off part-way (crash, full disk, an editor writing a partial
temp file, me killing the editor mid-save).

I also wanted to know what a rule of −7 actually produces, because
`COMPATIBILITY.md` says the applied value is `clamp(rule_nice + bias, …)` — so I
watched the same process for its first three minutes.

## Steps to reproduce

```
sudo cp /usr/bin/openssl /usr/local/bin/kzdel
printf '{"name": "kzdel", "nice": -7}\n' | sudo tee /etc/ananicy.d/72-kzdel.rules
sudo systemctl reload aranicy
setsid nohup /usr/local/bin/kzdel speed -seconds 600 rsa2048 >/dev/null 2>&1 </dev/null &
sleep 2;   ps -o pid,ni,comm -C kzdel      # -7
sleep 150; ps -o pid,ni,comm -C kzdel      # -4   <-- learned +3 already on top of my rule
sudo rm /etc/ananicy.d/72-kzdel.rules
sudo systemctl reload aranicy
sleep 70;  ps -o pid,ni,comm -C kzdel      # +3   <-- my rule is gone and the process is worse than default
```

Variant (b), a truncated save of the same rule:

```
printf '{"name": "khb1", "nice": -7}\n' | sudo tee /etc/ananicy.d/77-khb1.rules
sudo systemctl reload aranicy
setsid nohup /usr/local/bin/khb1 speed -seconds 900 rsa2048 >/dev/null 2>&1 </dev/null &
sudo sh -c 'head -c 22 /etc/ananicy.d/77-khb1.rules > /tmp/x && mv /tmp/x /etc/ananicy.d/77-khb1.rules'
sudo systemctl reload aranicy
sleep 20; ps -o pid,ni,comm -C khb1        # jumps straight from -7 to +1
```

## Expected

Three things, in order of how much they matter to me:

1. A running process whose rule I *delete* should not end up worse than a
   process that never had a rule. Either leave its nice alone, or put it back
   to what it was before my rule, but tell me which.
2. If the daemon cannot read a rule line, it should say what that means for the
   processes that were running under it. "no JSON object span" on a line number
   is a parser message, not "kazdiff is no longer pinned".
3. `reloaded N rules` should not be the only record of a pin disappearing —
   `N` dropping by one is the only trace, and I would have to remember the
   previous N.

## Actual

1. Deleting the rule and reloading moves the *running* process from −4 to **+3**
   within one drift pass (~70 s). The complete log of the reload is
   `reloaded 15 rules; bias reconfigured`. There is no line at all about
   `kzdel`: I grepped for the name, for "removed", "deleted" and "no longer" and
   got nothing.
2. A truncated save does produce one `WARN … no JSON object span (missing '{' or
   '}')`, but it is attached to the file and line, not to the process, and the
   process then jumps −7 → +1 → +3.
3. Even with the rule *intact*, the −7 I wrote is not what the process gets:
   after 150 s it is at −4, because the learned bias reached +3 and was added to
   my rule. `ps` says −4, the file says −7, and nothing on the machine connects
   the two numbers.

So the sequence "I pin my game at −7, then tidy up the rule file" is
−7 → −4 → +3, and the only thing that changed in the log is a rule count going
down by one.

## Evidence

Session 1 — rule deleted while the process ran (current build, 2026-11-11):

```
$ ./machine 'setsid nohup /usr/local/bin/kzdel speed -seconds 600 rsa2048 >/dev/null 2>&1 </dev/null & T0=$(date +%s); echo "t=0 kzdel(ni)=$(ps -o ni= -C kzdel|tr -d " ") rule=-7"; sleep 150; echo "t=$(( $(date +%s)-T0 )) kzdel(ni)=$(ps -o ni= -C kzdel|tr -d " ") rule=-7  bias=$(sudo journalctl -u aranicy --no-pager | grep "name=kzdel" | tail -1 | grep -o "bias=[0-9-]*")"; echo "--- delete the rule file entirely, reload ---"; sudo rm -f /etc/ananicy.d/72-kzdel.rules; sudo systemctl reload aranicy; sleep 3; J0=$(date +%s); sudo journalctl -u aranicy --no-pager --since "-10s" | grep -iE "kzdel|reload" | sed "s/.*aranicy\[[0-9]*\]: //"; sleep 70; echo "t=$(( $(date +%s)-T0 )) kzdel(ni)=$(ps -o ni= -C kzdel|tr -d " ") (rule file deleted)"; sleep 60; echo "t=$(( $(date +%s)-T0 )) kzdel(ni)=$(ps -o ni= -C kzdel|tr -d " ")"; echo "--- any warning about the disappearance? ---"; sudo journalctl -u aranicy --no-pager --since "-$(( $(date +%s)-J0+5 ))s" | grep -iE "kzdel|removed|deleted|no longer" | sed "s/.*aranicy\[[0-9]*\]: //"'
t=0 kzdel(ni)=-7 rule=-7
t=150 kzdel(ni)=-4 rule=-7  bias=bias=3
--- delete the rule file entirely, reload ---
Nov 11 10:34:00 su-gamer-r3 systemd[1]: Reloading aranicy - auto-nice daemon...
2026-11-11T10:34:01.014804Z  INFO reloaded 15 rules; bias reconfigured
Nov 11 10:34:01 su-gamer-r3 systemd[1]: Reloaded aranicy - auto-nice daemon.
t=224 kzdel(ni)=3 (rule file deleted)
t=284 kzdel(ni)=3
--- any warning about the disappearance? ---
```

(the last section is empty: no line mentions the name, the removal, or the
process)

Session 2 — truncated rule (current build, earlier the same morning):

```
$ ./machine 'sudo head -c 24 /etc/ananicy.d/90-kazdiff.rules | sudo tee /etc/ananicy.d/90-kazdiff.rules.new >/dev/null; sudo mv /etc/ananicy.d/90-kazdiff.rules.new /etc/ananicy.d/90-kazdiff.rules; cat /etc/ananicy.d/90-kazdiff.rules; sudo systemctl reload aranicy; sleep 3; sudo journalctl -u aranicy --no-pager --since "-30s" | tail -4; sleep 210; ps -o pid,ni,etime,comm -C kazdiff,kazbulk'
{"name": "kazdiff", "nic
…
2026-11-11T09:57:24.041692Z  WARN /etc/ananicy.d/90-kazdiff.rules:1: no JSON object span (missing '{' or '}')
2026-11-11T09:57:24.041873Z  INFO reloaded 3 rules; bias reconfigured
…
    PID  NI     ELAPSED COMMAND
   1353   3       04:28 kazdiff
   1354   3       04:28 kazbulk
```

Session 3 — the same truncation on the previous build (for comparison; see
`differential.md`), 22 bytes saved:

```
$ ./machine '… echo -n "rule file now: "; cat /etc/ananicy.d/77-khb1.rules; sudo systemctl reload aranicy; sleep 3; for i in $(seq 12); do sleep 15; echo "t=… kb(ni)=$(ps -o ni= -C kbb1|tr -d " ") kh(ni)=$(ps -o ni= -C khb1|tr -d " ")"; done'
rule file now: {"name": "khb1", "nice
t=129 kb(ni)=1 kh(ni)=1
t=144 kb(ni)=1 kh(ni)=1
…
t=189 kb(ni)=3 kh(ni)=3
```

The rule intact, for reference — the learned axis eroding it (same run,
`kh` at t=110 with the file still whole):

```
t=110 kb(ni)=0 kh(ni)=-7 [rule=-7]
```

…and later in the session, on a different name with the rule in the file
throughout (same conclusion, ends at −4):

```
t=15 kzb(ni)=-7 rule=-7
…
t=120 kzb(ni)=-5 rule=-7
t=181 kzb(ni)=-4 rule=-7
```

## What I could not determine

- Whether the daemon considers a deleted rule a *change at all*. It reloads
  rules on SIGHUP and reconfigures the bias, so it must know the rule set
  changed; it just does not act on the processes that lost one. `suspicion:`
  the drift pass re-derives each process's policy from *current* rules plus
  current bias, and a process with no rule and a non-zero bias gets the
  synthesized bias policy, so "no rule" quietly means "whatever the learner
  thinks".
- Whether `bias_protect` would have saved me here. It is keyed on the kernel
  comm (see kaz-6), and for this process comm = `kzdel`, so it would — but only
  because the launch name and the file name agreed this time.
- What happens if the rule is replaced by a *different* rule for the same name
  rather than removed. I only tested removal and truncation.
- Whether the 60-second lag between "rule gone" and "+3 applied" is the drift
  pass (`check_freq = 60`). It looks like one pass, but I did not instrument it.

`workflow this blocks`: "I tune a rule while the thing is running and look at
the result", and the harder one — "I must be able to undo a decision that hurts
one specific game". Undoing a rule does not undo the decision; it hands the
decision to the learner, which is the opposite of what I asked for.

`related`: kaz-5 (round 1 — `dump proc` shows the number, never the reason),
kaz-6 (bias_protect is comm-keyed), kaz-7 (losing history leaves applied nice in
place — this is the mirror image: losing the *rule* leaves the learned value in
charge).
