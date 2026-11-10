#!/vendor/bin/sh
# everyware-browser.sh -- the browser of the panel, started by init (product/everyware_browser.rc)
# when the panel runs: Cog in the Debian root as the user browser, a client of the panel's
# Wayland zone #zone/browser, with its D-Bus bus and the relay that hands it the pages to open.
# What runs and why: docs/project/browser.md.
#
# The three processes are children of this script, as the supplicant is of net_bringup.sh:
# init knows one service. When one of them ends, or the panel's socket goes (the panel
# restarted), all three are stopped and started again.
# Without a Debian root on the device there is no browser: the script says so and ends.
RUN=/data/vendor/everyware/run
R=/data/glibc/rootfs
ENTER=/vendor/bin/glibc-enter.sh
# the helper scripts of the image, as the root sees them (glibc-enter.sh binds the directory)
LIB=/usr/local/lib/har-browser
# the Debian user browser; no Android user has this id
BROWSER_ID=2900
BUS=unix:path=/tmp/har-dbus/bus
log() { /vendor/bin/log -t everyware_browser "$*"; }

[ -x $R/usr/bin/cog ] || { log "no Debian root with Cog at $R: no browser"; exit 0; }

export GLIBC_ROOT=$R HAR_SOCKET_DIR=$RUN HAR_BROWSER_LIB=/vendor/etc/everyware/browser
# as the browser's user inside the root, with the environment given as arguments
as_browser() { /vendor/bin/sh $ENTER /bin/sh -c "exec runuser -u browser -- env $*"; }

stop_all() {
    # by name: the processes are grandchildren (chroot, runuser), not ours to signal by pid
    kill $(pidof cog WPEWebProcess WPENetworkProcess dbus-daemon) 2>/dev/null
    kill $bus $cog $relay 2>/dev/null
    sleep 1
    kill -9 $(pidof cog WPEWebProcess WPENetworkProcess dbus-daemon) 2>/dev/null
}
trap 'stop_all; exit 0' TERM INT

while :; do
    until [ -S $RUN/har-display ]; do sleep 1; done
    # the panel made the socket for itself; the browser's group has to reach it
    chown root:$BROWSER_ID $RUN/har-display
    chmod 0660 $RUN/har-display
    # new FIFOs on every start: a full pipe of an earlier run must not block Cog's output
    for f in $RUN/har-browser-open $RUN/har-browser-events; do
        rm -f $f
        mkfifo $f
        chown root:$BROWSER_ID $f
        # not mkfifo -m: the service's umask would take the group's bits away
        chmod 0660 $f
    done

    # the bus of the root (browser-bus.conf): a socket in a directory only the browser enters
    as_browser "sh -c 'rm -rf /tmp/har-dbus; mkdir -m 0700 /tmp/har-dbus; exec dbus-daemon --config-file=$LIB/browser-bus.conf --nofork --nopidfile' > /tmp/har-dbus.log 2>&1" &
    bus=$!
    i=0
    until [ -S $R/tmp/har-dbus/bus ] || [ $i -ge 20 ]; do sleep 0.5; i=$((i + 1)); done

    as_browser "HOME=/home/browser XDG_RUNTIME_DIR=/tmp WAYLAND_DISPLAY=/run/har/har-display \
        DBUS_SESSION_BUS_ADDRESS=$BUS WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1 \
        FD_MESA_DEBUG=noubwc,notile sh $LIB/har-browser-cog.sh about:blank" &
    cog=$!
    as_browser "DBUS_SESSION_BUS_ADDRESS=$BUS sh $LIB/har-browser-relay.sh > /tmp/har-browser-relay.log 2>&1" &
    relay=$!
    log "browser started (bus $bus, cog $cog, relay $relay)"

    while kill -0 $bus 2>/dev/null && kill -0 $cog 2>/dev/null && kill -0 $relay 2>/dev/null \
        && [ -S $RUN/har-display ]; do
        sleep 2
    done
    log "a part of the browser ended or the panel's socket went: starting it again"
    stop_all
    sleep 2
done
