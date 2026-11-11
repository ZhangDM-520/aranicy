#!/usr/bin/env bash
# audit.sh — mechanical clean-room audit over a round's captured artefacts.
#
# A persona must never see the project's own development context. This scans
# every transcript, session log and filed issue for tokens that only exist in
# the repo's internal material, and fails loudly on a hit. A hit invalidates
# that persona's round: re-run it in a fresh environment.
#
# Usage: ./audit.sh <dir> [<dir>...]
#   exit 0  clean
#   exit 1  at least one leak (printed as file:line:match)
#   exit 2  bad usage / nothing to scan (an empty scan is not a pass)
set -euo pipefail

[ $# -gt 0 ] || { echo "usage: $0 <dir>..." >&2; exit 2; }

# The split is measured, not assumed: a token belongs in HARD only if it
# appears NOWHERE in the shipped package (README, docs/, units). Anything the
# package documents (crates/, bias_shadow, aranicy_wire, ...) is a legitimate
# quote and only warns. Check with:
#   grep -lE '<token>' README.md docs/*.md contrib/*.service
# Matched case-sensitively: `JOURNAL` is a filename, `journalctl` a user's tool.
HARD=(
    'AGENTS\.md'
    'JOURNAL'
    'scratchpad'
    'plan_locked'
    'synthetic-users'
    'persona (card|brief)'
    '/home/zhangdm'
)


# Vocabulary that could plausibly leak a design hint but might also be a user's
# own words. Reported, never fatal.
SOFT=(
    'clean-room'   # the shipped README calls itself one
    # Present in the shipped documentation, so quoting the package is not a
    # leak: COMPATIBILITY.md/BENCHMARKS.md ship under /usr/share/doc/aranicy
    # and name these on purpose.
    'crates/'
    'bias_shadow'
    'attack_min_util'
    'aranicy_wire'
    'SAMPLE_CAP'
    'ZhangDM-520'
    'scx_stats'
    'bias engine'
    'shadow classif'
    'classifier'
    'scx_lavd'
    'harness'
    'subagent'
)


files=0
hard_hits=0
soft_hits=0

for dir in "$@"; do
    while IFS= read -r -d '' f; do
        files=$((files + 1))
        for pat in "${HARD[@]}"; do
            if grep -nE -- "$pat" "$f" >/dev/null 2>&1; then
                grep -nE -- "$pat" "$f" | sed "s|^|LEAK $f:|" || true
                hard_hits=$((hard_hits + 1))
            fi
        done
        for pat in "${SOFT[@]}"; do
            if grep -nEi -- "$pat" "$f" >/dev/null 2>&1; then
                grep -nEi -- "$pat" "$f" | sed "s|^|warn $f:|" || true
                soft_hits=$((soft_hits + 1))
            fi
        done
    done < <(find "$dir" -type f \( -name '*.md' -o -name '*.log' -o -name '*.txt' \
             -o -name '*.jsonl' -o -name '*.json' \) -print0)
done

if [ "$files" -eq 0 ]; then
    echo "audit: no files found — an empty scan is not a pass" >&2
    exit 2
fi

echo "audit: scanned $files file(s), $hard_hits leak(s), $soft_hits warning(s)"
[ "$hard_hits" -eq 0 ] || exit 1
