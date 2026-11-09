#!/bin/bash
# cargo.sh: cargo for the .dcf tools, with the tree's toolchain and its outputs in the tree's
# out/, so that the repository stays clean and no cargo has to be installed.
#
#   tools/dcf/cargo.sh run -q --bin dcf-fetch -- --local <dir> --nodes '#tv-root' --output tv.dcf
#   tools/dcf/cargo.sh build --release
#
# Env:
#   DCF_FORK   a local clone of the DesignCompose fork instead of GitHub (cargo's source
#              replacement, the same branch): for a change to the importer before it is pushed
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
TREE=$(cd "$HERE/../../../../.." && pwd)
CARGO=$(ls -d "$TREE"/prebuilts/rust-toolchain/linux-x86/*/bin/cargo 2>/dev/null | sort -V | tail -1 || true)
[ -n "$CARGO" ] || { echo "no cargo under $TREE/prebuilts/rust-toolchain" >&2; exit 1; }
export PATH="$(dirname "$CARGO"):$PATH"
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$TREE/out/dcf-tools}
export CARGO_HOME=${CARGO_HOME:-$TREE/out/dcf-tools/cargo-home}
ARGS=()
if [ -n "${DCF_FORK:-}" ]; then
    FORK=$(realpath "$DCF_FORK")
    URL='https://github.com/sdv-everyware/automotive-design-compose'
    ARGS+=(--config "source.fork.git=\"$URL\"" --config 'source.fork.branch="everyware"'
           --config 'source.fork.replace-with="local-fork"'
           --config "source.local-fork.git=\"file://$FORK\"" --config 'source.local-fork.branch="everyware"')
fi
# the subcommand first, then the manifest: relative paths in the arguments stay the caller's
SUB=$1; shift
exec cargo "${ARGS[@]}" "$SUB" --manifest-path "$HERE/Cargo.toml" "$@"
