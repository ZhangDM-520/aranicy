#!/usr/bin/env bash
# build-payload.sh — produce the ONLY thing a synthetic user ever receives.
#
# A payload is a directory containing the two Arch packages built from a
# specific commit, plus MANIFEST.txt. Nothing else: no source tree, no docs,
# no git history, no harness scripts. The clean-room guarantee is structural
# (what is in this directory), not a promise.
#
# Usage:
#   ./build-payload.sh [REF]      # default: HEAD
#   ./build-payload.sh v0.2.3 out/prior   # a prior release, for differential rounds
#
# Output: research/synthetic-users/.cache/payload/<commit>-<short>/ (or the
# second argument), with MANIFEST.txt recording every reproducibility pin.
set -euo pipefail

REF=${1:-HEAD}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
CACHE="$HERE/../.cache"
COMMIT=$(git -C "$ROOT" rev-parse "$REF")
SHORT=$(git -C "$ROOT" rev-parse --short "$REF")
OUT=${2:-"$CACHE/payload/$SHORT-$(date +%Y%m%d)"}

log() { printf '[payload] %s\n' "$*" >&2; }

# --- refuse to build from a dirty tree: the payload must be a commit --------
if [ -n "$(git -C "$ROOT" status --porcelain)" ]; then
    log "refusing: working tree is dirty (the payload must be a committed state)"
    exit 1
fi

WORK="$CACHE/build-$SHORT"
rm -rf "$WORK"
mkdir -p "$WORK" "$OUT"
log "exporting $SHORT"
git -C "$ROOT" archive --prefix=aranicy-0.3.0/ "$COMMIT" | gzip > "$WORK/aranicy-0.3.0.tar.gz"
git -C "$ROOT" show "$COMMIT:contrib/PKGBUILD" > "$WORK/PKGBUILD"

log "building packages (this is the packaging path a user would install)"
( cd "$WORK" && makepkg -f --nocheck >build.log 2>&1 ) || {
    log "makepkg failed; last lines:"; tail -20 "$WORK/build.log" >&2; exit 1
}

local_pkgs=()
for p in "$WORK"/*.pkg.tar.zst; do
    [ -e "$p" ] || { log "no package produced"; exit 1; }
    cp "$p" "$OUT/"
    local_pkgs+=("$(basename "$p")")
done

# --- manifest: every pin needed to interpret a report -----------------------
{
    echo "commit:       $COMMIT"
    echo "ref:          $REF"
    echo "built_at:     $(date -Is)"
    echo "host_kernel:  $(uname -r)"
    echo "makepkg:      $(makepkg --version | head -1)"
    echo "packages:"
    for f in "${local_pkgs[@]}"; do
        printf '  %-45s %s\n' "$f" "$(sha256sum "$OUT/$f" | cut -d' ' -f1)"
    done
    echo
    echo "payload_contents (the entire clean-room surface):"
    ( cd "$OUT" && find . -mindepth 1 -not -name MANIFEST.txt | sort )
} > "$OUT/MANIFEST.txt"

rm -rf "$WORK"
log "payload at $OUT"
cat "$OUT/MANIFEST.txt" >&2
