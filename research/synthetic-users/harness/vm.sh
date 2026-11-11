#!/usr/bin/env bash
# vm.sh — one disposable, kernel-isolated environment per synthetic user.
#
# Why a VM and not a container: eBPF is not namespaced and aranicy applies
# nice to the pids it resolves. A privileged container would collect the
# HOST's exec events and could nice the host's processes (including the
# user's desktop and the production daemon). Own kernel = contained.
#
# Subcommands:
#   create <name> [mem_mb] [cpus]   new overlay disk + cloud-init seed
#   start  <name>                   boot it (detached), print the ssh port
#   wait   <name>                   block until ssh answers
#   ssh    <name> <cmd...>          run one command as user `arch` (passwordless sudo)
#   push   <name> <local> [dest]    copy a file in (default /tmp)
#   pull   <name> <remote> <local>  copy a file out (evidence capture)
#   stop   <name>                   power off
#   destroy <name>                  delete disk, seed, logs
#   list                            what exists
#   port   <name>                   the ssh port
#
# State lives in .cache/vms/<name>/ (gitignored). The base cloud image is
# shared, read-only, and never modified: each VM gets a qcow2 overlay.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
CACHE="$HERE/../.cache"
BASE="$CACHE/Arch-Linux-x86_64-cloudimg.qcow2"
KEY="$CACHE/id_ed25519"
VMS="$CACHE/vms"

log() { printf '[vm] %s\n' "$*" >&2; }
die() { log "error: $*"; exit 1; }

need_base() {
    [ -s "$BASE" ] || die "base image missing: $BASE (see README)"
    [ -s "$KEY" ] || { log "generating ssh key"; ssh-keygen -q -t ed25519 -N '' -f "$KEY"; }
}

# Deterministic per-VM port so a persona's session can be resumed, and so two
# concurrent VMs never collide.
port_of() {
    local sum
    sum=$(printf '%s' "$1" | cksum | cut -d' ' -f1)
    echo $(( 22000 + sum % 400 ))
}

dirname_of() { echo "$VMS/$1"; }

cmd_create() {
    local name=${1:?name} mem=${2:-2048} cpus=${3:-2}
    local d; d=$(dirname_of "$name")
    [ -e "$d" ] && die "$name already exists (destroy it first)"
    need_base
    mkdir -p "$d"
    log "creating $name (${mem}MB, ${cpus} vcpu)"
    qemu-img create -q -f qcow2 -F qcow2 -b "$BASE" "$d/disk.qcow2"

    local port; port=$(port_of "$name")
    cat > "$d/user-data" <<EOF
#cloud-config
hostname: $name
users:
  - name: arch
    sudo: ALL=(ALL) NOPASSWD:ALL
    shell: /bin/bash
    lock_passwd: false
    ssh_authorized_keys:
      - $(cat "$KEY.pub")
ssh_pwauth: false
package_update: false
runcmd:
  - [ systemctl, enable, --now, sshd ]
EOF
    cat > "$d/meta-data" <<EOF
instance-id: $name
local-hostname: $name
EOF
    genisoimage -quiet -output "$d/seed.iso" -volid cidata -joliet -rock \
        "$d/user-data" "$d/meta-data"
    echo "$port" > "$d/port"
    echo "$mem" > "$d/mem"
    echo "$cpus" > "$d/cpus"
    log "created; ssh port $port"
}

cmd_start() {
    local name=${1:?name}
    local d; d=$(dirname_of "$name")
    [ -d "$d" ] || die "no such VM: $name"
    [ -f "$d/qemu.pid" ] && kill -0 "$(cat "$d/qemu.pid")" 2>/dev/null && { log "already running"; cmd_port "$name"; return; }
    local port mem cpus
    port=$(cat "$d/port"); mem=$(cat "$d/mem"); cpus=$(cat "$d/cpus")
    log "booting $name"
    : > "$d/serial.log"
    qemu-system-x86_64 \
        -name "aranicy-su-$name" \
        -enable-kvm -cpu host -smp "$cpus" -m "$mem" \
        -drive "file=$d/disk.qcow2,if=virtio,format=qcow2" \
        -drive "file=$d/seed.iso,if=virtio,format=raw,readonly=on" \
        -netdev "user,id=net0,hostfwd=tcp:127.0.0.1:$port-:22" \
        -device virtio-net-pci,netdev=net0 \
        -display none -serial "file:$d/serial.log" \
        -pidfile "$d/qemu.pid" -daemonize
    echo "$port"
}

cmd_wait() {
    local name=${1:?name} timeout=${2:-90}
    local d; d=$(dirname_of "$name"); local port; port=$(cat "$d/port")
    local start=$SECONDS
    while :; do
        if ssh -i "$KEY" -p "$port" -o BatchMode=yes -o ConnectTimeout=2 \
               -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
               -o LogLevel=ERROR arch@127.0.0.1 true 2>/dev/null; then
            log "$name up after $((SECONDS-start))s (port $port)"; return 0
        fi
        if [ $((SECONDS-start)) -gt "$timeout" ]; then
            log "timed out waiting for ssh; serial tail:"
            tail -20 "$d/serial.log" >&2
            return 1
        fi
        sleep 2
    done
}

cmd_ssh() {
    local name=${1:?name}; shift
    local d; d=$(dirname_of "$name"); local port; port=$(cat "$d/port")
    ssh -i "$KEY" -p "$port" -o BatchMode=yes -o StrictHostKeyChecking=no \
        -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR \
        arch@127.0.0.1 -- "$@"
}

cmd_push() {
    local name=${1:?name} src=${2:?src} dest=${3:-/tmp/}
    local d; d=$(dirname_of "$name"); local port; port=$(cat "$d/port")
    scp -q -i "$KEY" -P "$port" -o StrictHostKeyChecking=no \
        -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR "$src" "arch@127.0.0.1:$dest"
}

cmd_pull() {
    local name=${1:?name} remote=${2:?remote} local_dest=${3:?dest}
    local d; d=$(dirname_of "$name"); local port; port=$(cat "$d/port")
    scp -q -r -i "$KEY" -P "$port" -o StrictHostKeyChecking=no \
        -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR "arch@127.0.0.1:$remote" "$local_dest"
}

cmd_stop() {
    local name=${1:?name}
    local d; d=$(dirname_of "$name")
    if [ -f "$d/qemu.pid" ]; then
        local pid; pid=$(cat "$d/qemu.pid")
        kill -0 "$pid" 2>/dev/null && { log "stopping $name ($pid)"; kill "$pid"; sleep 2; }
        kill -0 "$pid" 2>/dev/null && kill -9 "$pid"
        rm -f "$d/qemu.pid"
    fi
}

cmd_destroy() {
    local name=${1:?name}
    cmd_stop "$name" || true
    local d; d=$(dirname_of "$name")
    [ -d "$d" ] && { rm -rf "$d"; log "destroyed $name"; }
}

cmd_list() {
    [ -d "$VMS" ] || { echo "(none)"; return; }
    for d in "$VMS"/*; do
        [ -d "$d" ] || continue
        local n; n=$(basename "$d")
        local st=running
        [ -f "$d/qemu.pid" ] && kill -0 "$(cat "$d/qemu.pid")" 2>/dev/null || st=stopped
        printf '%-20s %-8s port %s  disk %s\n' "$n" "$st" \
            "$(cat "$d/port" 2>/dev/null || echo -)" \
            "$(du -h "$d/disk.qcow2" 2>/dev/null | cut -f1)"
    done
}

cmd_port() { cat "$(dirname_of "${1:?name}")/port"; }

case "${1:-}" in
    create) shift; cmd_create "$@" ;;
    start) shift; cmd_start "$@" ;;
    wait) shift; cmd_wait "$@" ;;
    ssh) shift; cmd_ssh "$@" ;;
    push) shift; cmd_push "$@" ;;
    pull) shift; cmd_pull "$@" ;;
    stop) shift; cmd_stop "$@" ;;
    destroy) shift; cmd_destroy "$@" ;;
    list) shift; cmd_list ;;
    port) shift; cmd_port "$@" ;;
    *) sed -n '2,26p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
