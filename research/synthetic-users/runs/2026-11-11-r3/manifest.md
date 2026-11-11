# round 3 — adversarial + differential

- run: 2026-11-11-r3
- started: 2026-11-11T18:25+08:00
- under test: commit `a72730d` (round-2 fixes)
  - `aranicy-0.3.0-1` sha256 `b3d851f51c82…`
  - `aranicy-collector-0.3.0-1` sha256 `626bce71bc2f…`
- comparator: `9a1baa8` (the round-1 build, i.e. what round 2 tested),
  `79cd087f2ea4…` / `c201639e13e0…`, provided in `~/previous-build`
- environment: 4 × QEMU/KVM, Arch cloud image, guest kernel `7.2.11-arch1-1`,
  2 vCPU / 1958 MiB, one per persona, destroyed afterwards
- isolation: card + `sysinfo.txt` + ssh wrapper + both builds + the persona's
  own round-2 reports. No statement of what changed between the builds.

Two questions: does it fail **loudly** under conditions a real machine
produces (junk config, non-root, killed peer, reboot, install/remove/reinstall,
renamed and recycled processes), and what changed for the worse between the
two builds the persona has actually used?
