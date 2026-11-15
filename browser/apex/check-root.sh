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

# check-root.sh: the build's check of the browser's Debian root (Android.bp). The image of an
# architecture goes into the APEX only when
#   - it is the image scripts/browser-root.sh --export made: its sha256 is the one of its
#     manifest (rootfs.erofs.manifest, written beside it);
#   - it was made from the locked versions: the manifest's lines of the architecture (the
#     Debian layer, the snapshot of the archive, every package with its version) are the ones
#     of root.lock.
# This does not prove where the image came from (anyone can write both files); it catches an
# image copied in part, an old one, or one made from other versions than the tree names.
# Without an image of the architecture the output is empty: this runs for both architectures
# (checkbuild), and build/tasks/browser.mk stops a product build that lacks the image of its own.
#
#   check-root.sh <arch> <root.lock> <out> <the files of prebuilt/<arch>/>
set -euo pipefail

arch=$1 lock=$2 out=$3
shift 3
fail() {
    echo "com.everyware.browser ($arch): $*" >&2
    exit 1
}
image= manifest=
for f in "$@"; do
    case "$f" in
        */rootfs.erofs) image=$f ;;
        */rootfs.erofs.manifest) manifest=$f ;;
    esac
done
if [ -z "$image" ] && [ -z "$manifest" ]; then
    : > "$out"
    exit 0
fi
[ -n "$image" ] && [ -n "$manifest" ] ||
    fail "browser/apex/prebuilt/$arch has only one of rootfs.erofs and rootfs.erofs.manifest:
    copy both again, or export again"
grep -q "^$arch " "$lock" ||
    fail "root.lock has no lines for $arch: export once on a $arch board, which writes them"

want=$(sed -n 's/^sha256 //p' "$manifest")
got=$(sha256sum "$image" | cut -d' ' -f1)
[ "$want" = "$got" ] ||
    fail "rootfs.erofs is not the image of its manifest (sha256 $got, the manifest has
    ${want:-none}): copy both files again, or export again"
[ "$(grep "^$arch " "$lock")" = "$(grep "^$arch " "$manifest")" ] ||
    fail "the image was made from other versions than root.lock names: export again
    (browser-root.sh -s <serial> --export)"
cp "$image" "$out"
