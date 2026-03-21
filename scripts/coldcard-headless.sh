#!/usr/bin/env bash
# Headless launcher for the Coldcard simulator.
# Usage: coldcard-headless.sh <micropython_binary> <bundle_dir> <socket_path>
#
# This script replicates what simulator.py does in --headless mode,
# without requiring Python3 or SDL2 on the host.

set -euo pipefail

MPY_BIN="$1"
BUNDLE_DIR="$2"
SOCKET_PATH="${3:-/tmp/ckcc-simulator.sock}"

SHARED_DIR="${BUNDLE_DIR}/shared"
UNIX_DIR="${BUNDLE_DIR}/unix"
WORK_DIR="${UNIX_DIR}/work"

# Create work directories if they don't exist
mkdir -p "${WORK_DIR}/MicroSD" "${WORK_DIR}/settings" "${WORK_DIR}/VirtDisk" "${WORK_DIR}/debug"

# Clean up stale socket
rm -f "${SOCKET_PATH}"

# Open /dev/null file descriptors for display, LED, and data pipes
# (the simulator expects these as positional arguments)
exec 10>/dev/null  # display_w
exec 11>/dev/null  # led_w
exec 12>/dev/null  # data_r

cd "${WORK_DIR}"

export MICROPYPATH="${SHARED_DIR}:${UNIX_DIR}"

exec "${MPY_BIN}" -X heapsize=9m \
    -i "${UNIX_DIR}/sim_boot.py" \
    10 -1 11 12 \
    "${SOCKET_PATH}"
