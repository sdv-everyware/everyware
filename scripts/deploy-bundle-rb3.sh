#!/bin/bash
# deploy-bundle-rb3.sh -- put a service bundle's APEX onto the RB3 and reboot it.
#
# A bundle (bundles/<name>) is the APEX com.everyware.<name>, built into the product image
# (product/everyware.mk), the panel the APEX com.everyware.smarthome (a /vendor APEX). Between
# flashes a rebuilt APEX goes to /product/apex (or /vendor/apex) of the board;
# apexd activates it at the next boot, so the board reboots. The APEX that was there is kept on
# the board as /data/local/tmp/apex-backup/com.everyware.<name>.apex.<timestamp>.
#
# Usage:
#   ./vendor/sdv-everyware/everyware/scripts/deploy-bundle-rb3.sh weather dashboard   # deploy, reboot, show the logs
#   ./vendor/sdv-everyware/everyware/scripts/deploy-bundle-rb3.sh all weather          # m com.everyware.weather first
#   ./vendor/sdv-everyware/everyware/scripts/deploy-bundle-rb3.sh noreboot weather     # push only (reboot yourself)
#
# After the reboot the screen is harry's; start the app again with run-smarthome-rb3.sh.
# Env: SERIAL (default 6aeccbe4)
set -e
TREE="$(cd "$(dirname "$0")/../../../.." && pwd)"
PRODUCT_OUT="$TREE/out_rb3/target/product/sdv_media_db845c"
# A bundle is a /product APEX; the panel (com.everyware.smarthome) is a /vendor one. The
# partition of each APEX is where the build put it.
part_of() { [ -f "$PRODUCT_OUT/vendor/apex/com.everyware.$1.apex" ] && echo vendor || echo product; }
SERIAL="${SERIAL:-6aeccbe4}"
A() { adb -s "$SERIAL" "$@"; }

BUILD=no REBOOT=yes NAMES=()
for arg in "$@"; do
  case "$arg" in
    all) BUILD=yes ;;
    noreboot) REBOOT=no ;;
    *) NAMES+=("$arg") ;;
  esac
done
[ ${#NAMES[@]} -gt 0 ] || { echo "usage: $0 [all] [noreboot] <bundle name>..."; exit 1; }

if [ "$BUILD" = yes ]; then
  MODULES=$(printf 'com.everyware.%s ' "${NAMES[@]}")
  echo ">> build $MODULES (tools/env.sh: lunch sdv_media_db845c, OUT_DIR=out_rb3)"
  ( cd "$TREE" && source device/rb3/sdv_media_db845c/tools/env.sh >/dev/null \
      && m $MODULES ) || { echo "!! build failed"; exit 1; }
fi

for name in "${NAMES[@]}"; do
  f="$PRODUCT_OUT/$(part_of "$name")/apex/com.everyware.$name.apex"
  [ -f "$f" ] || { echo "!! $f: not built (m com.everyware.$name)"; exit 1; }
done

echo ">> adb root ($SERIAL)"
A root >/dev/null 2>&1 || true
for _ in $(seq 20); do
  A wait-for-device
  [ "$(A shell id -u 2>/dev/null | tr -d '\r')" = "0" ] && break
  sleep 0.5
done
[ "$(A shell id -u | tr -d '\r')" = "0" ] || { echo "!! adb is not root"; exit 1; }

echo ">> adb remount (/product and /vendor writable until the next reboot)"
A remount >/dev/null 2>&1 || { A shell mount -o rw,remount /product; A shell mount -o rw,remount /vendor; }

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
    echo ">> keep the board's $APEX as /data/local/tmp/apex-backup/$APEX.$STAMP"
    A shell cp "/$PART/apex/$APEX" "/data/local/tmp/apex-backup/$APEX.$STAMP"
  else
    echo ">> $APEX is new on the board"
  fi
  echo ">> push $APEX to /$PART/apex ($(stat -c %s "$PRODUCT_OUT/$PART/apex/$APEX") bytes)"
  A push "$PRODUCT_OUT/$PART/apex/$APEX" "/$PART/apex/$APEX" | tail -1
done
A shell sync

if [ "$REBOOT" = no ]; then
  echo ">> pushed; reboot the board to activate: adb -s $SERIAL reboot"
  exit 0
fi

echo ">> reboot (apexd activates the APEXes at boot)"
A reboot
sleep 5
A wait-for-device
echo -n ">> waiting for boot"
for _ in $(seq 120); do
  [ "$(A shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = "1" ] && break
  echo -n .
  sleep 1
done
echo
[ "$(A shell getprop sys.boot_completed | tr -d '\r')" = "1" ] || { echo "!! no boot complete after 120 s"; exit 1; }

echo ">> active APEXes:"
for name in "${NAMES[@]}"; do
  A shell "ls -d /apex/com.everyware.$name 2>/dev/null || echo '!! com.everyware.$name not active'"
done
TAGS=$(printf '%s_bundle ' "${NAMES[@]}")
echo ">> the bundles' log so far (adb -s $SERIAL logcat -s $TAGS):"
sleep 10
A logcat -d -s $TAGS 2>/dev/null | tail -20
echo ">> the screen is harry's again; start the app: ./vendor/sdv-everyware/everyware/scripts/run-smarthome-rb3.sh"
