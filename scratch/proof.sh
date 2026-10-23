#!/usr/bin/env bash
# Functional proof (OPT2+OPT3 build): spoofed argv[0] rule application.
# Safety pattern: ananicy-cpp stopped for the window, restored via EXIT trap.
set -u
R=/home/zhangdm/Projects/ananicy-rs
RULES="$R/scratch/proof-rules"
LOG="$R/scratch/proof-daemon.log"
DPID=""

restore() {
    [ -n "$DPID" ] && sudo -n kill "$DPID" 2>/dev/null
    systemctl start ananicy-cpp 2>/dev/null
    echo "restore: ananicy-cpp $(systemctl is-active ananicy-cpp)"
}
trap restore EXIT

mkdir -p "$RULES"
cat > "$RULES/ananicy.conf" <<'EOF'
check_freq = 2
apply_nice = true
apply_ionice = true
apply_sched = true
apply_oom_score_adj = true
loglevel = info
log_applied_rule = true
EOF
echo '{ "name": "gsa-smoke-uniq", "nice": 7, "ioclass": "best-effort", "ionice": 3 }' > "$RULES/zz-proof.rules"

systemctl stop ananicy-cpp
sleep 1
echo "ananicy-cpp active=$(systemctl is-active ananicy-cpp) (must be inactive)"

timeout -s TERM 40 sudo -n env ANANICY_RS_CONFDIR="$RULES" \
    "$R/target/release/aranicy" start >"$LOG" 2>&1 &
sleep 3
DPID=$(pgrep -n -x aranicy || true)
if [ -z "$DPID" ]; then
    echo "FATAL: daemon did not start"
    cat "$LOG"
    exit 1
fi
echo "daemon pid=$DPID exe=$(readlink /proc/$DPID/exe)"

bash -c 'exec -a gsa-smoke-uniq sleep 15' &
TP=$!
echo "target pid=$TP comm=$(cat /proc/$TP/comm 2>/dev/null)"
for i in 1 2 3 4 5; do
    sleep 1
    N=$(ps -o ni= -p "$TP" 2>/dev/null | tr -d ' ')
    I=$(ionice -p "$TP" 2>/dev/null | tr -s ' ')
    echo "t+${i}s nice=${N:-gone} ionice=${I:-gone}"
done
echo "PROOF-CHECK nice=$(ps -o ni= -p "$TP" 2>/dev/null | tr -d ' ') ionice=$(ionice -p "$TP" 2>/dev/null | tr -s ' ')"
echo "daemon log:"
cat "$LOG"
