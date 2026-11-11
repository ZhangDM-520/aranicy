# round 1 — cold discovery

- run: 2026-11-11-r1
- started: 2026-11-11T16:10+08:00
- under test: commit `cfd45ad906ddc03ec09f6d56d6d252a16ec6c856` (v0.3.0)
- payload: `aranicy-0.3.0-1-x86_64.pkg.tar.zst` sha256 `467d6dcf…589d7f`,
  `aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst` sha256 `ba4cf0a1…4276f0b`
- environment: 4 × QEMU/KVM, Arch cloud image, guest kernel `7.2.11-arch1-1`,
  2 vCPU / 1958 MiB / no display, one per persona, destroyed after the round
- isolation: each persona got only a card, `sysinfo.txt`, an ssh wrapper and
  the two packages; the packages were pre-placed in `~/payload` inside the VM.
  No repo, docs, source, git history or session state was reachable.
- assignment (fixed, not re-rolled): gamer→`su-gamer-r1`, dev→`su-dev-r1`,
  homelab→`su-homelab-r1`, skeptic→`su-skeptic-r1`

Round-1 question: can a competent stranger get value out of the shipped
package, and what stops them?

Cards as handed over: `persona-cards/<id>.md`. Issues land in `issues/`.
