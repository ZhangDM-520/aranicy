# The key list in COMPATIBILITY.md omits a key that works, does not classify the one key that solves my override problem, and documents `autogroup` without the range that decides whether my rule is loaded at all

- severity: minor
- category: docs
- environment: Arch Linux rolling, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Get the full list of things I can put in `ananicy.conf` and, for each one, find
out whether editing it needs a restart. That is the first thing I do with a
daemon I am going to leave running. The shipped `/usr/share/doc/aranicy/COMPATIBILITY.md`
has a "Config keys (all `bias_*`)" paragraph and a "Reload liveness" section; I
used those as the list.

## Steps to reproduce

```
grep -n "Config keys (all" -A7 /usr/share/doc/aranicy/COMPATIBILITY.md
grep -n "bias_attack_min_util" /usr/share/doc/aranicy/COMPATIBILITY.md
awk '/^### Reload liveness/,/^## CLI/' /usr/share/doc/aranicy/COMPATIBILITY.md | grep -c bias_protect
grep -n "attack_min_util\|bias_protect" /etc/ananicy.d/ananicy.conf
mkdir -p ~/t2 && echo "bias_attack_min_util = 512" > ~/t2/ananicy.conf
aranicy --config-dir ~/t2 lint
mkdir -p ~/t3 && echo "bias_totally_bogus = 1" > ~/t3/ananicy.conf
aranicy --config-dir ~/t3 lint
echo '{"name": "kazauto", "autogroup": 500}' | sudo tee /etc/ananicy.d/92-kazauto.rules
aranicy lint
```

## Expected

Three things:

1. A key that the prose tells me to tune (the demand gate) appears in the list of
   keys, or the prose does not name a key at all.
2. A key that changes what gets applied to a process is classified in the reload
   table, so I know whether `systemctl reload` is enough.
3. A numeric field documents its range, so I do not write a value that makes my
   rule disappear.

## Actual

1. `bias_attack_min_util` is named in the prose (line 114) but is **not** in the
   key list (lines 243–249) and is not in the shipped config. It is nevertheless a
   real key — `lint` accepts it and rejects a made-up one, so this is the
   documented list being incomplete, not the prose inventing a key.
2. `bias_protect` is in the key list but appears **zero** times in the "Reload
   liveness" section — it is in none of live / reconfigurable / restart-only. It
   is in fact live: I edited it, reloaded, and the rule's nice came back on a
   running process within one drift pass. Because it is the key that decides
   whether the learned bias is allowed to touch a name, not knowing whether it
   needs a restart is the difference between "this works" and "this did nothing".
3. `autogroup` is documented as "`autogroup` | int | **superset**: writes the
   process autogroup nice" with no range. The real range is −20…19, and a value
   outside it drops the **whole line**, so the rule I wrote for that process does
   not exist. `lint` does catch it (unlike #1 and #2, this one is only a missing
   sentence, not a hazard — I am listing it because I had to hit it to learn the
   range).

## Evidence

1. Prose vs list:

```
$ ./machine 'grep -n "attack_min_util\|^Config keys (all" /usr/share/doc/aranicy/COMPATIBILITY.md; sed -n "243,249p" /usr/share/doc/aranicy/COMPATIBILITY.md'
114:where 1024 is one core — to reach `bias_attack_min_util` (default 512, i.e.
243:Config keys (all `bias_*`): `bias_enable`, `bias_min`, `bias_max`,
Config keys (all `bias_*`): `bias_enable`, `bias_min`, `bias_max`,
`bias_window_s`, `bias_deadband_pct`, `bias_min_samples`, `bias_cadence_ms`,
`bias_max_entries`, `bias_half_life_s`, `bias_state_path`,
`bias_collector_socket`, `bias_starved_min_demand`, `bias_starved_ratio`,
`bias_shadow`, `bias_lean_stage_min_ns`, `bias_lean_hop_hz`, `bias_protect`, plus
the non-`bias_*` collector keys `collector_events_socket` and
`collector_wait_ms`, and repeatable `bias_group.<tag> = <regex>` family
```

It is a real key:

```
$ ./machine 'mkdir -p ~/t2; printf "%s\n" "bias_attack_min_util = 512" > ~/t2/ananicy.conf; aranicy --config-dir ~/t2 lint; echo "EXIT=$?"'
OK: 0 rules, 0 types, 0 cgroups
EXIT=0
$ ./machine 'printf "%s\n" "bias_totally_bogus = 1" > ~/t3/ananicy.conf; aranicy --config-dir ~/t3 lint; echo "EXIT=$?"'
warning: config: line 1: unknown key "bias_totally_bogus"
lint: warnings found
EXIT=1
```

and it is nowhere in the shipped config, so the only place it exists is one
sentence of prose:

```
$ ./machine 'grep -n "attack_min_util" /etc/ananicy.d/ananicy.conf; echo "EXIT=$?"'
EXIT=1
```

2. `bias_protect` unclassified, but live:

```
$ ./machine 'awk "/^### Reload liveness/,/^## CLI/" /usr/share/doc/aranicy/COMPATIBILITY.md | grep -c "bias_protect"'
0
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

(that process was at −3 immediately before the edit; the reload is what fixed it,
with no restart.)

3. `autogroup` range:

```
$ ./machine 'mkdir -p ~/t4; printf "%s\n" "{\"name\": \"kazauto\", \"autogroup\": 500}" > ~/t4/a.rules; printf "%s\n" "check_freq = 60" > ~/t4/ananicy.conf; aranicy --config-dir ~/t4 lint; echo "EXIT=$?"'
warning: /home/arch/t4/a.rules:1: autogroup: 500 out of range -20..=19
lint: warnings found
EXIT=1
```

`lint` catches it, so this part is only "document the range I had to guess"; the
daemon's own reload warning is the same text:

```
$ ./machine 'sudo systemctl reload aranicy; sleep 2; sudo journalctl -u aranicy --no-pager --since "-1min" | grep autogroup'
Nov 11 09:14:52 su-gamer-r2 aranicy[1827]: 2026-11-11T09:14:52.215934Z  WARN /etc/ananicy.d/92-kazauto.rules:1: autogroup: 500 out of range -20..=19
```

I guessed 0…1000, which is what `/proc/<pid>/autogroup` looks like it should take
to a user.

## What I could not determine

- Whether `bias_attack_min_util` is meant to be user-facing at all. It is
  described with a default and an off-switch, which reads as user-facing, and
  `lint` accepts it, but the key list says otherwise.
- Whether the "Reload liveness" table is meant to be exhaustive ("Every key is
  classified exactly once"). If it is, exactly two of the keys I used are not
  classified — `bias_protect` and `bias_attack_min_util` — and I checked that by
  grepping each key from the list against that section:

```
$ ./machine 'D=/usr/share/doc/aranicy/COMPATIBILITY.md; for k in bias_enable bias_min bias_max bias_window_s bias_deadband_pct bias_min_samples bias_cadence_ms bias_max_entries bias_half_life_s bias_state_path bias_collector_socket bias_starved_min_demand bias_starved_ratio bias_shadow bias_lean_stage_min_ns bias_lean_hop_hz bias_protect bias_attack_min_util; do n=$(awk "/^### Reload liveness/,/^## CLI/" $D | grep -c "$k"); printf "%-24s %s\n" "$k" "$n"; done'
bias_enable              1
bias_min                 3
bias_max                 3
bias_window_s            1
bias_deadband_pct        1
bias_min_samples         1
bias_cadence_ms          1
bias_max_entries         1
bias_half_life_s         1
bias_state_path          1
bias_collector_socket    1
bias_starved_min_demand  1
bias_starved_ratio       1
bias_shadow              1
bias_lean_stage_min_ns   1
bias_lean_hop_hz         1
bias_protect             0
bias_attack_min_util     0
```

- Whether `lint` is supposed to catch everything that will stop a rule loading. It
  caught the `autogroup` range, which is the one case here where it did.

`workflow this blocks`: "read the docs, write the config, reload, know it took" —
I had to test each of these three by hand to find out.
