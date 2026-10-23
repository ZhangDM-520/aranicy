#!/usr/bin/env bash
# Final-binary confirmation: (1) strace evidence that per-event /proc reads go
# through the cached dirfd (openat/readlinkat relative), (2) spoofed-argv[0]
# functional proof, (3) perf sanity capture. ananicy-cpp stopped/restored.
set -u
B=/home/zhangdm/Workspace/aranicy-bench
R=/home/zhangdm/Projects/ananicy-rs
DPID=""
restore() {
    [ -n "$DPID" ] && sudo -n kill "$DPID" 2>/dev/null
    systemctl start ananicy-cpp 2>/dev/null
    echo "restore: ananicy-cpp $(systemctl is-active ananicy-cpp)"
}
trap restore EXIT

systemctl stop ananicy-cpp
sleep 1

echo "=== phase 1: strace + spoofed-name proof ==="
timeout -s TERM 25 sudo -n strace -f -e trace=openat,readlinkat -o "$R/scratch/strace.log" \
    env ANANICY_RS_CONFDIR="$R/scratch/proof-rules" \
    "$R/target/release/aranicy" start >/dev/null 2>&1 &
sleep 3
DPID=$(pgrep -n -x aranicy || true)
[ -n "$DPID" ] || { echo "FATAL: straced daemon did not start"; exit 1; }
echo "straced daemon pid=$DPID"

bash -c 'exec -a gsa-smoke-uniq sleep 12' &
TP=$!
sleep 2
echo "t+2s nice=$(ps -o ni= -p "$TP" | tr -d ' ') ionice=$(ionice -p "$TP" | tr -s ' ')"
sleep 3
echo "t+5s nice=$(ps -o ni= -p "$TP" | tr -d ' ') ionice=$(ionice -p "$TP" | tr -s ' ')"

echo "--- strace: openat/readlinkat via the cached /proc dirfd ---"
command grep -E 'openat\([0-9]+</proc>, "[0-9]+/(cmdline|exe|stat)"|readlinkat\([0-9]+</proc>, "[0-9]+/exe"' "$R/scratch/strace.log" | head -6
echo "--- strace: absolute /proc/<pid> path opens (should be empty) ---"
command grep -E 'openat\(AT_FDCWD, "/proc/[0-9]+/(cmdline|exe|stat)"|readlink\(.*"/proc/[0-9]+/exe"' "$R/scratch/strace.log" | head -6
sudo -n kill "$DPID" 2>/dev/null
sleep 2
DPID=""

echo "=== phase 2: perf sanity (default settings, no env) ==="
timeout -s TERM 300 sudo -n env ANANICY_RS_CONFDIR="$B/corpus" \
    "$R/target/release/aranicy" start >"$R/scratch/daemon-final.log" 2>&1 &
sleep 3
DPID=$(pgrep -n -x aranicy || true)
[ -n "$DPID" ] || { echo "FATAL: final daemon did not start"; exit 1; }
"$B/storm" --rate 2000 --duration 40 --argv0 gsa-storm-probe \
    >"$R/scratch/storm-final.txt" 2>&1 &
storm_pid=$!
sleep 2
sudo -n perf stat -p "$DPID" -- sleep 30 2>"$R/scratch/perf-final.txt"
wait "$storm_pid"
echo "final storm: $(grep -o 'execs=.*' "$R/scratch/storm-final.txt")"
grep -E "context-switches|instructions|cpu-cycles|task-clock" "$R/scratch/perf-final.txt"

sudo -n kill "$DPID" 2>/dev/null
DPID=""
echo "=== confirm complete ==="
