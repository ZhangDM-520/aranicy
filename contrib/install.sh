#!/usr/bin/env bash
# install.sh — generic installer for aranicy (binary + systemd unit + config).
#
# Installs aranicy the way ananicy-cpp lives on a system: /usr/bin/aranicy,
# a systemd unit, and /etc/ananicy.d/ananicy.conf alongside the existing
# rules corpus (never touched). Replaces ananicy-cpp by default (stop+disable);
# keep it with --keep-ananicy-cpp.
#
# Usage:
#   sudo ./contrib/install.sh [options]
#     --binary <path>      binary to install (default: build or
#                          <repo>/target/release/aranicy)
#     --prefix <dir>       install prefix (default /usr)
#     --no-enable          install but do not enable/start the service
#     --keep-ananicy-cpp   do not stop/disable ananicy-cpp.service
#     --uninstall          remove binary + unit (config and rules are kept)
#     --dry-run            print actions without changing anything
#
# Idempotent: safe to re-run. A pre-existing /etc/ananicy.d/ananicy.conf is
# never overwritten (the rules corpus and its config belong to the admin).
set -euo pipefail

ORIG_ARGS=("$@")
PREFIX=/usr
SYSCONFDIR=/etc
ENABLE=1
REPLACE_CPP=1
UNINSTALL=0
DRY=0
BIN_SRC=""

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(dirname "$SCRIPT_DIR")

log()  { echo "[aranicy-install] $*"; }
run()  { if [ "$DRY" = 1 ]; then log "DRY-RUN: $*"; else log "+ $*"; "$@"; fi; }

usage() { sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'; }

while [ $# -gt 0 ]; do
    case "$1" in
        --binary)           BIN_SRC="$2"; shift 2 ;;
        --prefix)           PREFIX="$2"; shift 2 ;;
        --no-enable)        ENABLE=0; shift ;;
        --keep-ananicy-cpp) REPLACE_CPP=0; shift ;;
        --uninstall)        UNINSTALL=1; shift ;;
        --dry-run)          DRY=1; shift ;;
        -h|--help)          usage; exit 0 ;;
        *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

BINDIR="$PREFIX/bin"
UNITDIR="$PREFIX/lib/systemd/system"

# Re-exec as root unless dry-run (which must work unprivileged).
if [ "$(id -u)" -ne 0 ] && [ "$DRY" = 0 ]; then
    log "root required for install; re-executing with sudo"
    exec sudo -n "$0" "${ORIG_ARGS[@]}"
fi

if [ "$UNINSTALL" = 1 ]; then
    log "uninstalling aranicy from $PREFIX (config and rules are kept)"
    run systemctl disable --now aranicy.service 2>/dev/null || true
    run rm -f "$BINDIR/aranicy" "$UNITDIR/aranicy.service"
    run systemctl daemon-reload
    log "uninstalled; /etc/ananicy.d left untouched"
    exit 0
fi

# --- resolve the binary -----------------------------------------------------
if [ -z "$BIN_SRC" ]; then
    if [ -x "$REPO_ROOT/target/release/aranicy" ]; then
        BIN_SRC="$REPO_ROOT/target/release/aranicy"
    else
        log "no prebuilt binary; building release in $REPO_ROOT"
        ( cd "$REPO_ROOT" && cargo build --release )
        BIN_SRC="$REPO_ROOT/target/release/aranicy"
    fi
fi
[ -x "$BIN_SRC" ] || { echo "error: binary not executable: $BIN_SRC" >&2; exit 1; }
[ -f "$REPO_ROOT/contrib/ananicy.service" ] || { echo "error: contrib/ananicy.service missing" >&2; exit 1; }

log "binary:  $BIN_SRC"
log "target:  $BINDIR/aranicy"
log "unit:    $UNITDIR/aranicy.service"

# --- install ----------------------------------------------------------------
run install -Dm755 "$BIN_SRC" "$BINDIR/aranicy"
run install -Dm644 "$REPO_ROOT/contrib/ananicy.service" "$UNITDIR/ananicy.service"

if [ -f "$SYSCONFDIR/ananicy.d/ananicy.conf" ]; then
    log "keeping existing $SYSCONFDIR/ananicy.d/ananicy.conf (rules corpus untouched)"
else
    run install -Dm644 "$REPO_ROOT/contrib/ananicy.conf" "$SYSCONFDIR/ananicy.d/ananicy.conf"
fi

run systemctl daemon-reload

# --- service transition -----------------------------------------------------
if [ "$REPLACE_CPP" = 1 ] && systemctl list-unit-files ananicy-cpp.service >/dev/null 2>&1; then
    if systemctl is-enabled ananicy-cpp.service >/dev/null 2>&1 \
        || systemctl is-active ananicy-cpp.service >/dev/null 2>&1; then
        log "replacing ananicy-cpp.service (stop + disable)"
        run systemctl disable --now ananicy-cpp.service
    fi
fi

if [ "$ENABLE" = 1 ]; then
    run systemctl enable --now aranicy.service
    if [ "$DRY" = 0 ]; then
        sleep 1
        STATE=$(systemctl is-active aranicy.service 2>/dev/null || true)
        log "aranicy.service: $STATE"
        [ "$STATE" = "active" ] || {
            echo "error: aranicy.service failed to start; see: journalctl -u aranicy" >&2
            exit 1
        }
    fi
else
    log "service installed but not enabled (--no-enable)"
fi

log "done"
