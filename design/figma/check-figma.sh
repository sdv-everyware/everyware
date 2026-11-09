#!/bin/bash
# Reads a built .fig back and checks it, read only: the Library (no component set over another or
# outside the frame), the app's contract (names under #smarthome-root, no designer-only frame in
# it) and the prototype (every tap and its target). Usage:
#   check-figma.sh "Smart HOme ui v41.fig"                      # all three
#   CHECK_NAMES=#device/card,#home/tabs check-figma.sh <file>   # and fail on a missing name
# Needs the same tools as roundtrip.js: node, unzip and zip.
set -uo pipefail
IN=${1:?usage: check-figma.sh <file.fig>}
HERE=$(cd "$(dirname "$0")" && pwd)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
status=0
for c in library contract prototype; do
  node "$HERE/roundtrip.js" "$IN" "$TMP/out.fig" "$HERE/check/$c.js" 2>&1 | grep '^CHECK' | sed 's/^CHECK //'
  [ "${PIPESTATUS[0]}" -eq 0 ] || status=1
done
exit $status
