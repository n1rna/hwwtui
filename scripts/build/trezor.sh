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

# Install Python deps. Trezor uses poetry, but we just need the build deps
# available globally so that make's python subprocesses can find them.
# Install the local trezor python package + key deps.
pip3 install --break-system-packages \
    click protobuf scons mako munch "setuptools>=60" \
    -e "${FIRMWARE_DIR}/python" \
    2>/dev/null \
|| pip3 install --user \
    click protobuf scons mako munch "setuptools>=60" \
    -e "${FIRMWARE_DIR}/python"

# Ensure `python` points to python3 (Ubuntu 24.04 only ships python3).
if ! command -v python &>/dev/null; then
    sudo ln -sf "$(command -v python3)" /usr/local/bin/python 2>/dev/null \
    || ln -sf "$(command -v python3)" /usr/local/bin/python 2>/dev/null \
    || true
fi

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

# Bundle all non-glibc shared libraries so the emulator is self-contained.
echo "==> Collecting shared libraries"
mkdir -p "${BUNDLE_DIR}/lib"
ldd "${BINARY}" | awk '/=>/ {print $3}' | while read -r lib; do
    # Skip glibc/ld/vdso — these must come from the host
    case "$(basename "$lib")" in
        libc.so*|libm.so*|libdl.so*|librt.so*|libpthread.so*|ld-linux*|libgcc_s.so*|libstdc++.so*) continue ;;
    esac
    cp -L "$lib" "${BUNDLE_DIR}/lib/" 2>/dev/null || true
done

# Create a wrapper script that sets LD_LIBRARY_PATH
cat > "${BUNDLE_DIR}/run.sh" <<'WRAPPER'
#!/usr/bin/env bash
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
export LD_LIBRARY_PATH="${SCRIPT_DIR}/lib:${LD_LIBRARY_PATH:-}"
export SDL_VIDEODRIVER="${SDL_VIDEODRIVER:-offscreen}"
exec "${SCRIPT_DIR}/trezor-emu-core" "$@"
WRAPPER
chmod +x "${BUNDLE_DIR}/run.sh"

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
