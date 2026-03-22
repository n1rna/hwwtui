#!/usr/bin/env bash
# Build and package the Ledger (Speculos) emulator bundle.
# Requires Docker.
#
# Expected environment:
#   BUNDLE_VERSION  – version string for bundle-info.json (default: "dev")
#   FIRMWARE_DIR    – path to checked-out app-bitcoin-new repo (default: "app-bitcoin-new")
#
# Produces: hwwtui-ledger-linux-x86_64.tar.gz in the current directory.
set -euo pipefail

BUNDLE_VERSION="${BUNDLE_VERSION:-dev}"
FIRMWARE_DIR="$(cd "${FIRMWARE_DIR:-app-bitcoin-new}" && pwd)"
WORK_DIR="$(pwd)"
PLATFORM="linux-x86_64"
BUNDLE_DIR="${WORK_DIR}/hwwtui-ledger-${PLATFORM}"

echo "==> Building Ledger Bitcoin app from ${FIRMWARE_DIR}"

# Build the app ELF using the Ledger builder Docker image.
docker run --rm \
    -v "${FIRMWARE_DIR}:/app" \
    -w /app \
    ghcr.io/ledgerhq/ledger-app-builder/ledger-app-builder:latest \
    make BOLOS_SDK=/opt/nanosplus-secure-sdk

# Find the ELF.
ELF=$(find "${FIRMWARE_DIR}/bin" -name '*.elf' -o -name 'app.elf' 2>/dev/null | head -1)
if [ -z "${ELF}" ]; then
    ELF=$(find "${FIRMWARE_DIR}/build" -name '*.elf' 2>/dev/null | head -1)
fi
if [ -z "${ELF}" ]; then
    echo "ERROR: Bitcoin app ELF not found"
    find "${FIRMWARE_DIR}" -name '*.elf' | head -10
    exit 1
fi
echo "==> Found app ELF: ${ELF}"

echo "==> Packaging bundle: ${BUNDLE_DIR}"
cd "${WORK_DIR}"
rm -rf "${BUNDLE_DIR}"
mkdir -p "${BUNDLE_DIR}"

cp "${ELF}" "${BUNDLE_DIR}/bitcoin-app.elf"

# Launcher script for Speculos.
cat > "${BUNDLE_DIR}/speculos-runner.sh" <<'RUNNER'
#!/usr/bin/env bash
# Launch Ledger Speculos emulator with the bundled Bitcoin app.
# Requires Docker.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

APDU_PORT="${APDU_PORT:-9999}"
API_PORT="${API_PORT:-5001}"
MODEL="${MODEL:-nanosp}"  # nanosp = Nano S Plus

echo "Starting Speculos (model=${MODEL}, apdu_port=${APDU_PORT}, api_port=${API_PORT})..."
docker run --rm \
    -v "${SCRIPT_DIR}/bitcoin-app.elf:/app/bitcoin.elf:ro" \
    -p "${APDU_PORT}:9999" \
    -p "${API_PORT}:5000" \
    ghcr.io/ledgerhq/speculos:latest \
    /app/bitcoin.elf \
    --model "${MODEL}" \
    --display headless \
    --seed "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"
RUNNER
chmod +x "${BUNDLE_DIR}/speculos-runner.sh"

# docker-compose.yml for convenience.
cat > "${BUNDLE_DIR}/docker-compose.yml" <<'COMPOSE'
version: "3.8"
services:
  speculos:
    image: ghcr.io/ledgerhq/speculos:latest
    ports:
      - "9999:9999"
      - "5000:5000"
    volumes:
      - ./bitcoin-app.elf:/app/bitcoin.elf:ro
    command: >
      /app/bitcoin.elf
      --model nanos2
      --display headless
      --seed "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"
COMPOSE

CONTENTS=$(cd "${BUNDLE_DIR}" && find . -type f | sort | jq -R -s 'split("\n") | map(select(length > 0))')
cat > "${BUNDLE_DIR}/bundle-info.json" <<EOF
{
  "wallet_type": "ledger",
  "version": "${BUNDLE_VERSION}",
  "platform": "${PLATFORM}",
  "build_date": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "contents": ${CONTENTS}
}
EOF

tar czf "${WORK_DIR}/hwwtui-ledger-${PLATFORM}.tar.gz" -C "${WORK_DIR}" "hwwtui-ledger-${PLATFORM}"
echo "==> Done: hwwtui-ledger-${PLATFORM}.tar.gz"
