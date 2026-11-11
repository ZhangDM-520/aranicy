# synthetic-users — outside-user testing harness

Four personas, three rounds, one clean-room environment per persona per round.
The point is to reproduce how a project improves from *outside* issue reports:
users who know nothing about the design use the shipped package, file issues,
and the project triages, fixes and re-verifies them.

## The clean-room guarantee

A persona receives **only** a payload directory:

| path | what |
|---|---|
| `aranicy-<ver>-x86_64.pkg.tar.zst` | the daemon + units + config |
| `aranicy-collector-<ver>-x86_64.pkg.tar.zst` | the BPF collector |
| `MANIFEST.txt` | commit, sha256, build host, payload file list |

Nothing else — no source tree, no `.git`, no `AGENTS.md`, no `docs/`, no
`.logs/`, no harness scripts. This is structural: the payload directory is
built by `harness/build-payload.sh` from `git archive` of a commit, and only
that directory is mounted into the environment.

A mechanical audit runs after every round: every transcript and issue file is
grepped for `AGENTS`, `JOURNAL`, `crates/`, `bias_shadow`, `attack_min_util`,
`synthetic-users`, the repo path and the owner's session ids. A hit invalidates
that persona's round and it is re-run.

## Two tiers (this is a safety boundary, not a convenience)

| tier | tool | can judge | must NOT judge |
|---|---|---|---|
| **A** | `harness/container.sh` (rootless podman, `archlinux:latest`) | install, CLI, config parsing, shipped docs *presence in the package*, loud-failure messages, uninstall | anything requiring eBPF or real `nice` — a container either cannot load BPF (EPERM) or, privileged, would see the **host's** events and could nice the host's processes |
| **B** | `harness/vm.sh` (QEMU + KVM, Arch cloud image) | everything: systemd, eBPF, `/proc`, cgroups, nice, real workloads | — (own kernel, so contained) |

Known Tier-A quirk on this host: pacman records `/usr/share/doc/...` in its
database but does not write the files to disk, for *any* package (verified with
a control package). Never judge the installed file set from Tier A; the VM is
authoritative.

## Quick start

```sh
# 1. one payload per commit under test (refuses a dirty tree)
./harness/build-payload.sh                 # HEAD
./harness/build-payload.sh v0.2.3 out/prior

# 2. tier A: what a user sees immediately
./harness/container.sh probe .cache/payload/<short>-<date>

# 3. tier B: one disposable VM per persona
./harness/vm.sh create su-gamer-1
./harness/vm.sh start  su-gamer-1 && ./harness/vm.sh wait su-gamer-1
./harness/vm.sh push   su-gamer-1 payload/*.zst
./harness/vm.sh ssh    su-gamer-1 'sudo pacman -U --noconfirm /tmp/*.zst'
./harness/vm.sh ssh    su-gamer-1 'systemctl start aranicy'
./harness/vm.sh destroy su-gamer-1
```

The base cloud image lives in `.cache/` (gitignored). Fetch it with:

```sh
curl -L -o .cache/Arch-Linux-x86_64-cloudimg.qcow2 \
  https://geo.mirror.pkgbuild.com/images/latest/Arch-Linux-x86_64-cloudimg.qcow2
```

## Layout

```
personas/<id>.md      the capability brief handed to one agent (versioned)
harness/              build-payload, vm, container
runs/<date>-r<N>/     manifest, the persona cards used, issues as filed
```

Raw transcripts stay out of git (large, and they contain the full session).
