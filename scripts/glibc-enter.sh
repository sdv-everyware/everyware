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
# Inside: XDG_RUNTIME_DIR=/run/har is the master's socket directory (/data/local/tmp, where
# smarthome_zone --wayland-zones listens as har-display), WAYLAND_DISPLAY=har-display.
# DNS: the Wi-Fi DHCP lease (vendor.net.wlan0.dns*), as Android's own resolver has no glibc side.
set -e
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
for l in fd:/proc/self/fd stdin:/proc/self/fd/0 stdout:/proc/self/fd/1 stderr:/proc/self/fd/2 ptmx:pts/ptmx; do
    [ -L "$R/dev/${l%%:*}" ] || ln -s "${l#*:}" "$R/dev/${l%%:*}"
done
[ "${HAR_CHROOT_SND:-0}" = 1 ] && bind /dev/snd "$R/dev/snd"
mkdir -p "$R/dev/pts"
mounted "$R/dev/pts" || mount -t devpts -o newinstance,ptmxmode=0666 devpts "$R/dev/pts"
mounted "$R/dev/shm" || { mkdir -p "$R/dev/shm"; mount -t tmpfs -o mode=1777 tmpfs "$R/dev/shm"; }
mounted "$R/tmp" || mount -t tmpfs -o mode=1777 tmpfs "$R/tmp"
bind "$SOCKDIR" "$R/run/har"

: > "$R/etc/resolv.conf"
for n in 1 2 3 4; do
  d=$(getprop vendor.net.wlan0.dns$n)
  [ -n "$d" ] && echo "nameserver $d" >> "$R/etc/resolv.conf"
done
[ -s "$R/etc/resolv.conf" ] || echo "nameserver 8.8.8.8" > "$R/etc/resolv.conf"

[ $# -gt 0 ] || set -- /bin/bash -l
exec chroot "$R" /usr/bin/env -i \
  HOME=/root TERM="${TERM:-xterm}" LANG=C.UTF-8 \
  PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
  XDG_RUNTIME_DIR=/run/har WAYLAND_DISPLAY=har-display \
  DEBIAN_FRONTEND=noninteractive \
  "$@"
