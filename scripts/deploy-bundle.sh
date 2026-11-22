#!/bin/bash
# deploy-bundle.sh -- put a service bundle's APEX onto the RB3 or Cuttlefish and reboot it.
#
# A bundle (bundles/<name>) is the APEX com.everyware.<name>, built into the product image
# (product/everyware.mk), the panel the APEX com.everyware.smarthome (a /vendor APEX). Between
# flashes or full VM rebuilds, a rebuilt APEX goes to /product/apex (or /vendor/apex) of the
# target; apexd activates it at the next boot, so the target reboots. The APEX that was there is
# kept on the target as /data/local/tmp/apex-backup/com.everyware.<name>.apex.<timestamp>.
#
# Usage:
#   ./vendor/sdv-everyware/everyware/scripts/deploy-bundle.sh weather dashboard
#   ./vendor/sdv-everyware/everyware/scripts/deploy-bundle.sh --target cf all weather
#   TARGET=cf ./vendor/sdv-everyware/everyware/scripts/deploy-bundle.sh noreboot weather
#
# Env: TARGET (rb3 or cf; default rb3), SERIAL (default 6aeccbe4 on rb3; auto-detected
#      127.0.0.1:<port> or 0.0.0.0:<port> on cf), CF_INSTANCE (default 7, port 6520+N-1),
#      TARGET_OUT_DIR (override target out dir; defaults to out_rb3 or cuttlefish_out).
set -e
TREE="$(cd "$(dirname "$0")/../../../.." && pwd)"

TARGET="${TARGET:-rb3}"
OUT_OVERRIDE="${TARGET_OUT_DIR:-}"
BUILD=no
REBOOT=yes
NAMES=()
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
    all) BUILD=yes; shift ;;
    noreboot) REBOOT=no; shift ;;
    -h|--help)
      sed -n '2,/^set -e/p' "$0" | sed '$d; s/^# \{0,1\}//'
      exit 0
      ;;
    -*)
      echo "!! unknown option '$1'" >&2
      echo "usage: $0 [--target rb3|cf] [--out-dir <dir>] [all] [noreboot] <bundle name>..." >&2
      exit 1
      ;;
    *) NAMES+=("$1"); shift ;;
  esac
done
[ ${#NAMES[@]} -gt 0 ] || { echo "usage: $0 [--target rb3|cf] [--out-dir <dir>] [all] [noreboot] <bundle name>..." >&2; exit 1; }

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

case "$TARGET" in
  rb3)
    ENV_SH="device/rb3/sdv_media_db845c/tools/env.sh"
    OUT_NAME="${OUT_OVERRIDE:-out_rb3}"
    PRODUCT="sdv_media_db845c"
    DEFAULT_SERIAL="6aeccbe4"
    ;;
  cf)
    ENV_SH="device/cf-everyware/sdv_media_everyware_cf/tools/env.sh"
    OUT_NAME="${OUT_OVERRIDE:-cuttlefish_out}"
    PRODUCT="sdv_media_everyware_cf"
    DEFAULT_SERIAL=""
    ;;
  *)
    echo "!! unknown TARGET '$TARGET' (expected rb3 or cf)" >&2
    exit 1
    ;;
esac

PRODUCT_OUT="$TREE/$OUT_NAME/target/product/$PRODUCT"
# A bundle is a /product APEX; the panel (com.everyware.smarthome) is a /vendor one. The
# partition of each APEX is where the build put it.
part_of() { [ -f "$PRODUCT_OUT/vendor/apex/com.everyware.$1.apex" ] && echo vendor || echo product; }

if [ -z "${SERIAL:-}" ]; then
  if [ "$TARGET" = cf ]; then
    SERIAL="$(resolve_cf_serial)"
  else
    SERIAL="$DEFAULT_SERIAL"
  fi
fi
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

wait_root() {
  wait_device
  A root >/dev/null 2>&1 || true
  for _ in $(seq 20); do
    wait_device
    [ "$(A shell id -u 2>/dev/null | tr -d '\r')" = "0" ] && break
    sleep 0.5
  done
  [ "$(A shell id -u 2>/dev/null | tr -d '\r')" = "0" ] || { echo "!! adb is not root" >&2; exit 1; }
}

wait_boot() {
  sleep 5
  wait_device
  echo -n ">> waiting for boot"
  for _ in $(seq 120); do
    [ "$(A shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = "1" ] && break
    echo -n .
    sleep 1
  done
  echo
  [ "$(A shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = "1" ] || { echo "!! no boot complete after 120 s" >&2; exit 1; }
}

if [ "$BUILD" = yes ]; then
  MODULES=$(printf 'com.everyware.%s ' "${NAMES[@]}")
  echo ">> build $MODULES ($ENV_SH: lunch $PRODUCT, OUT_DIR=$OUT_NAME)"
  ( cd "$TREE" && export OUT_DIR="$OUT_NAME" && source "$ENV_SH" >/dev/null \
      && m $MODULES ) || { echo "!! build failed" >&2; exit 1; }
fi

for name in "${NAMES[@]}"; do
  f="$PRODUCT_OUT/$(part_of "$name")/apex/com.everyware.$name.apex"
  [ -f "$f" ] || { echo "!! $f: not built (m com.everyware.$name)" >&2; exit 1; }
done

echo ">> adb root ($SERIAL, target=$TARGET)"
wait_root

echo ">> adb remount (/product and /vendor writable until the next reboot)"
REMOUNT_RC=0
REMOUNT_OUT=$(A remount 2>&1) || REMOUNT_RC=$?
if echo "$REMOUNT_OUT" | grep -qi "reboot"; then
  echo ">> first remount enabled overlayfs; rebooting once to activate it"
  A reboot
  wait_boot
  wait_root
  REMOUNT_RC=0
  REMOUNT_OUT=$(A remount 2>&1) || REMOUNT_RC=$?
fi
if [ "$REMOUNT_RC" -ne 0 ] || ! echo "$REMOUNT_OUT" | grep -qi "succeeded"; then
  echo ">> adb remount output: $REMOUNT_OUT" >&2
  echo ">> trying mount -o rw,remount /product and /vendor" >&2
  A shell "mount -o rw,remount /product && mount -o rw,remount /vendor" || {
    echo "!! failed to remount /product and /vendor writable" >&2
    exit 1
  }
fi

STAMP=$(date +%Y%m%d-%H%M)
A shell mkdir -p /data/local/tmp/apex-backup
# The project was com.smarthome until 2026-11-05: an old APEX of the same bundle would run next
# to the new one (two weather publishers). Keep it in the backup and take it out of /product.
for name in "${NAMES[@]}"; do
  OLD="com.smarthome.$name.apex"
  if A shell test -f "/product/apex/$OLD"; then
    echo ">> old $OLD: moved to /data/local/tmp/apex-backup/$OLD.$STAMP"
    A shell mv "/product/apex/$OLD" "/data/local/tmp/apex-backup/$OLD.$STAMP"
  fi
done
for name in "${NAMES[@]}"; do
  APEX="com.everyware.$name.apex"
  PART=$(part_of "$name")
  if A shell test -f "/$PART/apex/$APEX"; then
    echo ">> keep the target's $APEX as /data/local/tmp/apex-backup/$APEX.$STAMP"
    A shell cp "/$PART/apex/$APEX" "/data/local/tmp/apex-backup/$APEX.$STAMP"
  else
    echo ">> $APEX is new on the target"
  fi
  echo ">> push $APEX to /$PART/apex ($(stat -c %s "$PRODUCT_OUT/$PART/apex/$APEX") bytes)"
  A push "$PRODUCT_OUT/$PART/apex/$APEX" "/$PART/apex/$APEX" | tail -1
done
A shell sync

if [ "$REBOOT" = no ]; then
  echo ">> pushed; reboot the target to activate: adb -s $SERIAL reboot"
  exit 0
fi

echo ">> reboot (apexd activates the APEXes at boot)"
A reboot
wait_boot

echo ">> active APEXes:"
for name in "${NAMES[@]}"; do
  A shell "ls -d /apex/com.everyware.$name 2>/dev/null || echo '!! com.everyware.$name not active'"
done
TAGS=$(printf '%s_bundle ' "${NAMES[@]}")
echo ">> the bundles' log so far (adb -s $SERIAL logcat -s $TAGS):"
sleep 10
A logcat -d -s $TAGS 2>/dev/null | tail -20
echo ">> done; for a dev build of the panel run: ./vendor/sdv-everyware/everyware/scripts/run-smarthome.sh --target $TARGET"
