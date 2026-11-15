#!/system/bin/sh
# glibc-enter.sh -- run a command inside the Debian (glibc) root on the device, as a Wayland
# client of the HAR master. The development form of the glibc layer: a chroot under /data, runtime mounts
# only, root, the SELinux domain of the caller (the board is permissive). The product form is
# an APEX with its own domain (docs/project/browser.md, Open).
#
# On the device (adb root):
#   sh /data/glibc/glibc-enter.sh                    # a shell inside Debian
#   sh /data/glibc/glibc-enter.sh apt-get install -y cog
#   sh /data/glibc/glibc-enter.sh cog --platform=wl https://example.org
#
# Inside: XDG_RUNTIME_DIR=/run/har is the master's socket directory (HAR_SOCKET_DIR; without
# it /data/local/tmp, where a developer's build of the panel listens as har-display),
# WAYLAND_DISPLAY=har-display.
# The root may be read-only (GLIBC_ROOT: the image of com.everyware.browser): what is written at
# run time goes to tmpfs mounts (/dev, /tmp, /run, with resolv.conf and machine-id) and to
# HAR_BROWSER_HOME, a directory bound at /home/browser.
# DNS: the Wi-Fi DHCP lease (vendor.net.wlan0.dns*), as Android's own resolver has no glibc side.
set -e
# init starts a service with umask 077: the directories, the device nodes' parents and
# resolv.conf made here are for the root's users too
umask 022
R=${GLIBC_ROOT:-/data/glibc/rootfs}
SOCKDIR=${HAR_SOCKET_DIR:-/data/local/tmp}

mounted() { grep -q " $1 " /proc/mounts; }
bind() { mkdir -p "$2"; mounted "$2" || mount -o bind "$1" "$2"; }

mounted "$R/proc" || mount -t proc proc "$R/proc"
mounted "$R/sys" || mount -t sysfs sysfs "$R/sys"
# /dev: a tmpfs with only what the browser needs, not the host's /dev (which has the sound
# cards with their microphones, the input devices and the KMS card). HAR_CHROOT_SND=1 adds
# /dev/snd for a sound server inside the root; its group (Android's audio, gid 1005) has the
# user pulse only.
node() {
    # a device node with the host's numbers and mode (toybox mount cannot bind a single node)
    [ -c "$R/dev/$1" ] && return 0
    rm -f "$R/dev/$1"
    mkdir -p "$(dirname "$R/dev/$1")"
    set -- "$1" $(stat -c '%t %T %a' "/dev/$1")
    mknod -m "$4" "$R/dev/$1" c $((0x$2)) $((0x$3))
}
if mounted "$R/dev" && [ -e "$R/dev/kmsg" ]; then
    umount -l "$R/dev"                   # the host's whole /dev must not be in the root
fi
mounted "$R/dev" || mount -t tmpfs -o mode=0755,nosuid,noexec tmpfs "$R/dev"
# card0: WebKit lists DRM devices by their nodes and wants the primary one to exist; its mode
# (0660, root in here) keeps the browser from opening it, as on the host.
for n in null zero full random urandom tty dri/renderD128 dri/card0; do node "$n"; done
# also where an earlier entry made it under another umask: the browser has to reach renderD128
chmod 0755 "$R/dev/dri"
for l in fd:/proc/self/fd stdin:/proc/self/fd/0 stdout:/proc/self/fd/1 stderr:/proc/self/fd/2 ptmx:pts/ptmx; do
    [ -L "$R/dev/${l%%:*}" ] || ln -s "${l#*:}" "$R/dev/${l%%:*}"
done
[ "${HAR_CHROOT_SND:-0}" = 1 ] && bind /dev/snd "$R/dev/snd"
mkdir -p "$R/dev/pts"
mounted "$R/dev/pts" || mount -t devpts -o newinstance,ptmxmode=0666 devpts "$R/dev/pts"
mounted "$R/dev/shm" || { mkdir -p "$R/dev/shm"; mount -t tmpfs -o mode=1777 tmpfs "$R/dev/shm"; }
mounted "$R/tmp" || mount -t tmpfs -o mode=1777 tmpfs "$R/tmp"
mounted "$R/run" || mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs "$R/run"
bind "$SOCKDIR" "$R/run/har"
# the browser's helper scripts from the image (everyware-browser.sh), read by the root's shell
[ -n "$HAR_BROWSER_LIB" ] && bind "$HAR_BROWSER_LIB" "$R/usr/local/lib/har-browser"
# the browser's home, where the root is read-only (everyware-browser.sh)
[ -n "$HAR_BROWSER_HOME" ] && bind "$HAR_BROWSER_HOME" "$R/home/browser"

# in /run, where /etc/resolv.conf points (browser-root.sh --export makes the link; a writable
# root gets it here). Written beside and moved in place: two callers may enter at the same time.
[ -L "$R/etc/resolv.conf" ] || ln -sf ../run/resolv.conf "$R/etc/resolv.conf"
resolv="$R/run/resolv.conf.$$"
: > "$resolv"
for n in 1 2 3 4; do
  # by its path: a vendor shell (init's service) has only /vendor/bin on its PATH, and there
  # is no getprop there
  d=$(/system/bin/getprop vendor.net.wlan0.dns$n) || d=
  [ -n "$d" ] && echo "nameserver $d" >> "$resolv"
done
[ -s "$resolv" ] || echo "nameserver 8.8.8.8" > "$resolv"
mv "$resolv" "$R/run/resolv.conf"
# the machine id D-Bus reads (/etc/machine-id of an exported root points here): one per boot,
# not one for every board of the image
if [ ! -s "$R/run/machine-id" ]; then
    tr -d '-' < /proc/sys/kernel/random/uuid > "$R/run/machine-id.$$"
    mv "$R/run/machine-id.$$" "$R/run/machine-id"
fi

[ $# -gt 0 ] || set -- /bin/bash -l
exec chroot "$R" /usr/bin/env -i \
  HOME=/root TERM="${TERM:-xterm}" LANG=C.UTF-8 \
  PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
  XDG_RUNTIME_DIR=/run/har WAYLAND_DISPLAY=har-display \
  DEBIAN_FRONTEND=noninteractive \
  "$@"
