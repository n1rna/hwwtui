#!/usr/bin/env bash
# Build and package the Specter DIY simulator bundle.
#
# Expected environment:
#   BUNDLE_VERSION  – version string for bundle-info.json (default: "dev")
#   FIRMWARE_DIR    – path to checked-out specter-diy repo (default: "specter-diy")
#
# Produces: hwwtui-specter-linux-x86_64.tar.gz in the current directory.
set -euo pipefail

BUNDLE_VERSION="${BUNDLE_VERSION:-dev}"
FIRMWARE_DIR="$(cd "${FIRMWARE_DIR:-specter-diy}" && pwd)"
WORK_DIR="$(pwd)"
PLATFORM="linux-x86_64"
BUNDLE_DIR="${WORK_DIR}/hwwtui-specter-${PLATFORM}"

echo "==> Building Specter unix simulator from ${FIRMWARE_DIR}"

cd "${FIRMWARE_DIR}"
make unix || make micropython_unix

# Locate binary.
BIN=$(find "${FIRMWARE_DIR}" -maxdepth 3 -type f \( -name 'micropython_unix' -o -name 'micropython' \) | head -1)
if [ -z "${BIN}" ]; then
    echo "ERROR: micropython unix binary not found"
    find "${FIRMWARE_DIR}" -type f -executable | head -20
    exit 1
fi
echo "==> Found binary: ${BIN}"

echo "==> Packaging bundle: ${BUNDLE_DIR}"
cd "${WORK_DIR}"
rm -rf "${BUNDLE_DIR}"
mkdir -p "${BUNDLE_DIR}"

cp "${BIN}" "${BUNDLE_DIR}/micropython_unix"
chmod +x "${BUNDLE_DIR}/micropython_unix"

# Copy boot script and source tree needed at runtime.
[ -f "${FIRMWARE_DIR}/boot.py" ] && cp "${FIRMWARE_DIR}/boot.py" "${BUNDLE_DIR}/"
if [ -d "${FIRMWARE_DIR}/src" ]; then
    rsync -a \
        --exclude='__pycache__' \
        --exclude='*.pyc' \
        "${FIRMWARE_DIR}/src/" "${BUNDLE_DIR}/src/"
fi

CONTENTS=$(cd "${BUNDLE_DIR}" && find . -type f | sort | jq -R -s 'split("\n") | map(select(length > 0))')
cat > "${BUNDLE_DIR}/bundle-info.json" <<EOF
{
  "wallet_type": "specter",
  "version": "${BUNDLE_VERSION}",
  "platform": "${PLATFORM}",
  "build_date": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "contents": ${CONTENTS}
}
EOF

tar czf "${WORK_DIR}/hwwtui-specter-${PLATFORM}.tar.gz" -C "${WORK_DIR}" "hwwtui-specter-${PLATFORM}"
echo "==> Done: hwwtui-specter-${PLATFORM}.tar.gz"
