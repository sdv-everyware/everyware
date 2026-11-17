#!/bin/bash
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

# calendar-setup-url.sh: prints the link of the calendar's setup page, to open on this computer:
#
#   calendar-setup-url.sh            http://localhost:8765/setup?t=<token>, the port forwarded by adb
#   calendar-setup-url.sh --panel    the link as the panel publishes it (for a phone in its network)
#
# While no account is linked, the calendar bundle serves the setup page on the panel's port 8765
# and writes its URL to the file setup-url in its directory (bundles/calendar/README.md). The
# token in it is good for one visit: opening the link, also with curl, ends it, and the bundle
# writes the next one. So this script only reads the file; run it again for a new link.
#
# localhost works where the OAuth client's redirect URI is http://localhost:8765/callback or the
# relay page, which sends the browser back to the address the flow began on.
#
# Needs adb as root (a userdebug or eng build). Env: ANDROID_SERIAL picks the device.
set -euo pipefail

MODE=${1:-}
case "$MODE" in ""|--panel) ;; *) echo "usage: $0 [--panel]"; exit 1 ;; esac

DIR=/data/service_bundle_data/com.everyware.calendar/CalendarServiceBundle
PORT=8765

adb root >/dev/null 2>&1 || true
adb wait-for-device
[ "$(adb shell id -u | tr -d '\r')" = 0 ] || { echo "!! adb is not root" >&2; exit 1; }
adb shell test -d "$DIR" || { echo "!! $DIR does not exist: is com.everyware.calendar on the device and started once?" >&2; exit 1; }

url=$(adb shell "cat $DIR/setup-url 2>/dev/null" | tr -d '\r' || true)
if [ -z "$url" ]; then
  if adb shell test -f "$DIR/grant.json"; then
    echo "!! no setup page: an account is linked (Disconnect on the panel's calendar opens it again)" >&2
  elif ! adb shell test -f "$DIR/oauth-client.json"; then
    echo "!! no setup page: the panel has no OAuth client yet (scripts/calendar-client.sh <client.json>)" >&2
  else
    echo "!! no setup page yet: the bundle opens it within 10 s of its start (adb logcat -s calendar_bundle)" >&2
  fi
  exit 1
fi

if [ "$MODE" = --panel ]; then
  echo "$url"
else
  adb forward "tcp:$PORT" "tcp:$PORT" >/dev/null
  echo "$url" | sed -E "s#^http://[^/]+#http://localhost:$PORT#"
fi
