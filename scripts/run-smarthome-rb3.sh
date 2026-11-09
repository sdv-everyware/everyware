#!/bin/bash
# run-smarthome-rb3.sh -- build, deploy and run smarthome_app on the RB3 (HDMI 1920x1080).
#
# The steps:
#   1. adb root (DRM master needs root)
#   2. push the binary and data/ (smarthome.dcf + fonts) to /data/local/tmp
#   3. stop what holds the display: the panel's own service `smarthome` (com.everyware.smarthome,
#      docs/concepts/9-starting-the-app.md), or harry on an image without the panel APEX;
#      runtime only, the installed panel is back after a reboot or with `stop`
#   4. run with the APEX libs, log.tag INFO, LINEAR scanout buffers and touch mapped to the
#      HDMI mode
#
# Usage:
#   ./vendor/sdv-everyware/everyware/scripts/run-smarthome-rb3.sh        # deploy + run (foreground, Ctrl-C stops)
#   ./vendor/sdv-everyware/everyware/scripts/run-smarthome-rb3.sh all    # m smarthome_app first
#   ./vendor/sdv-everyware/everyware/scripts/run-smarthome-rb3.sh stop   # kill the dev build, start the installed panel again
#
# Env: SERIAL (default 6aeccbe4), TOUCH_SIZE=WxH (default: the first HDMI mode)
set -e
TREE="$(cd "$(dirname "$0")/../../../.." && pwd)"
APP="$TREE/packages/services/display_safety/utils/smarthome-app"
SERIAL="${SERIAL:-6aeccbe4}"
MODE="${1:-run}"
REMOTE_DATA=/data/local/tmp/smarthome-app-data
# the libraries of the panel APEX; an image without it still has HAR's
LDP=/apex/com.everyware.smarthome/lib64:/apex/com.google.sdv.display_safety.har/lib64:/system/lib64:/vendor/lib64
A() { adb -s "$SERIAL" "$@"; }

if [ "$MODE" = "all" ]; then
  echo ">> build smarthome_app (tools/env.sh: lunch sdv_media_db845c, OUT_DIR=out_rb3)"
  ( cd "$TREE" && source device/rb3/sdv_media_db845c/tools/env.sh >/dev/null \
      && m smarthome_app ) || { echo "!! build failed"; exit 1; }
fi

echo ">> adb root ($SERIAL)"
A root >/dev/null 2>&1 || true
# adbd restarts after "adb root"; poll until it answers as uid 0.
for _ in $(seq 20); do
  A wait-for-device
  [ "$(A shell id -u 2>/dev/null | tr -d '\r')" = "0" ] && break
  sleep 0.5
done
[ "$(A shell id -u | tr -d '\r')" = "0" ] || { echo "!! adb is not root"; exit 1; }

if [ "$MODE" = "stop" ]; then
  echo ">> kill the dev build, start the installed panel (or harry on an image without it)"
  A shell "killall smarthome_app 2>/dev/null; if [ -n \"\$(getprop init.svc.smarthome)\" ]; then start smarthome; else start harry; fi"
  exit 0
fi

# The APEX variant: it links the APEX's own gRPC (libgrpc++-1.72.2.so). The platform variant
# (android_arm64_*) links /system's libgrpc++.so; next to the APEX libraries both load and
# gRPC aborts at start ("Metric name ... has already been registered").
BIN=$(ls -t "$TREE"/out_rb3/soong/.intermediates/packages/services/display_safety/utils/smarthome-app/smarthome_app/android_vendor_arm64_*_apex*/smarthome_app 2>/dev/null | head -1)
[ -n "$BIN" ] || { echo "!! smarthome_app not built: $0 all"; exit 1; }
echo ">> binary: $BIN ($(date -r "$BIN" +%m-%d_%H:%M))"

echo ">> stop what holds the display (runtime only): the panel service, smarthome_app, harry"
# The installed panel is a service init restarts when it ends: stop the service, do not kill
# it. On an image without the panel, init.sdv_db845c.rc restarted harry once after it stopped
# (vendor.sdv.harry.retry): mark the retry as used first.
A shell "stop smarthome 2>/dev/null; setprop vendor.sdv.harry.retry 1; killall smarthome_app 2>/dev/null; stop harry 2>/dev/null; stop vendor.hwcomposer-3 2>/dev/null; true"
# One DRM master: wait until nobody has card0 open.
for _ in $(seq 20); do
  HOLDER=$(A shell 'for p in /proc/[0-9]*; do ls -l $p/fd 2>/dev/null | grep -q dri/card0 && echo "${p#/proc/} $(cat $p/comm)"; done' | tr -d '\r')
  [ -z "$HOLDER" ] && break
  sleep 0.5
done
[ -z "$HOLDER" ] || { echo "!! card0 still held by: $HOLDER"; exit 1; }

echo ">> push binary + data"
A push "$BIN" /data/local/tmp/smarthome_app | tail -1
A shell "chmod 755 /data/local/tmp/smarthome_app; rm -rf $REMOTE_DATA"
A push "$APP/data" "$REMOTE_DATA" | tail -1

# First line of modes = the mode harry takes; can be empty right after a reboot.
if [ -z "${TOUCH_SIZE:-}" ]; then
  for _ in $(seq 10); do
    TOUCH_SIZE="$(A shell "head -1 /sys/class/drm/card0-HDMI-A-1/modes" | tr -d '\r')"
    [ -n "$TOUCH_SIZE" ] && break
    sleep 1
  done
fi
[ -n "$TOUCH_SIZE" ] || { echo "!! HDMI has no modes (cable/monitor?); set TOUCH_SIZE=WxH"; exit 1; }
echo ">> touch mapped to $TOUCH_SIZE"

# log.tag INFO: the image has none, harry would log VERBOSE. minigbm nocompression: harry's
# minigbm allocates UBWC scanout buffers that mesa renders uncompressed (noise on screen).
echo ">> run smarthome_app (logs: adb -s $SERIAL logcat -s smarthome-app; stop: $0 stop)"
A shell "setprop log.tag INFO; setprop vendor.minigbm.debug nocompression; \
  LD_LIBRARY_PATH=$LDP /data/local/tmp/smarthome_app -d $REMOTE_DATA -i smarthome --touch-size $TOUCH_SIZE"
