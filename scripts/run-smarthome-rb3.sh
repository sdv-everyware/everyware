#!/bin/bash
# Backwards-compatible RB3 entry point; see run-smarthome.sh (supports --target rb3|cf).
TARGET=rb3 exec "$(dirname "$0")/run-smarthome.sh" "$@"
