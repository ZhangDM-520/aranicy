#!/usr/bin/env bash
# Repeat perf captures (2nd samples) for A and B0 to quantify instruction-noise.
set -u
B=/home/zhangdm/Workspace/aranicy-bench
R=/home/zhangdm/Projects/ananicy-rs
DPID=""
GOV_RECORDED=""

restore() {
    [ -n "$DPID" ] && sudo -n kill "$DPID" 2>/dev/null
    if [ -n "$GOV_RECORDED" ]; then
        while read -r cpu gov; do
            echo "$gov" | sudo -n tee "/sys/devices/system/cpu/${cpu}/cpufreq/scaling_governor" >/dev/null
        done < "$R/scratch/governors.orig"
        echo "governors restored"
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

systemctl stop ananicy-cpp
sleep 1

capture() {
    local bin="$1" label="$2" spin="$3"
    local spin_env=()
    [ -n "$spin" ] && spin_env=("ANANICY_RS_BUSY_SPIN_US=$spin")
    timeout -s TERM 300 sudo -n env ANANICY_RS_CONFDIR="$B/corpus" "${spin_env[@]}" \
        "$bin" start >"$R/scratch/daemon-$label-3.log" 2>&1 &
    sleep 3
    DPID=$(pgrep -n -x "$(basename "$bin")" || true)
    [ -n "$DPID" ] || { echo "FATAL: $label did not start"; exit 1; }
    "$B/storm" --rate 2000 --duration 40 --argv0 gsa-storm-probe \
        >"$R/scratch/storm-$label-3.txt" 2>&1 &
    local storm_pid=$!
    sleep 2
    sudo -n perf stat -p "$DPID" -- sleep 30 2>"$R/scratch/perf-$label-3.txt"
    wait "$storm_pid"
    sudo -n kill "$DPID" 2>/dev/null
    sleep 2
    DPID=""
}

capture "$R/scratch/aranicy-A" A ""
capture "$R/target/release/aranicy" B0 "0"
echo "=== repeat captures complete ==="
