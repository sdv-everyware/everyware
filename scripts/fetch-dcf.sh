#!/bin/bash
# fetch-dcf.sh -- an app's .dcf from a file in Figma (the other way is fig-to-dcf.sh: from a
# .fig, without Figma).
#
# The node-scoped fetcher dcf-fetch (tools/dcf, on our fork of DesignCompose: the .dcf has the
# format version harry expects): the root frame by name and the Library, hidden layers skipped.
# The previous .dcf is kept next to it as <out>.dcf.<timestamp>. The first run builds dcf-fetch
# (tools/dcf/cargo.sh, the tree's cargo, the fork from GitHub).
#
# Usage:
#   FIGMA_TOKEN=... vendor/sdv-everyware/everyware/scripts/fetch-dcf.sh <out.dcf> [DOC_ID]
#   e.g. .../fetch-dcf.sh packages/services/display_safety/utils/pairs-app/data/pairs.dcf <key>
#   DOC_ID = file key from the Figma URL figma.com/design/<DOC_ID>/<name>; without it the key in
#   scripts/figma-key (the file imported last; a local file, in .gitignore: no key in git).
#
# Env:
#   DCF_ROOT   root frame to fetch (default #<out's name>-root: pairs.dcf -> #pairs-root)
#   DCF_NODES  more top-level frames, space separated (default: Library, the frame of the
#              Components page: the component sets the instances use)
#   DCF_CHECK  more layer names that must be in the result (the root is always checked)
#
# Never put the token on the command line or in a file: export it.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
TREE="$(cd "$HERE/../../../.." && pwd)"
OUT="${1:?usage: FIGMA_TOKEN=... fetch-dcf.sh <out.dcf> [DOC_ID]}"
OUT=$(realpath -m "$OUT")
DOC_ID="${2:-${DOC_ID:-$(cat "$(dirname "$0")/figma-key" 2>/dev/null || true)}}"
ROOT="${DCF_ROOT:-#$(basename "$OUT" .dcf)-root}"
read -r -a EXTRA <<< "${DCF_NODES-Library}"
read -r -a CHECK <<< "$ROOT ${DCF_CHECK:-}"
[ -n "$DOC_ID" ] || { echo "usage: FIGMA_TOKEN=... $0 <out.dcf> [DOC_ID]"; exit 1; }
[ -n "${FIGMA_TOKEN:-}" ] || { echo "!! FIGMA_TOKEN is not set (export it, do not paste it into scripts)"; exit 1; }

mkdir -p "$(dirname "$OUT")"
if [ -f "$OUT" ]; then
  BAK="$OUT.$(date -r "$OUT" +%Y%m%d-%H%M)"
  cp -p "$OUT" "$BAK"
  echo ">> previous document kept as $BAK"
fi

# Cheap /meta call first (no file-content quota): a wrong key, a wrong token or an exhausted
# quota shows up here instead of after minutes of waiting.
echo ">> Figma /v1/files/$DOC_ID/meta:"
META_H=$(mktemp) META_B=$(mktemp)
curl -sS -m 30 -D "$META_H" -o "$META_B" -H "X-Figma-Token: $FIGMA_TOKEN" \
  "https://api.figma.com/v1/files/$DOC_ID/meta" || { echo "!! no answer from api.figma.com"; exit 1; }
grep -iE '^HTTP|x-figma-plan-tier|x-figma-rate-limit|retry-after|x-figma-upgrade' "$META_H" | sed 's/^/   /'
META=$(python3 - "$META_B" <<'PY'
import json,sys
d=json.load(open(sys.argv[1])); f=d.get('file',d)
if d.get('status') and d.get('status')!=200 or d.get('err'): print("   !!", d.get('err') or d.get('message') or d, file=sys.stderr); sys.exit(1)
print("   name: %s | last change: %s | version: %s" % (f.get('name'), f.get('last_touched_at'), f.get('version')), file=sys.stderr)
print("%s %s" % (f.get('version') or '-', (f.get('last_touched_at') or '-')[:19]))
PY
) || { echo "!! Figma refused the key/token, see above"; rm -f "$META_H" "$META_B"; exit 1; }
read -r META_VERSION META_TOUCHED <<< "$META"
rm -f "$META_H" "$META_B"

# Two fetches into the same .dcf/.log corrupt both: refuse.
if pgrep -f "dcf-fetch --doc-id .* --output $OUT" >/dev/null 2>&1; then
  echo "!! another fetch is already writing $OUT; wait for it or kill it"; exit 1
fi

NODE_ARGS=(--nodes "$ROOT"); for n in "${EXTRA[@]}"; do NODE_ARGS+=(--nodes "$n"); done
echo ">> fetching ${NODE_ARGS[*]} from $DOC_ID -> $OUT"
# Library answers (team-library components, variables) do not change between fetches: kept on
# disk, only our own file goes to Figma. Delete the directory after a library update.
export FIGMA_HTTP_CACHE_DIR="$TREE/out/figma-lib-cache" FIGMA_HTTP_CACHE_EXCLUDE="$DOC_ID"
mkdir -p "$FIGMA_HTTP_CACHE_DIR"
FETCH_LOG="$OUT.fetch.log"
FIGMA_TOKEN="$FIGMA_TOKEN" "$HERE/../tools/dcf/cargo.sh" run -q --bin dcf-fetch -- \
  --doc-id "$DOC_ID" "${NODE_ARGS[@]}" --output "$OUT" > "$FETCH_LOG" 2>&1 &
FETCH_PID=$!
START=$(date +%s)
SHOWN=0
show_log_lines() {
  tail -n +"$1" "$FETCH_LOG" | tr -d '\0' | awk '
    /Rate limiter: waiting/ { rl++; next }
    /failed to fetch component key/ { fk++; next }
    { print "   | " $0 }
    END {
      if (rl) printf "   | (rate limiter waited %d x 100 ms)\n", rl
      if (fk) printf "   | (%d library component keys not found, skipped)\n", fk
    }'
}
# Progress every 5 s: new log lines, or what the fetcher is doing.
while kill -0 "$FETCH_PID" 2>/dev/null; do
  sleep 5
  ELAPSED=$(( $(date +%s) - START ))
  LOG_LINES=$(wc -l < "$FETCH_LOG")
  if [ "$LOG_LINES" -gt "$SHOWN" ]; then show_log_lines "$((SHOWN + 1))"; SHOWN=$LOG_LINES; fi
  TOOL_PID=$(pgrep -f 'dcf-fetch --doc-id' | head -1 || true)
  # The fetch ended between two samples: no progress line, the result follows below.
  kill -0 "$FETCH_PID" 2>/dev/null || break
  if [ -z "$TOOL_PID" ]; then
    printf '   %3ds  compiling dcf-fetch (cargo)\n' "$ELAPSED"
  else
    RX=$(ss -tinp 2>/dev/null | grep -A1 "pid=$TOOL_PID," | grep -oE 'bytes_received:[0-9]+' | head -1 | cut -d: -f2 || true)
    printf '   %3ds  talking to Figma, %s KB received\n' "$ELAPSED" "$(( ${RX:-0} / 1024 ))"
  fi
done
FETCH_RC=0; wait "$FETCH_PID" || FETCH_RC=$?
echo ">> fetch output:"; show_log_lines 1
[ "$FETCH_RC" = 0 ] || { echo "!! fetch failed (exit $FETCH_RC)"; exit 1; }

# Figma caches the big answer: right after an import the old content can come back.
GOT_VERSION=$(grep -aE '^ *Version: *[0-9]+' "$FETCH_LOG" | grep -oE '[0-9]+' | head -1 || true)
GOT_MODIFIED=$(grep -aE '^ *Last Modified: ' "$FETCH_LOG" | grep -oE '[0-9]{4}-[0-9-]+T[0-9:]{8}' | head -1 || true)
if [ "$GOT_VERSION" = "$META_VERSION" ]; then
  echo ">> version matches the file ($GOT_VERSION)"
elif [ -n "$GOT_MODIFIED" ] && [ "$META_TOUCHED" != - ] && [[ "$GOT_MODIFIED" < "$META_TOUCHED" ]]; then
  echo "!! STALE: fetched version $GOT_VERSION from $GOT_MODIFIED, the file changed at $META_TOUCHED (Figma cache). Wait a few minutes and fetch again."
else
  echo ">> fetched version $GOT_VERSION ($GOT_MODIFIED), file edited after /meta, ok"
fi
ls -la "$OUT"

# Layer names are length-prefixed strings in the .dcf: count them. 0 = the layer is hidden in
# Figma (hidden layers are not fetched) or has another name; the app then cannot reach it.
echo ">> layers the app needs:"
MISSING=0
for n in "${CHECK[@]}"; do
  c=$(grep -a -o -F "$n" "$OUT" | wc -l || true)   # no match: 0, not an exit (pipefail)
  printf '   %-32s %s%s\n' "$n" "$c" "$([ "$c" -eq 0 ] && echo '   !! MISSING')"
  if [ "$c" -eq 0 ]; then MISSING=$((MISSING + 1)); fi
done
[ "$MISSING" -eq 0 ] || echo "!! $MISSING layer(s) missing: show them in Figma or fix the name, then fetch again"
