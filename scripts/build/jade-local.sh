#!/usr/bin/env bash
# Clone and build the Jade bundle locally (requires Docker on the host).
# Produces: out/hwwtui-jade-linux-x86_64.tar.gz
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORK_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
BUILD_DIR="${WORK_DIR}/.build-cache/jade"

mkdir -p "${BUILD_DIR}" "${WORK_DIR}/out"

# Clone if needed
if [ ! -d "${BUILD_DIR}/jade-firmware" ]; then
    echo "==> Cloning Jade firmware..."
    git clone --recurse-submodules https://github.com/Blockstream/Jade.git "${BUILD_DIR}/jade-firmware"
fi

cd "${BUILD_DIR}"
FIRMWARE_DIR="${BUILD_DIR}/jade-firmware" BUNDLE_VERSION=dev "${SCRIPT_DIR}/jade.sh"
cp hwwtui-jade-linux-x86_64.tar.gz "${WORK_DIR}/out/"
echo "==> Bundle at out/hwwtui-jade-linux-x86_64.tar.gz"
