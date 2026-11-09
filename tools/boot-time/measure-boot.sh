#!/bin/bash
# measure-boot.sh [runs] -- boot time of the device to the first frame of its app, as Google
# measures SDV and HAR (README.md next to this script): N reboots, every time from the kernel's
# start, on the kernel's clock.
#
#   ./vendor/sdv-everyware/everyware/tools/boot-time/measure-boot.sh 3
#
# Env: SERIAL (default 6aeccbe4), SERVICE (the app's init service, default smarthome),
#      TAG (its log tag, default smarthome-app).
# Prints one line per run (seconds since the kernel started) and the averages, as CSV
# (";" separated) for the write-up.
set -u
SERIAL="${SERIAL:-6aeccbe4}"
SERVICE="${SERVICE:-smarthome}"
TAG="${TAG:-smarthome-app}"
RUNS="${1:-3}"
A() { adb -s "$SERIAL" "$@"; }
ns_to_s() { awk -v v="$1" 'BEGIN { if (v == "") print ""; else printf "%.2f", v / 1e9 }'; }

root() {
  A root >/dev/null 2>&1
  for _ in $(seq 30); do
    A wait-for-device
    [ "$(A shell id -u 2>/dev/null | tr -d '\r')" = 0 ] && return
    sleep 0.5
  done
  echo "!! adb is not root" >&2; exit 1
}

echo "run;init;$SERVICE starts;boot completed;first frame;first frame from init"
sum_init=0; sum_svc=0; sum_done=0; sum_frame=0; n=0
for run in $(seq "$RUNS"); do
  root
  A reboot
  sleep 5
  A wait-for-device
  booted=no
  for _ in $(seq 180); do
    [ "$(A shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = 1 ] && { booted=yes; break; }
    sleep 1
  done
  if [ "$booted" = no ]; then echo "$run;?;?;?;?;? (no boot completed within 180 s)"; continue; fi
  # the app logs its first frame with the time since boot (CLOCK_BOOTTIME)
  frame=""
  for _ in $(seq 60); do
    frame=$(A logcat -d -s "$TAG" 2>/dev/null | sed -nE "s/.*first frame of '[^']*' at ([0-9.]+) s since boot.*/\1/p" | head -1)
    [ -n "$frame" ] && break
    sleep 1
  done
  root
  init=$(ns_to_s "$(A shell getprop ro.boottime.init | tr -d '\r')")
  svc=$(ns_to_s "$(A shell getprop ro.boottime.$SERVICE | tr -d '\r')")
  # Google's boot-complete marker, written by SDV's init to the kernel log; logcat's kernel
  # buffer keeps it (the kernel's own buffer may have dropped it). Its wall-clock stamp is the
  # time since boot as long as the clock is still at 1970 (no RTC; chrony sets the clock later).
  done_=$(A logcat -d -b kernel -v epoch 2>/dev/null | awk '/VIRTUAL_DEVICE_BOOT_COMPLETED/ { if ($1 < 1000000) printf "%.2f", $1; exit }')
  [ -n "$frame" ] && from_init=$(awk -v f="$frame" -v i="$init" 'BEGIN { printf "%.2f", f - i }') || from_init=""
  printf "%s;%s;%s;%s;%s;%s\n" "$run" "$init" "$svc" "${done_:-?}" "${frame:-?}" "${from_init:-?}"
  if [ -n "$frame" ] && [ -n "$done_" ]; then
    n=$((n + 1))
    sum_init=$(awk -v a="$sum_init" -v b="$init" 'BEGIN { print a + b }')
    sum_svc=$(awk -v a="$sum_svc" -v b="$svc" 'BEGIN { print a + b }')
    sum_done=$(awk -v a="$sum_done" -v b="$done_" 'BEGIN { print a + b }')
    sum_frame=$(awk -v a="$sum_frame" -v b="$frame" 'BEGIN { print a + b }')
  fi
done
if [ "$n" -gt 0 ]; then
  awk -v n="$n" -v i="$sum_init" -v s="$sum_svc" -v d="$sum_done" -v f="$sum_frame" \
    'BEGIN { printf "average;%.2f;%.2f;%.2f;%.2f;%.2f\n", i/n, s/n, d/n, f/n, (f-i)/n }'
fi
