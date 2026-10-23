#!/usr/bin/env bash
# Sensitivity/attribution set: B0 = changed build with the busy-spin disabled
# (ANANICY_RS_BUSY_SPIN_US=0): isolates OPT3 (dirfd) + the drain restructure
# from the busy-spin cost. Same protocol/pinning as scratch/ab.sh.
set -u
B=/home/zhangdm/Workspace/aranicy-bench
R=/home/zhangdm/Projects/ananicy-rs
OUT="$B/results-opt23.jsonl"
DPID=""
GOV_RECORDED=""

restore() {
    [ -n "$DPID" ] && sudo -n kill "$DPID" 2>/dev/null
    if [ -n "$GOV_RECORDED" ]; then
        while read -r cpu gov; do
            echo "$gov" | sudo -n tee "/sys/devices/system/cpu/${cpu}/cpufreq/scaling_governor" >/dev/null
        done < "$R/scratch/governors.orig"
        echo "governors restored: $(grep . /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor)"
    fi
    systemctl start ananicy-cpp 2>/dev/null
    echo "restore: ananicy-cpp $(systemctl is-active ananicy-cpp)"
}
trap restore EXIT

for g in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do
    cpu=${g#/sys/devices/system/cpu/}; cpu=${cpu%/cpufreq/scaling_governor}
    echo "$cpu $(cat "$g")"
done > "$R/scratch/governors.orig"
GOV_RECORDED=1
sudo -n cpupower frequency-set -g performance >/dev/null 2>&1
echo "governor: $(grep . /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor)"

systemctl stop ananicy-cpp
sleep 1
echo "ananicy-cpp active=$(systemctl is-active ananicy-cpp) (must be inactive)"

bin="$R/target/release/aranicy"
label="B0"
echo "=== SET $label: $bin spin=0 ==="
timeout -s TERM 1500 sudo -n env ANANICY_RS_CONFDIR="$B/corpus" ANANICY_RS_BUSY_SPIN_US=0 \
    "$bin" start >"$R/scratch/daemon-$label.log" 2>&1 &
sleep 3
DPID=$(pgrep -n -x aranicy || true)
if [ -z "$DPID" ]; then
    echo "FATAL: $label daemon did not start"
    cat "$R/scratch/daemon-$label.log"
    exit 1
fi
echo "$label daemon pid=$DPID"

python3 "$B/run_bench.py" --pid "$DPID" --rate 1 --duration 30 --warmup 0 \
    --reps 1 --label "$label-idle" --out "$OUT"
python3 "$B/run_bench.py" --pid "$DPID" --rate 2000 --duration 60 --warmup 5 \
    --reps 3 --argv0 gsa-storm-probe --label "$label-unruled" --out "$OUT"
python3 "$B/run_bench.py" --pid "$DPID" --rate 2000 --duration 60 --warmup 5 \
    --reps 3 --argv0 ffmpeg --label "$label-ruled" --out "$OUT"

"$B/storm" --rate 2000 --duration 45 --argv0 gsa-storm-probe \
    >"$R/scratch/storm-$label.txt" 2>&1 &
storm_pid=$!
sleep 2
sudo -n perf stat -p "$DPID" -- sleep 30 2>"$R/scratch/perf-$label.txt"
wait "$storm_pid"
echo "$label storm: $(grep -o 'execs=.*' "$R/scratch/storm-$label.txt")"

sudo -n kill "$DPID" 2>/dev/null
sleep 2
DPID=""
echo "=== B0 run complete ==="
