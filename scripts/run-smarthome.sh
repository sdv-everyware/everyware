#!/bin/bash
# run-smarthome.sh -- build, deploy and run smarthome_app on the RB3 or Cuttlefish.
#
# The steps:
#   1. adb root (DRM master needs root)
#   2. push the binary and data/ (smarthome.dcf + fonts) to /data/local/tmp
#   3. stop what holds the display: the panel's own service `smarthome` (com.everyware.smarthome,
#      docs/concepts/9-starting-the-app.md), or harry on an image without the panel APEX;
#      runtime only, the installed panel is back after a reboot or with `stop`
#   4. run with the APEX libs, log.tag INFO, and touch mapped to the display mode
#
# Usage:
#   ./vendor/sdv-everyware/everyware/scripts/run-smarthome.sh                   # deploy + run on the connected device
#   ./vendor/sdv-everyware/everyware/scripts/run-smarthome.sh --target cf       # the same, the target named
#   ./vendor/sdv-everyware/everyware/scripts/run-smarthome.sh --target cf all   # build + deploy + run
#   ./vendor/sdv-everyware/everyware/scripts/run-smarthome.sh --target cf stop  # restore installed panel
#
# Env: TARGET (rb3 or cf; default: what the connected device says it is, ro.product.device),
#      SERIAL (default: the one device adb sees; with a target named, 6aeccbe4 on rb3 and
#      127.0.0.1:<port> or 0.0.0.0:<port> on cf), CF_INSTANCE (default 7, port 6520+N-1),
#      TARGET_OUT_DIR (override target out dir; defaults to out_rb3 or cuttlefish_out),
#      TOUCH_SIZE=WxH (default: the first display mode),
#      SMARTHOME_LOG=debug|trace (the panel's own log level, main.rs)
set -e
TREE="$(cd "$(dirname "$0")/../../../.." && pwd)"
APP="$TREE/packages/services/display_safety/utils/smarthome-app"

TARGET="${TARGET:-}"
OUT_OVERRIDE="${TARGET_OUT_DIR:-}"
MODE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --target)
      [ $# -ge 2 ] || { echo "!! --target requires an argument (rb3 or cf)" >&2; exit 1; }
      TARGET="$2"
      shift 2
      ;;
    --target=*)
      TARGET="${1#--target=}"
      shift
      ;;
    --out-dir)
      [ $# -ge 2 ] || { echo "!! --out-dir requires a directory name" >&2; exit 1; }
      OUT_OVERRIDE="$2"
      shift 2
      ;;
    --out-dir=*)
      OUT_OVERRIDE="${1#--out-dir=}"
      shift
      ;;
    run|all|stop)
      MODE="$1"
      shift
      ;;
    -h|--help)
      sed -n '2,/^set -e/p' "$0" | sed '$d; s/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "usage: $0 [--target rb3|cf] [--out-dir <dir>] [run|all|stop]" >&2
      exit 1
      ;;
  esac
done
MODE="${MODE:-run}"

resolve_cf_serial() {
  local port=$((6520 + ${CF_INSTANCE:-7} - 1))
  local devs
  devs=$(adb devices 2>/dev/null || true)
  if echo "$devs" | awk -v s="127.0.0.1:$port" '$1 == s && $2 == "device" { found=1 } END { exit !found }'; then
    echo "127.0.0.1:$port"
  elif echo "$devs" | awk -v s="0.0.0.0:$port" '$1 == s && $2 == "device" { found=1 } END { exit !found }'; then
    echo "0.0.0.0:$port"
  else
    adb connect "127.0.0.1:$port" >/dev/null 2>&1 || true
    echo "127.0.0.1:$port"
  fi
}

# No target named: the device says which product it is (ro.product.device). The device is the
# one adb sees, or SERIAL (ANDROID_SERIAL) when several are connected.
detect_target() {
  local serial="${SERIAL:-${ANDROID_SERIAL:-}}" s id seen="" count=0
  if [ -z "$serial" ]; then
    for s in $(adb devices 2>/dev/null | awk 'NR > 1 && $2 == "device" { print $1 }'); do
      # one VM can be listed under two addresses (127.0.0.1 and 0.0.0.0): the same serial number
      id=$(adb -s "$s" shell getprop ro.serialno 2>/dev/null | tr -d '\r')
      id="${id:-$s}"
      case " $seen " in *" $id "*) continue ;; esac
      seen="$seen $id"
      count=$((count + 1))
      [ -n "$serial" ] || serial="$s"
    done
    [ "$count" -ge 1 ] || { echo "!! no device: connect one, or name the target (--target rb3|cf)" >&2; exit 1; }
    [ "$count" -eq 1 ] || { echo "!! $count devices: set SERIAL, or name the target (--target rb3|cf)" >&2; exit 1; }
  fi
  local product
  product=$(adb -s "$serial" shell getprop ro.product.device 2>/dev/null | tr -d '\r')
  case "$product" in
    sdv_media_db845c) TARGET=rb3 ;;
    sdv_media_everyware_cf) TARGET=cf ;;
    *) echo "!! $serial is the product '$product', neither the RB3 nor Cuttlefish: name the target (--target rb3|cf)" >&2; exit 1 ;;
  esac
  SERIAL="$serial"
  echo ">> target $TARGET: $serial is $product"
}
[ -n "$TARGET" ] || detect_target

case "$TARGET" in
  rb3)
    ENV_SH="device/rb3/sdv_media_db845c/tools/env.sh"
    OUT_NAME="${OUT_OVERRIDE:-out_rb3}"
    PRODUCT="sdv_media_db845c"
    ARCH="arm64"
    DEFAULT_SERIAL="6aeccbe4"
    MODE_PATH="/sys/class/drm/card0-HDMI-A-1/modes"
    # minigbm allocates UBWC scanout buffers that mesa renders uncompressed (noise on screen)
    EXTRA_PROPS="setprop vendor.minigbm.debug nocompression;"
    ;;
  cf)
    ENV_SH="device/cf-everyware/sdv_media_everyware_cf/tools/env.sh"
    OUT_NAME="${OUT_OVERRIDE:-cuttlefish_out}"
    PRODUCT="sdv_media_everyware_cf"
    ARCH="x86_64"
    DEFAULT_SERIAL=""
    MODE_PATH="/sys/class/drm/card0-Virtual-1/modes"
    EXTRA_PROPS=""
    ;;
  *)
    echo "!! unknown TARGET '$TARGET' (expected rb3 or cf)" >&2
    exit 1
    ;;
esac

if [ -z "${SERIAL:-}" ]; then
  if [ "$TARGET" = cf ]; then
    SERIAL="$(resolve_cf_serial)"
  else
    SERIAL="$DEFAULT_SERIAL"
  fi
fi

REMOTE_DATA=/data/local/tmp/smarthome-app-data
# the libraries of the panel APEX; an image without it still has HAR's
LDP=/apex/com.everyware.smarthome/lib64:/apex/com.google.display_safety.har/lib64:/system/lib64:/vendor/lib64
A() { adb -s "$SERIAL" "$@"; }

wait_device() {
  for _ in $(seq 60); do
    if adb devices 2>/dev/null | awk -v s="$SERIAL" '$1 == s && $2 == "device" { found=1 } END { exit !found }'; then
      return 0
    fi
    if [ "$TARGET" = cf ]; then
      adb connect "$SERIAL" >/dev/null 2>&1 || true
    fi
    sleep 1
  done
  echo "!! device $SERIAL did not come online after 60 s" >&2
  exit 1
}

if [ "$MODE" = "all" ]; then
  echo ">> build smarthome_app ($ENV_SH: lunch $PRODUCT, OUT_DIR=$OUT_NAME)"
  # the APEX variant, the one pushed below: `m smarthome_app` builds only the platform
  # variant, and the APEX one stays as it was (a dev run of an old binary)
  VARIANT=$(ls -td "$TREE/$OUT_NAME"/soong/.intermediates/packages/services/display_safety/utils/smarthome-app/smarthome_app/android_vendor_${ARCH}_*_apex* 2>/dev/null | head -1)
  TARGET_MOD=${VARIANT:+${VARIANT#$TREE/}/smarthome_app}
  ( cd "$TREE" && export OUT_DIR="$OUT_NAME" && source "$ENV_SH" >/dev/null \
      && m ${TARGET_MOD:-com.everyware.smarthome} ) || { echo "!! build failed" >&2; exit 1; }
fi

echo ">> adb root ($SERIAL, target=$TARGET)"
wait_device
A root >/dev/null 2>&1 || true
# adbd restarts after "adb root"; poll until it answers as uid 0.
for _ in $(seq 20); do
  wait_device
  [ "$(A shell id -u 2>/dev/null | tr -d '\r')" = "0" ] && break
  sleep 0.5
done
[ "$(A shell id -u 2>/dev/null | tr -d '\r')" = "0" ] || { echo "!! adb is not root" >&2; exit 1; }

if [ "$MODE" = "stop" ]; then
  echo ">> kill the dev build, start the installed panel (or harry on an image without it)"
  A shell "killall smarthome_app 2>/dev/null; if [ -n \"\$(getprop init.svc.smarthome)\" ]; then start smarthome; else start harry; fi"
  exit 0
fi

# The APEX variant: it links the APEX's own gRPC (libgrpc++-1.72.2.so). The platform variant
# (android_<arch>_*) links /system's libgrpc++.so; next to the APEX libraries both load and
# gRPC aborts at start ("Metric name ... has already been registered").
BIN=$(ls -t "$TREE/$OUT_NAME"/soong/.intermediates/packages/services/display_safety/utils/smarthome-app/smarthome_app/android_vendor_${ARCH}_*_apex*/smarthome_app 2>/dev/null | head -1)
[ -n "$BIN" ] || { echo "!! smarthome_app not built: $0 --target $TARGET all" >&2; exit 1; }
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
[ -z "$HOLDER" ] || { echo "!! card0 still held by: $HOLDER" >&2; exit 1; }

echo ">> push binary + data"
A push "$BIN" /data/local/tmp/smarthome_app | tail -1
A shell "chmod 755 /data/local/tmp/smarthome_app; rm -rf $REMOTE_DATA"
A push "$APP/data" "$REMOTE_DATA" | tail -1

# First line of modes = the mode harry takes; can be empty right after a reboot.
if [ -z "${TOUCH_SIZE:-}" ]; then
  for _ in $(seq 10); do
    TOUCH_SIZE="$(A shell "head -1 $MODE_PATH 2>/dev/null" | tr -d '\r')"
    [ -n "$TOUCH_SIZE" ] && break
    sleep 1
  done
fi
[ -n "$TOUCH_SIZE" ] || { echo "!! display has no modes at $MODE_PATH; set TOUCH_SIZE=WxH" >&2; exit 1; }
echo ">> touch mapped to $TOUCH_SIZE"

# On an image with the panel APEX the dev build stands in for the service smarthome, so it
# runs as the service does (the APEX's init rc): the Wayland zones in the runtime directory,
# where the browser (everyware-browser.sh) and camera (camera_app) find the socket and FIFOs.
# On Cuttlefish, smarthome_app checks ro.vendor.everyware.zone_plane=false and keeps zone planes off.
SERVICE=""
if [ -n "$(A shell getprop init.svc.smarthome | tr -d '\r')" ]; then
  RUN=/data/vendor/everyware/run
  SERVICE="XDG_RUNTIME_DIR=$RUN HAR_BROWSER_FIFO=$RUN/har-browser-open HAR_BROWSER_EVENTS=$RUN/har-browser-events"
  ZONES="--wayland-zones --zone-plane"
fi

# log.tag INFO: the image has none, harry would log VERBOSE.
echo ">> run smarthome_app (logs: adb -s $SERIAL logcat -s smarthome-app; stop: $0 --target $TARGET stop)"
A shell "setprop log.tag INFO; $EXTRA_PROPS \
  $SERVICE SMARTHOME_LOG=${SMARTHOME_LOG:-info} LD_LIBRARY_PATH=$LDP \
  /data/local/tmp/smarthome_app -d $REMOTE_DATA -i smarthome --touch-size $TOUCH_SIZE ${ZONES:-}"
