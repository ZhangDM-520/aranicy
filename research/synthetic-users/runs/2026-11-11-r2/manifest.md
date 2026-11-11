# round 2 — steady use + blind verification

- run: 2026-11-11-r2
- started: 2026-11-11T17:05+08:00
- under test: commit `9a1baa8` (the round-1 fixes), built through the same
  `makepkg` split package
  - `aranicy-0.3.0-1` sha256 `79cd087f2ea43dcb0559ab73f0138d340c46e822f94b890045975d994bf1e5b9`
  - `aranicy-collector-0.3.0-1` sha256 `c201639e13e027df36c36fe1ccadbcb5a5f45a8fb119b25eb9339f6aaadda613`
- environment: 4 × QEMU/KVM, Arch cloud image, guest kernel `7.2.11-arch1-1`,
  2 vCPU / 1958 MiB / no display, one per persona, destroyed after the round
- isolation: card + `sysinfo.txt` + ssh wrapper + the two packages, exactly as
  round 1, plus each persona's **own** round-1 reports (`previous-reports/`).
  Nothing about what was fixed, no other persona's reports.

Round-2 question: do the round-1 reports hold up blind — are the defects gone,
do the fixes hold under the same use, and did any fix break something the
persona relied on?

The personas are told what they reported, never what changed.

## Fix verification before handing the build over (`su-verify-r2`)

| check | observation |
|---|---|
| fresh install with no rules | startup warning `no rules loaded from /etc/ananicy.d…` in the journal |
| PID 1 under load, empty rule set | `/proc/1/stat` nice **0**; `dump bias` shows `systemd … 0 unknown` where round 1 reached `Bulk +3` |
| rule `{"name":"systemd","nice":19}` + reload + drift pass | PID 1 nice **0**, and **zero** `rule="systemd"` applies logged |
| `exec -a spoof /bin/sleep` with a `spoof` rule | nice **0** (no borrowed rule) |
| real `~/spoof` binary | nice **12** (the rule applies to the file) |
| `pacman -R aranicy` | unit `inactive`, daemon gone, unit file gone, `is-enabled` → `not-found` |
| docs | `/usr/share/doc/aranicy/{README,COMPATIBILITY,BENCHMARKS}.md`, collector has its own README |
| `lint` on empty name / `cc1*` / `.json` | three warnings with file:line, exit **1**; `--help` matches |
