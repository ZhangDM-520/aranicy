# The learned bias overrides the nice I wrote (−5 became −2), also nices processes that have no rule at all (a fresh `python3` ran at 3), and cannot be switched off for one executable

- severity: major
- category: behaviour
- environment: Arch Linux rolling, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Two things I do on my own desktop:

1. Pin one game to a nice level I chose, and be sure that is what it gets.
2. Not have anything else moved behind my back — if something is slow, I want to be able
   to point at one executable and say "not this one".

I wrote `{"name": "kazgame", "nice": -5}` in `/etc/ananicy.d/50-kaz.rules`, reloaded, and
started the process. It was applied correctly at exec:

```
INFO applied rule rule="kazgame" pid=7056 bias=0 route="exec"
```

## Steps to reproduce

```
# rule in place, daemon running
./machine '/usr/local/bin/kazgame 420 &'          # CPU-bound stand-in for the game
# wait ~4 minutes without touching the rule
./machine 'ps -o pid,ni,comm -C kazgame'
./machine 'sudo journalctl -u aranicy --no-pager | grep kazgame'
# and, for the second half:
./machine 'aranicy dump rules'                    # no rule mentions python3
./machine 'python3 -c "import time; time.sleep(6)" &' ; sleep 1; ps -o pid,ni,args -C python3
```

`kazgame` here is `cp /usr/bin/openssl /usr/local/bin/kazgame` run as
`kazgame speed -seconds 420 rsa2048` — a CPU-bound process with a game-like name, which
is the point: a game *is* CPU-bound.

## Expected

- The rule's `-5` is what the process runs at, unless I change the rule.
- If the daemon is going to adjust something, it adjusts things I named.
- If it gets one of them wrong, I can exempt that one executable without turning the
  whole feature off.

The shipped README describes the arithmetic as `clamp(rule_nice + bias, -20, 19)`, which
reads as "a rule exists and the learned bias is a delta on it".

## Actual

1. Four minutes in, the engine classifies my game as `Bulk` and steps it +1 at a time.
   The process goes from −5 to **−2** while I watch, with no rule change:

```
$ ./machine 'cat /etc/ananicy.d/50-kaz.rules; ps -o pid,ni,comm -C kazgame'
{"name": "kazgame", "nice": -5}
{"name": "openssl", "nice": 19, "ioclass": "idle"}
    PID  NI COMMAND
   7056  -2 kazgame
```

2. The bias is applied to names that have **no rule at all**. `python3` is not in the
   rules file; a freshly started `python3` runs at nice 3.

```
$ ./machine 'aranicy dump rules'
{
  "exact": [
    {
      "match": "exact",
      "name": "kazgame",
      "nice": -5
    },
    {
      "ioclass": "idle",
      "match": "exact",
      "name": "openssl",
      "nice": 19
    }
  ],
  "regex": []
}
$ ./machine 'python3 -c "import time; time.sleep(6)" & sleep 1; ps -o pid,ni,args -C python3'
    PID  NI COMMAND
   7199   3 python3 -c import time; time.sleep(6)
```

3. There is no per-executable opt-out. A `bias` key in a rule is rejected (this lint ran
   against a scratch rules directory, `~/t1`, so the live config was untouched):

```
$ ./machine 'printf "%s\n" "{\"name\": \"kazgame\", \"nice\": -5, \"bias\": false}" > r.rules; aranicy --config-dir ~/t1 lint'
warning: /home/arch/t1/r.rules:1: unknown key "bias" ignored
lint: warnings found
EXIT=1
```

   and `aranicy --help` / `aranicy dump --help` offer no bias control at all:

```
$ ./machine 'aranicy --help | grep -i bias'
EXIT=1
```

   The only off switch I found is a config key that appears nowhere in the shipped
   documentation:

```
$ ./machine 'grep -n bias /etc/ananicy.d/ananicy.conf'
33:# Dynamic nice bias: pool related compiler tools into one learning entry.
37:bias_group.build = ^(cc1.*|lto1.*|collect2|as|ld.*)$
$ ./machine 'grep -nE "bias_enable" /usr/share/doc/aranicy/README.md; echo "EXIT=$?"'
EXIT=1
```

   I found `bias_enable` by guessing it, and the daemon then told me it is restart-only:

```
$ ./machine 'printf "\nbias_enable = false\n" | sudo tee -a /etc/ananicy.d/ananicy.conf; sudo systemctl reload aranicy'
Nov 11 08:22:14 su-gamer-r1 aranicy[1126]: 2026-11-11T08:22:14.419844Z  WARN restart-only keys ignored: bias_enable
```

   After `systemctl restart aranicy` with `bias_enable = false`, new `sshd-session` and
   `bash` processes came up at nice 0 again, so it does work — you just have to know the
   key exists, and it takes the whole feature away.

## Evidence

The bias steps, from the daemon's journal, for a rule whose value never changed:

```
Nov 11 08:22:51 su-gamer-r1 aranicy[7012]: 2026-11-11T08:22:51.331317Z  INFO applied rule rule="kazgame" pid=7056 bias=0 route="exec"
Nov 11 08:25:06 su-gamer-r1 aranicy[7012]: 2026-11-11T08:25:06.203076Z  INFO bias changed name=kazgame bias=1 prev=0 class=Bulk cause=Evidence
Nov 11 08:25:36 su-gamer-r1 aranicy[7012]: 2026-11-11T08:25:36.203092Z  INFO bias changed name=kazgame bias=2 prev=1 class=Bulk cause=Evidence
Nov 11 08:25:36 su-gamer-r1 aranicy[7012]: 2026-11-11T08:25:36.215105Z  INFO applied rule rule="kazgame" pid=7056 bias=2 route="drift"
Nov 11 08:26:06 su-gamer-r1 aranicy[7012]: 2026-11-11T08:26:06.203232Z  INFO bias changed name=kazgame bias=3 prev=2 class=Bulk cause=Evidence
Nov 11 08:26:36 su-gamer-r1 aranicy[7012]: 2026-11-11T08:26:36.216139Z  INFO applied rule rule="kazgame" pid=7056 bias=3 route="drift"
```

The no-rule case, same journal — the daemon says "applied rule" for a name that is not
in `dump rules`:

```
Nov 11 08:27:35 su-gamer-r1 aranicy[7012]: 2026-11-11T08:27:35.691180Z  INFO applied rule rule="python3" pid=7180 bias=3 route="exec"
Nov 11 08:27:47 su-gamer-r1 aranicy[7012]: 2026-11-11T08:27:47.547168Z  INFO applied rule rule="python3" pid=7199 bias=3 route="exec"
```

It is not only `python3`. In the same session `dump bias` reported bias 3 for `ps`,
`pacman`, `systemd-bless-b`, `systemd-debug-g`, `systemd-fstab-g` and `systemd-tpm2-ge`
(system generators, all with no rule), and an earlier session hit 3 on `sshd-session`,
`sshd-auth`, `systemd-journal`, `systemd` and `bash`:

```
$ ./machine 'sudo journalctl -u aranicy --no-pager | grep -E "name=sshd|name=bash" | head -4'
Nov 11 08:19:19 su-gamer-r1 aranicy[1126]: 2026-11-11T08:19:19.456348Z  INFO bias changed name=bash bias=1 prev=0 class=Bulk cause=Evidence
Nov 11 08:19:24 su-gamer-r1 aranicy[1126]: 2026-11-11T08:19:24.450010Z  INFO bias changed name=sshd-session bias=1 prev=0 class=Bulk cause=Evidence
Nov 11 08:19:49 su-gamer-r1 aranicy[1126]: 2026-11-11T08:19:49.446854Z  INFO bias changed name=bash bias=2 prev=1 class=Bulk cause=Evidence
Nov 11 08:20:04 su-gamer-r1 aranicy[1126]: 2026-11-11T08:20:04.446836Z  INFO bias changed name=sshd-session bias=2 prev=1 class=Bulk cause=Evidence
```

including my own inspection commands:

```
$ ./machine 'sudo journalctl -u aranicy --no-pager --since "08:31" | grep "applied rule" | grep -vE "kazgame|openssl" | tail -4'
Nov 11 08:37:02 su-gamer-r1 aranicy[7525]: 2026-11-11T08:37:02.105134Z  INFO applied rule rule="pgrep" pid=315165 bias=2 route="exec"
Nov 11 08:37:02 su-gamer-r1 aranicy[7525]: 2026-11-11T08:37:02.109065Z  INFO applied rule rule="ps" pid=315166 bias=3 route="exec"
Nov 11 08:37:02 su-gamer-r1 aranicy[7525]: 2026-11-11T08:37:02.111304Z  INFO applied rule rule="python3" pid=315168 bias=3 route="exec"
Nov 11 08:38:15 su-gamer-r1 aranicy[7525]: 2026-11-11T08:38:15.275321Z  INFO applied rule rule="systemd-userwork" pid=315181 bias=-1 route="exec"
```

with the result shown live:

```
$ ./machine 'sudo ps -o pid,ppid,ni,comm -e | grep -E "sshd-session|bash" | grep -v grep'
   6893     744   3 sshd-session
   6896    6893   3 sshd-session
   6897    6896   3 bash
```

## What I could not determine

- Whether a name with no rule *should* be biased. The README's formula says
  `clamp(rule_nice + bias, ...)`, which reads as "the bias is added to a rule", but I have
  not seen the design intent beyond that sentence, and I am not going to guess at the
  cause of the difference.
- Whether the `Bulk` verdict on a CPU-bound game is intended. A game does look like a
  compiler from the outside: high CPU, few wakeups. If that is expected, then a
  per-executable "never bias this name" is not a nice-to-have, it is the only thing that
  makes the feature safe for me.
- Why `sshd-session` was `Bulk`+3 in one session and `Responsive`−1 in the next. The
  verdict moves with the rest of the running population, so I cannot give a reproduction
  for the session-demotion part; the `python3` part above reproduces every time.
- I did not measure whether nice 3 on my shell changes anything perceptible on 2 vCPU;
  my complaint is that it happened, not that it was slow.

`workflow this blocks`: "leave the automatic tuning on, but pin one game and be sure that
one program is untouched" — that is the only configuration I actually want, and it is not
expressible today (all-or-nothing `bias_enable`, which is undocumented and restart-only).

`related`: kaz-1 (same mechanism reaching PID 1), kaz-5 (there is no status line that
would have told me this was happening to my game).
