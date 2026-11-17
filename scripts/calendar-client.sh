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

# calendar-client.sh: puts the owner's OAuth client (Google's downloaded client JSON, a client
# of the type "Web application") on the panel, where the calendar bundle reads it:
#
#   calendar-client.sh <client.json>
#
# The file goes to the bundle's directory as oauth-client.json, owned by the bundle's user, mode
# 0600, labelled as the directory. It is never part of git or of the image
# (bundles/calendar/README.md). A bundle without a client finds the file within 10 s; a bundle
# that has one already is restarted (the lifecycle manager starts it again), so it takes the new
# one. A new client makes a linked account's grant worthless: the screen shows the setup code.
#
# Needs adb as root (a userdebug or eng build). Env: ANDROID_SERIAL picks the device.
set -euo pipefail

JSON=${1:-}
[ -n "$JSON" ] && [ -f "$JSON" ] || { echo "usage: $0 <client.json>   (Google Cloud console: an OAuth client of the type Web application)"; exit 1; }
python3 - "$JSON" <<'PY' || exit 1
import json, sys
web = json.load(open(sys.argv[1])).get("web") or {}
missing = [k for k in ("client_id", "client_secret", "redirect_uris") if not web.get(k)]
if missing:
    sys.exit(f"!! {sys.argv[1]}: no 'web' client or no {', '.join(missing)}")
print(f">> client {web['client_id']}, redirect URI {web['redirect_uris'][0]}")
PY

DIR=/data/service_bundle_data/com.everyware.calendar/CalendarServiceBundle
FILE=$DIR/oauth-client.json
PROCESS=CalendarServiceBundle:instance-1

echo ">> adb root (${ANDROID_SERIAL:-the only device})"
adb root >/dev/null 2>&1 || true
adb wait-for-device
[ "$(adb shell id -u | tr -d '\r')" = 0 ] || { echo "!! adb is not root"; exit 1; }
adb shell test -d "$DIR" || { echo "!! $DIR does not exist: is com.everyware.calendar on the device and started once?"; exit 1; }

had=$(adb shell "test -f $FILE && echo yes || echo no" | tr -d '\r')
echo ">> push to $FILE"
adb push "$JSON" "$FILE.new" >/dev/null
adb shell "chown \$(stat -c %u:%g $DIR) $FILE.new && chmod 0600 $FILE.new && mv $FILE.new $FILE && restorecon $FILE"
adb shell ls -lZ "$FILE"

if [ "$had" = yes ]; then
  pid=$(adb shell pidof "$PROCESS" | tr -d '\r' || true)
  if [ -n "$pid" ]; then
    echo ">> restart the bundle (kill $pid; the lifecycle manager starts it again)"
    adb shell kill "$pid"
  else
    echo ">> the bundle is not running; it reads the client when it starts"
  fi
else
  echo ">> the bundle takes the client within 10 s"
fi
echo ">> log: adb logcat -s calendar_bundle"
# The client secret is the only guard against a sign-in flow someone else starts with the public
# client id (bundles/calendar/README.md, Security): it belongs on the panel only.
echo ">> now delete $JSON from this computer: the client secret belongs on the panel only"
