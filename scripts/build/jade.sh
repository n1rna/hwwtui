#!/usr/bin/env bash
# Build and package the Jade QEMU emulator bundle.
# Requires Docker.
#
# Expected environment:
#   BUNDLE_VERSION  – version string for bundle-info.json (default: "dev")
#   FIRMWARE_DIR    – path to checked-out Jade firmware repo (default: "jade-firmware")
#
# Produces: hwwtui-jade-linux-x86_64.tar.gz in the current directory.
set -euo pipefail

BUNDLE_VERSION="${BUNDLE_VERSION:-dev}"
FIRMWARE_DIR="$(cd "${FIRMWARE_DIR:-jade-firmware}" && pwd)"
WORK_DIR="$(pwd)"
PLATFORM="linux-x86_64"
BUNDLE_DIR="${WORK_DIR}/hwwtui-jade-${PLATFORM}"

echo "==> Building Jade QEMU Docker image from ${FIRMWARE_DIR}"

cd "${FIRMWARE_DIR}"
docker build -t jade-qemu -f Dockerfile.qemu .

echo "==> Exporting Docker image"
cd "${WORK_DIR}"
docker save jade-qemu | gzip > jade-qemu-image.tar.gz

echo "==> Packaging bundle: ${BUNDLE_DIR}"
rm -rf "${BUNDLE_DIR}"
mkdir -p "${BUNDLE_DIR}"

mv jade-qemu-image.tar.gz "${BUNDLE_DIR}/"

# Launcher script.
cat > "${BUNDLE_DIR}/jade-runner.sh" <<'RUNNER'
#!/usr/bin/env bash
# Launch Jade QEMU emulator from the bundled Docker image.
# Requires Docker.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

SERIAL_PORT="${SERIAL_PORT:-30121}"
IMAGE_NAME="jade-qemu"

# Load the image if not already present.
if ! docker image inspect "${IMAGE_NAME}" &>/dev/null; then
    echo "Loading Jade QEMU Docker image..."
    docker load -i "${SCRIPT_DIR}/jade-qemu-image.tar.gz"
fi

echo "Starting Jade QEMU emulator (serial_port=${SERIAL_PORT})..."
docker run --rm \
    -p "${SERIAL_PORT}:30121" \
    "${IMAGE_NAME}"
RUNNER
chmod +x "${BUNDLE_DIR}/jade-runner.sh"

CONTENTS=$(cd "${BUNDLE_DIR}" && find . -type f | sort | jq -R -s 'split("\n") | map(select(length > 0))')
cat > "${BUNDLE_DIR}/bundle-info.json" <<EOF
{
  "wallet_type": "jade",
  "version": "${BUNDLE_VERSION}",
  "platform": "${PLATFORM}",
  "build_date": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "contents": ${CONTENTS}
}
EOF

tar czf "${WORK_DIR}/hwwtui-jade-${PLATFORM}.tar.gz" -C "${WORK_DIR}" "hwwtui-jade-${PLATFORM}"
echo "==> Done: hwwtui-jade-${PLATFORM}.tar.gz"
