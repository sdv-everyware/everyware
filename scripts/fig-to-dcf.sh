#!/bin/bash
# fig-to-dcf.sh -- an app's .dcf straight from a .fig, without Figma: no import, no file key,
# no token, no quota. The same importer as fetch-dcf.sh (dcf-fetch in tools/dcf, on our fork of
# DesignCompose), fed by fig2json.js instead of the REST API (dcf-fetch --local).
#
#   vendor/sdv-everyware/everyware/scripts/fig-to-dcf.sh <project>/design/<name>.fig <out.dcf>
#
# The first run builds dcf-fetch (tools/dcf/cargo.sh, the tree's cargo, the fork from GitHub).
#
# The previous .dcf is kept next to it with its time (as fetch-dcf.sh does). The .fig is the
# output of a project's design/build.sh; a file designed by hand in Figma goes through
# fetch-dcf.sh.
#
# Env:
#   DCF_ROOT   root frame (default #<out's name>-root: pairs.dcf -> #pairs-root)
#   DCF_NODES  more top-level frames, space separated (default: Library)
#   DCF_CHECK  more layer names that must be in the result (the root is always checked)
set -euo pipefail
FIG="${1:?usage: fig-to-dcf.sh <file.fig> <out.dcf>}"
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="${2:?usage: fig-to-dcf.sh <file.fig> <out.dcf>}"
FIG=$(realpath "$FIG")
OUT=$(realpath -m "$OUT")
ROOT="${DCF_ROOT:-#$(basename "$OUT" .dcf)-root}"
read -r -a EXTRA <<< "${DCF_NODES-Library}"
read -r -a CHECK <<< "$ROOT ${DCF_CHECK:-}"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
NAME=$(basename "$FIG" .fig)
JSON_DIR="$WORK/$NAME"

echo ">> fig2json: $FIG"
node "$HERE/../design/figma/fig2json.js" "$FIG" "$JSON_DIR"

mkdir -p "$(dirname "$OUT")"
if [ -f "$OUT" ]; then
  BAK="$OUT.$(date -r "$OUT" +%Y%m%d-%H%M)"
  cp -p "$OUT" "$BAK"
  echo ">> previous document kept as $BAK"
fi

NODE_ARGS=(--nodes "$ROOT"); for n in "${EXTRA[@]}"; do NODE_ARGS+=(--nodes "$n"); done
echo ">> importing ${NODE_ARGS[*]} -> $OUT"
"$HERE/../tools/dcf/cargo.sh" run -q --bin dcf-fetch -- --local "$JSON_DIR" "${NODE_ARGS[@]}" --output "$OUT" 2>&1 \
  | grep -v "^\[.*INFO" || true
[ -s "$OUT" ] || { echo "!! no .dcf written"; exit 1; }
ls -la "$OUT"

# Layer names are length-prefixed strings in the .dcf: count them. 0 = the layer is hidden or
# misnamed in the design.
fail=0
for n in "${CHECK[@]}"; do
  c=$(grep -a -o -F "$n" "$OUT" | wc -l || true)   # no match: 0, not an exit (pipefail)
  printf '   %-24s %s\n' "$n" "$c"
  [ "$c" -gt 0 ] || fail=1
done
[ $fail = 0 ] || { echo "!! a checked layer is missing"; exit 1; }
echo ">> done: $(strings -n 6 "$OUT" | grep -m1 -o "$NAME" || echo "$NAME") -> $OUT"
