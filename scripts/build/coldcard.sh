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

# GCC 13+ on Ubuntu 24.04 is stricter with -Werror; globally remove it
# from all micropython Makefiles so the old code compiles.
echo "==> Patching micropython to remove -Werror for GCC 13+ compatibility"
find "${FIRMWARE_DIR}/external/micropython" \( -name 'Makefile' -o -name '*.mk' \) -print | \
    xargs sed -i 's/-Werror//g' 2>/dev/null || true

# Build mpy-cross first (micropython's bytecode compiler, needed for frozen modules).
echo "==> Building mpy-cross"
cd "${FIRMWARE_DIR}/external/micropython/mpy-cross"
make

cd "${FIRMWARE_DIR}/unix"

make setup || true  # setup's 'tools' target may fail on frozen_content but creates needed symlinks
make ngu-setup

# Patch the simulator version string BEFORE compilation.
# The unix simulator hardcodes '5.x.x' in variant/version.py which gets
# frozen into the binary.  We patch it to match the firmware tag.
FW_TAG=$(cd "${FIRMWARE_DIR}" && git describe --tags --always 2>/dev/null || echo "unknown")
FW_VERSION=$(echo "${FW_TAG}" | grep -oP 'v\K[0-9]+\.[0-9]+\.[0-9]+[A-Z]*' || echo "6.0.0")
FW_DATE=$(echo "${FW_TAG}" | grep -oP '^\d{4}-\d{2}-\d{2}' || date -u +%Y-%m-%d)
echo "==> Patching variant/version.py: ${FW_VERSION} (${FW_DATE})"
sed -i "s|return '2023-02-31', '5.x.x', '230231195308'|return '${FW_DATE}', '${FW_VERSION}', '$(date -u +%y%m%d%H%M%S)'|" \
    "${FIRMWARE_DIR}/unix/variant/version.py"

make

# Locate the Coldcard-patched micropython binary.
# The Coldcard build produces "coldcard-mpy" (not generic "micropython").
# It may be at unix/coldcard-mpy, unix/l-port/micropython, or
# external/micropython/ports/unix/coldcard-mpy depending on version.
MP_BIN=$(find "${FIRMWARE_DIR}" -maxdepth 5 -type f -name 'coldcard-mpy' ! -path '*/build-*' | head -1)
if [ -z "${MP_BIN}" ]; then
    # Fallback: look for the micropython binary in the unix port directory
    MP_BIN=$(find "${FIRMWARE_DIR}/external/micropython/ports/unix" -maxdepth 1 -type f -name 'micropython' | head -1)
fi
if [ -z "${MP_BIN}" ]; then
    echo "ERROR: Coldcard micropython binary not found"
    echo "Searched for 'coldcard-mpy' and 'micropython' in ${FIRMWARE_DIR}"
    find "${FIRMWARE_DIR}" -type f -executable -name '*micropython*' -o -name 'coldcard-mpy' 2>/dev/null | head -20
    exit 1
fi
echo "==> Found Coldcard micropython binary: ${MP_BIN}"

# Verify it has the 'pyb' module (Coldcard-patched build)
if ! echo "import pyb" | "${MP_BIN}" 2>&1 | grep -q "pyb"; then
    echo "==> Binary has Coldcard patches (pyb module available)"
else
    echo "WARNING: Binary may be generic micropython (missing pyb module)"
fi

echo "==> Packaging bundle: ${BUNDLE_DIR}"
cd "${WORK_DIR}"
rm -rf "${BUNDLE_DIR}"
mkdir -p "${BUNDLE_DIR}"

# Name it "micropython" for consistency with BundleManager discovery.
cp "${MP_BIN}" "${BUNDLE_DIR}/micropython"
chmod +x "${BUNDLE_DIR}/micropython"

# Runtime data: shared/ has the Python modules the simulator imports.
# Use -L to follow symlinks (many modules are symlinked from external/).
rsync -aL \
    --exclude='__pycache__' \
    --exclude='*.pyc' \
    "${FIRMWARE_DIR}/shared/" "${BUNDLE_DIR}/shared/"

rsync -aL \
    --exclude='__pycache__' \
    --exclude='*.pyc' \
    --exclude='*.o' \
    --exclude='l-port' \
    --exclude='l-mpy' \
    --exclude='coldcard-mpy' \
    "${FIRMWARE_DIR}/unix/" "${BUNDLE_DIR}/unix/" || true

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
