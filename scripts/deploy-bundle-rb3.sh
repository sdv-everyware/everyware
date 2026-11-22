#!/bin/bash
# Backwards-compatible RB3 entry point; see deploy-bundle.sh (supports --target rb3|cf).
TARGET=rb3 exec "$(dirname "$0")/deploy-bundle.sh" "$@"
