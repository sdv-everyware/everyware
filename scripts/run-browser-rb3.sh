#!/bin/bash
# run-browser-rb3.sh -- run the panel with a browser zone on the RB3 and measure it.
# What runs, the Debian root it needs and the modes: docs/project/browser.md.
#
#   run-browser-rb3.sh start [--no-zone-plane] [--zone-redraw timer|frames] [--zone-fps N] [URL]
#       adb root, stop the panel, kill the old master/browser (by pidof), push the built
#       smarthome_app as smarthome_zone with the app's data/, start it with --wayland-zones and
#       --zone-plane (the browser on a hardware plane of its own: a page's frame costs no redraw
#       of the dashboard; --no-zone-plane composites it on the GPU instead, and only then
#       --zone-redraw chooses when the dashboard is redrawn for the zone), wait for
#       the socket, start Cog in the Debian root as user `browser`. Cog shows in the dashboard's
#       right panel (#panel/browser): a tap on a news story opens its page there. The page goes
#       from the dashboard through the FIFO /data/local/tmp/har-browser-open (0660 root:2900)
#       to har-browser-relay.sh (uid browser), which runs cogctl over a D-Bus session bus that
#       exists only inside the root. Back, har-browser-cog.sh runs Cog and writes one word per
#       page event (committed, loaded, failed) to /data/local/tmp/har-browser-events (0660
#       root:2900): the dashboard's loader goes when the page shows. URL (default
#       about:blank) is Cog's first page.
#       Env: WEBKIT_DEBUG=Events,Scrolling   passed to Cog (its log: /home/browser/cog.log in
#                                            the root, /data/glibc/rootfs/home/browser/cog.log)
#   run-browser-rb3.sh stop                  kill master + browser, start the installed panel
#   run-browser-rb3.sh measure [SECONDS]     CPU and memory per process (default 10 s)
#   run-browser-rb3.sh drag N [X Y]          N synthetic upward drags (120 px, 20 steps, 16 ms
#                                            apart) starting at screen X Y (default 512 300)
#
# Env: SERIAL (default 6aeccbe4), TREE (the Android tree), DRAG_DY (px, default 120). drag
# writes to /dev/input/event3, the board's touchscreen.
# Never `pkill -f "cog ..."`: inside `adb shell` the pattern matches the shell itself.
set -e
HERE="$(cd "$(dirname "$0")" && pwd)"
TREE="${TREE:-$(cd "$HERE/../../../.." && pwd)}"
SERIAL="${SERIAL:-6aeccbe4}"
BIN="$TREE/out_rb3/target/product/sdv_media_db845c/system/bin/smarthome_app"
# the design and the fonts the app loads (smarthome-app's data/)
DATA="$TREE/packages/services/display_safety/utils/smarthome-app/data"
# System first: with the APEX first the process loads two gRPC copies and aborts before main.
LDP=/system/lib64:/vendor/lib64:/apex/com.everyware.smarthome/lib64
URL_DEFAULT="about:blank"
R=/data/glibc/rootfs
FIFO=/data/local/tmp/har-browser-open
EVENTS=/data/local/tmp/har-browser-events
# the Debian user browser (docs/project/browser.md); no Android user has this gid
BROWSER_ID=2900
A() { adb -s "$SERIAL" "$@"; }

kill_old() {
  # by pidof, in one shell on the board; the names never match the shell's own command line
  # (the relay is a shell: by its pid file)
  A shell "kill \$(cat $R/tmp/har-browser-relay.pid 2>/dev/null) 2>/dev/null; rm -f $R/tmp/har-browser-relay.pid;
           kill \$(pidof cog WPEWebProcess WPENetworkProcess dbus-daemon smarthome_zone) 2>/dev/null; sleep 1;
           kill -9 \$(pidof cog WPEWebProcess WPENetworkProcess dbus-daemon smarthome_zone) 2>/dev/null; true"
}

cmd_start() {
  local redraw="" fps="" plane=1 url="$URL_DEFAULT"
  while [ $# -gt 0 ]; do
    case "$1" in
      --zone-redraw) redraw="${2:?--zone-redraw needs timer or frames}"; shift 2 ;;
      --zone-fps) fps="${2:?--zone-fps needs a number}"; shift 2 ;;
      --zone-plane) plane=1; shift ;;
      --no-zone-plane) plane=""; shift ;;
      -*) echo "unknown option $1" >&2; exit 2 ;;
      *) url="$1"; shift ;;
    esac
  done
  local flags=""
  [ -n "$redraw" ] && flags="$flags --zone-redraw $redraw"
  [ -n "$fps" ] && flags="$flags --zone-fps $fps"
  [ -n "$plane" ] && flags="$flags --zone-plane"
  A root >/dev/null 2>&1 || true
  A wait-for-device
  for _ in $(seq 20); do [ "$(A shell id -u | tr -d '\r')" = 0 ] && break; sleep 1; done
  # before the installed panel is stopped: a start that cannot work leaves the board as it is
  [ -f "$BIN" ] || { echo "!! $BIN is not built (m smarthome_app)" >&2; exit 1; }
  A shell 'test -x /data/glibc/rootfs/usr/bin/cog' || { echo "!! no Debian root with Cog on the board (docs/project/browser.md)" >&2; exit 1; }
  A shell stop smarthome || true
  kill_old
  A push "$BIN" /data/local/tmp/smarthome_zone >/dev/null
  A shell 'mkdir -p /data/local/tmp/zone-data' && A push "$DATA/." /data/local/tmp/zone-data/ >/dev/null
  A push "$HERE/glibc-enter.sh" /data/glibc/ >/dev/null
  A push "$HERE/har-browser-relay.sh" $R/usr/local/bin/har-browser-relay >/dev/null
  A shell "mkdir -p $R/etc/har" && A push "$HERE/browser-bus.conf" $R/etc/har/browser-bus.conf >/dev/null
  A push "$HERE/har-browser-cog.sh" $R/usr/local/bin/har-browser-cog >/dev/null
  A shell "chmod 755 $R/usr/local/bin/har-browser-relay $R/usr/local/bin/har-browser-cog;
           for f in $FIFO $EVENTS; do rm -f \$f; mkfifo -m 0660 \$f; chown root:$BROWSER_ID \$f; done"
  A shell 'rm -f /data/local/tmp/har-display /data/local/tmp/har-display.lock; chmod 755 /data/local/tmp/smarthome_zone'
  A shell "cd /data/local/tmp; LD_LIBRARY_PATH=$LDP nohup ./smarthome_zone \
    --data-path /data/local/tmp/zone-data --document-id smarthome --touch-size 1920x1080 \
    --wayland-zones$flags > zone-master.log 2>&1 &" >/dev/null 2>&1
  for _ in $(seq 30); do
    A shell 'test -S /data/local/tmp/har-display' 2>/dev/null && break
    sleep 1
  done
  A shell 'test -S /data/local/tmp/har-display' || { echo "!! master did not open the socket" >&2; exit 1; }
  local dbg=""
  [ -n "$WEBKIT_DEBUG" ] && dbg="WEBKIT_DEBUG=$WEBKIT_DEBUG"
  # The D-Bus session bus of the root (browser-bus.conf): a socket in the browser's own 0700
  # directory, so only the user browser reaches it (Cog listens there for cogctl); no service
  # activation, no system bus.
  local bus=unix:path=/tmp/har-dbus/bus
  A shell "nohup sh /data/glibc/glibc-enter.sh /bin/sh -c \"exec runuser -u browser -- sh -c '\
    rm -rf /tmp/har-dbus; mkdir -m 0700 /tmp/har-dbus; \
    exec dbus-daemon --config-file=/etc/har/browser-bus.conf --nofork --nopidfile' \
    > /tmp/har-dbus.log 2>&1\" >/dev/null 2>&1 &" >/dev/null 2>&1
  for _ in $(seq 10); do A shell "test -S $R/tmp/har-dbus/bus" 2>/dev/null && break; sleep 0.5; done
  A shell "nohup sh /data/glibc/glibc-enter.sh /bin/sh -c \"exec runuser -u browser -- \
    env HOME=/home/browser XDG_RUNTIME_DIR=/tmp WAYLAND_DISPLAY=/run/har/har-display \
    DBUS_SESSION_BUS_ADDRESS=$bus PULSE_SERVER=unix:/run/pulse/native \
    WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1 FD_MESA_DEBUG=noubwc,notile $dbg \
    har-browser-cog '$url'\" >/dev/null 2>&1 &" >/dev/null 2>&1
  A shell "nohup sh /data/glibc/glibc-enter.sh /bin/sh -c \"exec runuser -u browser -- \
    env DBUS_SESSION_BUS_ADDRESS=$bus har-browser-relay > /tmp/har-browser-relay.log 2>&1\" \
    >/dev/null 2>&1 &" >/dev/null 2>&1
  echo "started: master [${flags:- default}] cog $url, relay on $FIFO ${dbg:+($dbg)}"
}

cmd_stop() {
  kill_old
  A shell start smarthome
}

cmd_measure() {
  A push "$HERE/measure.sh" /data/local/tmp/measure.sh >/dev/null
  A shell sh /data/local/tmp/measure.sh "${1:-10}"
}

# One shell script on the board writes raw input_event records (24 bytes: time 16, type, code,
# value) to the touchscreen, one write per frame, usleep between frames: no adb round trip per
# event. Axes are 0..32767 for the 1920x1080 screen.
cmd_drag() {
  local n="${1:?usage: drag N [X Y]}" x="${2:-512}" y="${3:-300}" dy="${DRAG_DY:-120}"
  local tmp; tmp="$(mktemp)"
  python3 - "$n" "$x" "$y" "$dy" > "$tmp" <<'PY'
import sys
n, x, y, dy = map(int, sys.argv[1:5])
def sx(v): return min(32767, v * 32767 // 1919)
def sy(v): return min(32767, v * 32767 // 1079)
def ev(t, c, v):
    b = bytes(16) + t.to_bytes(2, 'little') + c.to_bytes(2, 'little') + (v & 0xffffffff).to_bytes(4, 'little')
    return ''.join('\\%03o' % x for x in b)
def frame(*evs): return "printf '%s' > $D\n" % ''.join(ev(*e) for e in evs + ((0, 0, 0),))
print("D=/dev/input/event3")
steps = 20
for i in range(n):
    print(frame((3, 0x39, i + 1), (3, 0x35, sx(x)), (3, 0x36, sy(y)), (3, 0, sx(x)), (3, 1, sy(y)), (1, 0x14a, 1)), end='')
    for s in range(1, steps + 1):
        py = y - dy * s // steps
        print("usleep 16000")
        print(frame((3, 0x35, sx(x)), (3, 0x36, sy(py)), (3, 0, sx(x)), (3, 1, sy(py))), end='')
    print("usleep 16000")
    print(frame((3, 0x39, -1), (1, 0x14a, 0)), end='')
    print("usleep 400000")
PY
  A push "$tmp" /data/local/tmp/drag.sh >/dev/null
  rm -f "$tmp"
  A shell sh /data/local/tmp/drag.sh
}

sub="${1:-}"; [ $# -gt 0 ] && shift
case "$sub" in
  start) cmd_start "$@" ;;
  stop) cmd_stop ;;
  measure) cmd_measure "$@" ;;
  drag) cmd_drag "$@" ;;
  *) sed -n '2,28p' "$0"; exit 2 ;;
esac
