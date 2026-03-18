#!/usr/bin/env bash
# Build and package the Trezor emulator bundle.
#
# Expected environment:
#   BUNDLE_VERSION  – version string for bundle-info.json (default: "dev")
#   FIRMWARE_DIR    – path to checked-out trezor-firmware repo (default: "trezor-firmware")
#
# Produces: hwwtui-trezor-linux-x86_64.tar.gz in the current directory.
set -euo pipefail

BUNDLE_VERSION="${BUNDLE_VERSION:-dev}"
FIRMWARE_DIR="$(cd "${FIRMWARE_DIR:-trezor-firmware}" && pwd)"
WORK_DIR="$(pwd)"
PLATFORM="linux-x86_64"
BUNDLE_DIR="${WORK_DIR}/hwwtui-trezor-${PLATFORM}"

echo "==> Building Trezor emulator from ${FIRMWARE_DIR}"

# Install Python deps globally so make's subprocess calls can find them.
# The Pipfile at the repo root lists everything (click, protobuf, etc.).
pip3 install --user -r <(cd "${FIRMWARE_DIR}" && pipenv requirements --dev 2>/dev/null || pip3 install --user pipenv && cd "${FIRMWARE_DIR}" && pipenv requirements --dev)

# Build the unix emulator.
cd "${FIRMWARE_DIR}/core"
make build_unix

# Verify binary exists.
BINARY="${FIRMWARE_DIR}/core/build/unix/trezor-emu-core"
if [ ! -f "${BINARY}" ]; then
    echo "ERROR: trezor-emu-core binary not found at ${BINARY}"
    exit 1
fi

echo "==> Packaging bundle: ${BUNDLE_DIR}"
cd "${WORK_DIR}"
rm -rf "${BUNDLE_DIR}"
mkdir -p "${BUNDLE_DIR}"

cp "${BINARY}" "${BUNDLE_DIR}/"
chmod +x "${BUNDLE_DIR}/trezor-emu-core"

# Copy the Python source tree the emulator loads at runtime.
rsync -a \
    --exclude='__pycache__' \
    --exclude='*.pyc' \
    --exclude='tests/' \
    "${FIRMWARE_DIR}/core/src/" "${BUNDLE_DIR}/src/"

# bundle-info.json
CONTENTS=$(cd "${BUNDLE_DIR}" && find . -type f | sort | jq -R -s 'split("\n") | map(select(length > 0))')
cat > "${BUNDLE_DIR}/bundle-info.json" <<EOF
{
  "wallet_type": "trezor",
  "version": "${BUNDLE_VERSION}",
  "platform": "${PLATFORM}",
  "build_date": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "contents": ${CONTENTS}
}
EOF

tar czf "${WORK_DIR}/hwwtui-trezor-${PLATFORM}.tar.gz" -C "${WORK_DIR}" "hwwtui-trezor-${PLATFORM}"
echo "==> Done: hwwtui-trezor-${PLATFORM}.tar.gz"
