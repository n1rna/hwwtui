#!/usr/bin/env bash
# Build and package the Coldcard simulator bundle.
#
# Expected environment:
#   BUNDLE_VERSION  – version string for bundle-info.json (default: "dev")
#   FIRMWARE_DIR    – path to checked-out Coldcard firmware repo (default: "coldcard-firmware")
#
# Produces: hwwtui-coldcard-linux-x86_64.tar.gz in the current directory.
set -euo pipefail

BUNDLE_VERSION="${BUNDLE_VERSION:-dev}"
FIRMWARE_DIR="$(cd "${FIRMWARE_DIR:-coldcard-firmware}" && pwd)"
WORK_DIR="$(pwd)"
PLATFORM="linux-x86_64"
BUNDLE_DIR="${WORK_DIR}/hwwtui-coldcard-${PLATFORM}"

echo "==> Building Coldcard unix simulator from ${FIRMWARE_DIR}"

cd "${FIRMWARE_DIR}/unix"
make setup
make ngu-setup
make

# Locate the micropython binary (path varies by firmware version).
MP_BIN=$(find "${FIRMWARE_DIR}/unix" -maxdepth 2 -type f -name 'micropython' | head -1)
if [ -z "${MP_BIN}" ]; then
    echo "ERROR: micropython binary not found"
    find "${FIRMWARE_DIR}/unix" -type f -executable | head -20
    exit 1
fi
echo "==> Found micropython binary: ${MP_BIN}"

echo "==> Packaging bundle: ${BUNDLE_DIR}"
cd "${WORK_DIR}"
rm -rf "${BUNDLE_DIR}"
mkdir -p "${BUNDLE_DIR}"

cp "${MP_BIN}" "${BUNDLE_DIR}/micropython"
chmod +x "${BUNDLE_DIR}/micropython"

# Runtime data: shared/ has the Python modules the simulator imports.
rsync -a \
    --exclude='__pycache__' \
    --exclude='*.pyc' \
    "${FIRMWARE_DIR}/shared/" "${BUNDLE_DIR}/shared/"

rsync -a \
    --exclude='__pycache__' \
    --exclude='*.pyc' \
    --exclude='*.o' \
    "${FIRMWARE_DIR}/unix/" "${BUNDLE_DIR}/unix/"

CONTENTS=$(cd "${BUNDLE_DIR}" && find . -type f | sort | jq -R -s 'split("\n") | map(select(length > 0))')
cat > "${BUNDLE_DIR}/bundle-info.json" <<EOF
{
  "wallet_type": "coldcard",
  "version": "${BUNDLE_VERSION}",
  "platform": "${PLATFORM}",
  "build_date": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "contents": ${CONTENTS}
}
EOF

tar czf "${WORK_DIR}/hwwtui-coldcard-${PLATFORM}.tar.gz" -C "${WORK_DIR}" "hwwtui-coldcard-${PLATFORM}"
echo "==> Done: hwwtui-coldcard-${PLATFORM}.tar.gz"
