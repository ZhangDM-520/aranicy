#!/usr/bin/env bash
# container.sh — Tier A: the fast, safe, no-BPF checks.
#
# What this tier CAN judge: package install, CLI surface, config parsing,
# shipped documentation presence in the PACKAGE, loud-failure behaviour,
# uninstall.
# What it MUST NOT judge: scheduling, nice, bias, cgroups — eBPF is not
# namespaced, so a container either cannot load the programs (no capability)
# or would see the HOST's events (with it). Behaviour belongs to vm.sh.
#
# Known quirk on this host: pacman records /usr/share/doc/* in its database but
# writes nothing to disk, for ANY package (verified with a control package).
# Never judge the installed file set here: use `install` for the package
# *contents*, and the VM for the real on-disk result.
#
# Subcommands:
#   install <payload-dir>   fresh container, pacman -U, list what a user has
#   probe   <payload-dir>   install + every no-privilege command a user runs
#   shell   <payload-dir>   drop into a shell with the package installed
set -euo pipefail

IMAGE=${SU_IMAGE:-docker.io/library/archlinux:latest}

die() { printf '[container] error: %s\n' "$*" >&2; exit 1; }

check_payload() {
    [ -d "${1:-}" ] || die "no such payload dir: ${1:-<none>}"
    local n
    n=$(find "$1" -maxdepth 1 -name '*.pkg.tar.zst' | wc -l)
    [ "$n" -ge 2 ] || die "payload needs both packages, found $n"
}

# The inner script arrives on stdin, so no host-side expansion can rewrite it:
# a host glob here would install the wrong thing, or nothing, silently.
run_script() {
    podman run --rm -i -v "$1:/pkg:ro,Z" "$IMAGE" bash -s
}

cmd_install() {
    check_payload "${1:-}"
    echo "== payload: $1"
    run_script "$1" <<'EOF'
set -u
pacman -U --noconfirm /pkg/*.pkg.tar.zst >/tmp/install.log 2>&1 || { cat /tmp/install.log; exit 1; }
echo '== installed =='
pacman -Q aranicy aranicy-collector 2>/dev/null
echo '== files the PACKAGE declares =='
pacman -Ql aranicy aranicy-collector 2>/dev/null | awk '{print $2}' | grep -v '/$'
EOF
}

cmd_probe() {
    check_payload "${1:-}"
    run_script "$1" <<'EOF'
set -u
pacman -U --noconfirm /pkg/*.pkg.tar.zst >/tmp/install.log 2>&1 || { cat /tmp/install.log; exit 1; }
echo '== aranicy --version =='; aranicy --version
echo '== aranicy lint =='; aranicy lint; echo "lint rc=$?"
echo '== aranicy dump rules =='; aranicy dump rules | head -20
echo '== aranicy dump types (head) =='; aranicy dump types 2>&1 | head -5
echo '== match-test usage =='; aranicy match-test --help 2>&1 | head -6
echo '== start without a collector =='; timeout 12 aranicy start 2>&1 | tail -4
echo '== collector probe without capabilities =='; timeout 12 aranicy-collectord probe 2>&1 | tail -4
echo '== collector help =='; aranicy-collectord --help 2>&1 | head -6
echo '== shipped docs, as the PACKAGE declares them =='
pacman -Ql aranicy aranicy-collector 2>/dev/null | grep -E 'doc|LICENSE' || true
echo '== uninstall =='; pacman -R --noconfirm aranicy aranicy-collector >/dev/null 2>&1 && echo 'uninstall ok'
EOF
}

cmd_shell() {
    check_payload "${1:-}"
    podman run --rm -it -v "$1:/pkg:ro,Z" "$IMAGE" bash -c \
        'pacman -U --noconfirm /pkg/*.pkg.tar.zst >/dev/null 2>&1; exec bash'
}

case "${1:-}" in
    install) shift; cmd_install "$@" ;;
    probe) shift; cmd_probe "$@" ;;
    shell) shift; cmd_shell "$@" ;;
    *) sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
