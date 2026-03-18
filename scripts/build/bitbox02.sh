#!/usr/bin/env bash
# Build and package the BitBox02 simulator bundle.
#
# Expected environment:
#   BUNDLE_VERSION  – version string for bundle-info.json (default: "dev")
#   FIRMWARE_DIR    – path to checked-out bitbox02-firmware repo (default: "bitbox02-firmware")
#
# Produces: hwwtui-bitbox02-linux-x86_64.tar.gz in the current directory.
set -euo pipefail

BUNDLE_VERSION="${BUNDLE_VERSION:-dev}"
FIRMWARE_DIR="${FIRMWARE_DIR:-bitbox02-firmware}"
PLATFORM="linux-x86_64"
BUNDLE_DIR="hwwtui-bitbox02-${PLATFORM}"

echo "==> Building BitBox02 simulator from ${FIRMWARE_DIR}"

cd "${FIRMWARE_DIR}"
mkdir -p build-sim && cd build-sim
cmake .. \
    -GNinja \
    -DCMAKE_BUILD_TYPE=Release \
    -DBUILD_TYPE=simulator
ninja
cd ../..

# Locate the simulator binary (name varies across versions).
BIN=$(find "${FIRMWARE_DIR}/build-sim" -type f \( -name 'bitbox02_simulator*' -o -name 'simulator' \) | head -1)
if [ -z "${BIN}" ]; then
    echo "ERROR: simulator binary not found in ${FIRMWARE_DIR}/build-sim/"
    find "${FIRMWARE_DIR}/build-sim" -type f -executable | head -20
    exit 1
fi
echo "==> Found simulator binary: ${BIN}"

echo "==> Packaging bundle: ${BUNDLE_DIR}"
rm -rf "${BUNDLE_DIR}"
mkdir -p "${BUNDLE_DIR}"

cp "${BIN}" "${BUNDLE_DIR}/bitbox02-simulator"
chmod +x "${BUNDLE_DIR}/bitbox02-simulator"

CONTENTS=$(cd "${BUNDLE_DIR}" && find . -type f | sort | jq -R -s 'split("\n") | map(select(length > 0))')
cat > "${BUNDLE_DIR}/bundle-info.json" <<EOF
{
  "wallet_type": "bitbox02",
  "version": "${BUNDLE_VERSION}",
  "platform": "${PLATFORM}",
  "build_date": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "contents": ${CONTENTS}
}
EOF

tar czf "hwwtui-bitbox02-${PLATFORM}.tar.gz" "${BUNDLE_DIR}"
echo "==> Done: hwwtui-bitbox02-${PLATFORM}.tar.gz"
