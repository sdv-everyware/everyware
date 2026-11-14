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

# browser-root.sh: puts the Debian root of the panel's browser on a device. The browser is
# experimental (docs/project/browser.md): an image built with EVERYWARE_EXPERIMENTAL_BROWSER=true
# has its service and scripts, the root lives in /data and is not part of the image. This does
# what browser.md lists under "The Debian root, once per board", for any board:
#   1. Debian 13 (trixie): the layer of Docker Hub's official debian:trixie image for the device's
#      architecture (arm64 or amd64), its sha256 checked, kept in ~/.cache/everyware;
#   2. unpacked into /data/glibc/rootfs, glibc-enter.sh next to it;
#   3. inside: apt-get install cog dbus libgles2 (WebKit's renderer opens libGLESv2.so.2), and
#      the user browser (uid and gid 2900).
# The device needs its network for apt. A root that has Cog already is left as it is.
#
#   browser-root.sh [-s SERIAL] [--force]
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
SERIAL="${ANDROID_SERIAL:-}"
FORCE=0
while [ $# -gt 0 ]; do
    case "$1" in
        -s) SERIAL="$2"; shift 2 ;;
        --force) FORCE=1; shift ;;
        *) echo "usage: $0 [-s SERIAL] [--force]" >&2; exit 2 ;;
    esac
done
A() { if [ -n "$SERIAL" ]; then adb -s "$SERIAL" "$@"; else adb "$@"; fi; }
R=/data/glibc/rootfs
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/everyware"
NEED_MB=1536 # the root with Cog and its libraries, and room for apt

A root >/dev/null
A wait-for-device
abi=$(A shell getprop ro.product.cpu.abi | tr -d '\r')
case "$abi" in
    arm64-v8a) arch=arm64; variant=v8 ;;
    x86_64) arch=amd64; variant= ;;
    *) echo "!! no Debian root for the ABI '$abi'" >&2; exit 1 ;;
esac
if [ "$FORCE" = 0 ] && A shell "test -x $R/usr/bin/cog"; then
    echo "the root is there ($R has Cog); --force sets it up again"
    exit 0
fi

# 1. the layer of debian:trixie for this architecture, from Docker Hub's registry API
mkdir -p "$CACHE"
layer=$(python3 -I - "$arch" "$variant" "$CACHE" <<'PY'
import hashlib, json, os, sys, urllib.request

arch, variant, cache = sys.argv[1], sys.argv[2] or None, sys.argv[3]
registry = "https://registry-1.docker.io/v2/library/debian"

def get(url, headers):
    with urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=60) as r:
        return r.read()

token = json.loads(get("https://auth.docker.io/token?service=registry.docker.io"
                       "&scope=repository:library/debian:pull", {}))["token"]
auth = {"Authorization": "Bearer " + token}
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
print(path)
PY
)
echo "debian:trixie $arch: $layer"

free_mb=$(( $(A shell "df -k /data | tail -1" | awk '{print $4}') / 1024 ))
if [ "$free_mb" -lt "$NEED_MB" ]; then
    echo "!! /data has $free_mb MB free, the root needs about $NEED_MB MB" >&2
    echo "   (Cuttlefish: CF_DATA_MB=4096 tools/run.sh start)" >&2
    exit 1
fi

# 2. unpacked on the device
A shell "rm -rf $R && mkdir -p $R"
A push "$layer" /data/glibc/rootfs.tar.gz >/dev/null
A shell "cd $R && tar -xzf /data/glibc/rootfs.tar.gz && rm /data/glibc/rootfs.tar.gz"
A push "$HERE/glibc-enter.sh" /data/glibc/glibc-enter.sh >/dev/null
echo "unpacked into $R"

# 3. Cog, D-Bus and the user browser inside. glibc-enter.sh binds the socket directory at
# /run/har once and keeps the first bind: with the service's directory, as everyware-browser.sh
# enters, or the browser would not find the panel's socket until a reboot.
SOCKDIR=/data/vendor/everyware/run
A shell "HAR_SOCKET_DIR=$SOCKDIR sh /data/glibc/glibc-enter.sh /bin/sh -c '
    export DEBIAN_FRONTEND=noninteractive
    apt-get update &&
    apt-get install -y cog dbus libgles2 &&
    { getent group browser >/dev/null || groupadd -g 2900 browser; } &&
    { id browser >/dev/null 2>&1 || useradd -u 2900 -g 2900 -m -d /home/browser -s /bin/sh browser; }
'"
A shell "test -x $R/usr/bin/cog" || { echo "!! Cog is not in the root after apt" >&2; exit 1; }
echo "the root is ready: Cog $(A shell "HAR_SOCKET_DIR=$SOCKDIR sh /data/glibc/glibc-enter.sh cog --version" 2>/dev/null | tr -d '\r' | head -1)"
if [ "$(A shell getprop ro.vendor.everyware.browser | tr -d '\r')" = true ]; then
    A shell start everyware_browser && echo "started everyware_browser"
else
    echo "this image has no browser: build it with EVERYWARE_EXPERIMENTAL_BROWSER=true"
fi
