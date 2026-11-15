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

# browser-root.sh: makes the Debian root of the panel's browser on a device. The browser is
# experimental (docs/project/browser.md). An image built with EVERYWARE_EXPERIMENTAL_BROWSER=true
# carries the root as the APEX com.everyware.browser; this script makes what goes into it:
#
#   browser-root.sh [-s SERIAL] --export [--update-lock]
#       the image for the APEX: browser/apex/prebuilt/<arch>/rootfs.erofs and its manifest,
#       made on this device (a board of the architecture, with its network) and copied to the
#       host. Local only, not in git; the build checks it against browser/apex/root.lock.
#   browser-root.sh [-s SERIAL] [--force]
#       the same root in /data/glibc/rootfs, writable, for trying things with apt, entered by
#       hand. The service never runs it: it runs only the APEX's root, which is verified.
#
# The root, the same each time for one root.lock:
#   1. Debian 13 (trixie): the layer of Docker Hub's official debian:trixie image for the
#      device's architecture (arm64 or amd64), the one root.lock names (the newest when it names
#      none), its sha256 checked, kept in ~/.cache/everyware;
#   2. apt from snapshot.debian.org at the date the layer was made from (written in its
#      debian.sources): the archive as it was then, apt still checks its signature;
#   3. apt-get install cog dbus libgles2 (WebKit's renderer opens libGLESv2.so.2) and the user
#      browser (uid and gid 2900);
#   4. --export: the root made read-only (resolv.conf and machine-id in /run, which
#      glibc-enter.sh writes; no apt lists or logs), its packages compared with root.lock (a
#      root.lock without the architecture gets its lines; --update-lock takes new ones), then
#      packed on the host with the tree's mkfs.erofs.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
APEX_DIR="$HERE/../browser/apex"
LOCK="$APEX_DIR/root.lock"
SERIAL="${ANDROID_SERIAL:-}"
FORCE=0 EXPORT=0 UPDATE_LOCK=0
while [ $# -gt 0 ]; do
    case "$1" in
        -s) SERIAL="$2"; shift 2 ;;
        --force) FORCE=1; shift ;;
        --export) EXPORT=1; shift ;;
        --update-lock) UPDATE_LOCK=1; shift ;;
        *) echo "usage: $0 [-s SERIAL] [--force] | [-s SERIAL] --export [--update-lock]" >&2; exit 2 ;;
    esac
done
A() { if [ -n "$SERIAL" ]; then adb -s "$SERIAL" "$@"; else adb "$@"; fi; }
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/everyware"
# free on /data: the root with Cog and its libraries, room for apt; for an export also its tar
NEED_MB=1536
# the panel's socket directory: glibc-enter.sh binds it at /run/har once and keeps the first
# bind, so the root is entered with the service's directory (everyware-browser.sh)
SOCKDIR=/data/vendor/everyware/run

A root >/dev/null
A wait-for-device
abi=$(A shell getprop ro.product.cpu.abi | tr -d '\r')
case "$abi" in
    arm64-v8a) arch=arm64; variant=v8 ;;
    x86_64) arch=amd64; variant= ;;
    *) echo "!! no Debian root for the ABI '$abi'" >&2; exit 1 ;;
esac
if [ "$EXPORT" = 1 ]; then
    R=/data/glibc/export
    NEED_MB=2560
else
    R=/data/glibc/rootfs
    if [ "$FORCE" = 0 ] && A shell "test -x $R/usr/bin/cog"; then
        echo "the root is there ($R has Cog); --force sets it up again"
        exit 0
    fi
fi
lock_layer=$(sed -n "s/^$arch layer //p" "$LOCK")
lock_snapshot=$(sed -n "s/^$arch snapshot //p" "$LOCK")
if [ "$UPDATE_LOCK" = 1 ]; then
    lock_layer= lock_snapshot=
fi

# 1. the layer of debian:trixie for this architecture, from Docker Hub's registry API: the one
# of root.lock, or the newest
mkdir -p "$CACHE"
read -r layer digest < <(python3 -I - "$arch" "$variant" "$CACHE" "$lock_layer" <<'PY'
import hashlib, json, os, sys, urllib.request

arch, variant, cache, digest = sys.argv[1], sys.argv[2] or None, sys.argv[3], sys.argv[4]
registry = "https://registry-1.docker.io/v2/library/debian"

def get(url, headers):
    with urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=60) as r:
        return r.read()

token = json.loads(get("https://auth.docker.io/token?service=registry.docker.io"
                       "&scope=repository:library/debian:pull", {}))["token"]
auth = {"Authorization": "Bearer " + token}
if not digest:
    index = json.loads(get(registry + "/manifests/trixie", dict(auth, Accept=(
        "application/vnd.oci.image.index.v1+json,"
        " application/vnd.docker.distribution.manifest.list.v2+json"))))
    entry = next(m for m in index["manifests"]
                 if m.get("platform", {}).get("os") == "linux"
                 and m["platform"].get("architecture") == arch
                 and m["platform"].get("variant") == variant)
    manifest = json.loads(get(registry + "/manifests/" + entry["digest"],
                              dict(auth, Accept=entry["mediaType"])))
    layers = manifest["layers"]
    if len(layers) != 1:
        sys.exit(f"debian:trixie {arch} has {len(layers)} layers, expected one")
    digest = layers[0]["digest"]
path = os.path.join(cache, f"debian-trixie-{arch}-{digest.split(':')[1][:16]}.tar.gz")
if not os.path.exists(path):
    data = get(registry + "/blobs/" + digest, auth)
    if "sha256:" + hashlib.sha256(data).hexdigest() != digest:
        sys.exit(f"the layer of debian:trixie {arch} does not match {digest}")
    with open(path + ".part", "wb") as f:
        f.write(data)
    os.rename(path + ".part", path)
print(path, digest)
PY
)
[ -n "${digest:-}" ] || { echo "!! no layer of debian:trixie for $arch" >&2; exit 1; }
echo "debian:trixie $arch: $digest"

# what an earlier run left counts as free
A shell "rm -rf $R /data/glibc/export.tar"
free_mb=$(( $(A shell "df -k /data | tail -1" | awk '{print $4}') / 1024 ))
if [ "$free_mb" -lt "$NEED_MB" ]; then
    echo "!! /data has $free_mb MB free, the root needs about $NEED_MB MB" >&2
    echo "   (Cuttlefish: an image with EVERYWARE_EXPERIMENTAL_BROWSER=true has a 4 GiB /data)" >&2
    exit 1
fi

# 2. unpacked on the device, apt pointed at the archive of the layer's date
A shell "mkdir -p $R"
A push "$layer" /data/glibc/rootfs.tar.gz >/dev/null
A shell "cd $R && tar -xzf /data/glibc/rootfs.tar.gz && rm /data/glibc/rootfs.tar.gz"
A push "$HERE/glibc-enter.sh" /data/glibc/glibc-enter.sh >/dev/null
snapshot=${lock_snapshot:-$(A shell "sed -n 's|^# http://snapshot.debian.org/archive/debian/\([0-9TZ]*\)$|\1|p' $R/etc/apt/sources.list.d/debian.sources" | tr -d '\r' | head -1)}
[ -n "$snapshot" ] || { echo "!! the layer names no snapshot of the Debian archive" >&2; exit 1; }
A shell "cat > $R/etc/apt/sources.list.d/debian.sources" <<EOF
# The Debian archive as it was at $snapshot (scripts/browser-root.sh of everyware)
Types: deb
URIs: http://snapshot.debian.org/archive/debian/$snapshot
Suites: trixie trixie-updates
Components: main
Signed-By: /usr/share/keyrings/debian-archive-keyring.pgp
Check-Valid-Until: no

Types: deb
URIs: http://snapshot.debian.org/archive/debian-security/$snapshot
Suites: trixie-security
Components: main
Signed-By: /usr/share/keyrings/debian-archive-keyring.pgp
Check-Valid-Until: no
EOF
echo "unpacked into $R, apt from the archive of $snapshot"

# 3. Cog, D-Bus and the user browser inside
enter() { A shell "GLIBC_ROOT=$R HAR_SOCKET_DIR=$SOCKDIR sh /data/glibc/glibc-enter.sh $*"; }
enter "/bin/sh -c '
    export DEBIAN_FRONTEND=noninteractive
    apt-get update &&
    apt-get install -y cog dbus libgles2 &&
    { getent group browser >/dev/null || groupadd -g 2900 browser; } &&
    { id browser >/dev/null 2>&1 || useradd -u 2900 -g 2900 -m -d /home/browser -s /bin/sh browser; }
'"
A shell "test -x $R/usr/bin/cog" || { echo "!! Cog is not in the root after apt" >&2; exit 1; }
echo "Cog $(enter cog --version 2>/dev/null | tr -d '\r' | head -1)"

if [ "$EXPORT" = 0 ]; then
    echo "enter it: adb shell GLIBC_ROOT=$R sh /data/glibc/glibc-enter.sh"
    exit 0
fi

# 4. read-only: what the root writes at run time goes to /run (a tmpfs of glibc-enter.sh) or to
# the browser's home (bound from /data); the mount points of glibc-enter.sh are there. The
# machine id is made per boot, not one for every board of the image.
enter "/bin/sh -c '
    apt-get clean &&
    rm -rf /var/lib/apt/lists/* /var/log/apt/* /var/log/*.log /home/browser/* &&
    rm -f /etc/resolv.conf /etc/machine-id /var/lib/dbus/machine-id &&
    ln -s ../run/resolv.conf /etc/resolv.conf &&
    ln -s ../run/machine-id /etc/machine-id &&
    ln -s /etc/machine-id /var/lib/dbus/machine-id &&
    mkdir -p /usr/local/lib/har-browser &&
    dpkg-query -W -f \"\\\${Package}:\\\${Architecture} \\\${Version}\\n\" | LC_ALL=C sort > /run/packages
'"
packages=$(A shell "cat $R/run/packages" | tr -d '\r')
[ -n "$packages" ] || { echo "!! no package list from the root" >&2; exit 1; }
# everything glibc-enter.sh mounted in the root, the deepest first
A shell "grep ' $R/' /proc/mounts | cut -d' ' -f2 | sort -r | while read -r m; do umount -l \$m; done"
if A shell "grep -q ' $R/' /proc/mounts"; then
    echo "!! $R still has mounts" >&2
    exit 1
fi

lines=$(
    echo "$arch layer $digest"
    echo "$arch snapshot $snapshot"
    echo "$packages" | sed "s/^/$arch package /"
)
locked=$(grep "^$arch " "$LOCK" || true)
if [ -n "$locked" ] && [ "$UPDATE_LOCK" = 0 ] && [ "$locked" != "$lines" ]; then
    echo "!! the root has other packages than root.lock names for $arch:" >&2
    diff <(echo "$locked") <(echo "$lines") | grep '^[<>]' | head -20 >&2 || true
    echo "   take them with --update-lock" >&2
    exit 1
fi

# packed on the host: the tree's mkfs.erofs, owners and modes of the tar
TREE="$(cd "$HERE/../../../.." && pwd)"
MKFS=${MKFS_EROFS:-$(command -v mkfs.erofs || ls "$TREE"/{out,out_rb3,cuttlefish_out,raspberry_out}/host/linux-x86/bin/mkfs.erofs 2>/dev/null | head -1 || true)}
[ -x "$MKFS" ] || { echo "!! no mkfs.erofs: m mkfs.erofs, or MKFS_EROFS=<path>" >&2; exit 1; }
tar="$CACHE/browser-root-$arch.tar"
echo "copying the root to $tar"
# a file and adb pull: a stream through adb exec-out is about a hundred times slower
A shell "tar --numeric-owner -C $R -cf /data/glibc/export.tar ."
A pull /data/glibc/export.tar "$tar" >/dev/null
A shell "rm -f /data/glibc/export.tar"
tar -tf "$tar" ./usr/bin/cog >/dev/null || { echo "!! the copy of the root has no Cog" >&2; exit 1; }
out="$APEX_DIR/prebuilt/$arch"
mkdir -p "$out"
rm -f "$out/rootfs.erofs" "$out/rootfs.erofs.manifest"
# the files keep their times: fontconfig takes its caches as current by them
"$MKFS" -zlz4hc --tar=f "$out/rootfs.erofs" "$tar" >/dev/null
rm -f "$tar"
{
    echo "$lines"
    echo "sha256 $(sha256sum "$out/rootfs.erofs" | cut -d' ' -f1)"
} > "$out/rootfs.erofs.manifest"
if [ "$locked" != "$lines" ]; then
    { grep -v "^$arch " "$LOCK" || true; echo "$lines"; } > "$LOCK.new"
    mv "$LOCK.new" "$LOCK"
    echo "root.lock: the lines of $arch written (commit them)"
fi
A shell "rm -rf $R"
echo "the image: $out/rootfs.erofs ($(( $(stat -c %s "$out/rootfs.erofs") / 1048576 )) MiB), Cog $(echo "$packages" | sed -n 's/^cog:[a-z0-9]* //p')"
