#!/usr/bin/env bash
# Clone and build the Ledger bundle locally (requires Docker on the host).
# Produces: out/hwwtui-ledger-linux-x86_64.tar.gz
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORK_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
BUILD_DIR="${WORK_DIR}/.build-cache/ledger"

mkdir -p "${BUILD_DIR}" "${WORK_DIR}/out"

# Clone if needed
if [ ! -d "${BUILD_DIR}/app-bitcoin-new" ]; then
    echo "==> Cloning app-bitcoin-new..."
    git clone --recurse-submodules https://github.com/LedgerHQ/app-bitcoin-new.git "${BUILD_DIR}/app-bitcoin-new"
fi

cd "${BUILD_DIR}"
FIRMWARE_DIR="${BUILD_DIR}/app-bitcoin-new" BUNDLE_VERSION=dev "${SCRIPT_DIR}/ledger.sh"
cp hwwtui-ledger-linux-x86_64.tar.gz "${WORK_DIR}/out/"
echo "==> Bundle at out/hwwtui-ledger-linux-x86_64.tar.gz"
