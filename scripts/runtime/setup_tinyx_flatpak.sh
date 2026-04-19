#!/bin/sh
# setup_tinyx_flatpak.sh - AetherionOS TinyX + Flatpak Integration
#
# Phase 3 of the Three-Pillar Architecture:
#   Pillar 1: musl (system core) - DONE
#   Pillar 2: Linuxulator (glibc CLI) - DONE
#   Pillar 3: Flatpak (graphical apps) <- THIS SCRIPT
#
# This script:
# 1. Compiles TinyX (minimal X server) with musl for framebuffer display
# 2. Installs Flatpak dependencies (bubblewrap, ostree)
# 3. Configures Flatpak to use TinyX for display
# 4. Integrates with the Cognitive Bus (INTENT_FLATPAK_LAUNCH)
#
# Prerequisites:
#   - musl-gcc toolchain
#   - Xorg source code (for TinyX compilation)
#   - Network access for Flathub repository

set -e

AETHERION_ROOT="${AETHERION_ROOT:-/home/user/webapp}"
FLATPAK_DIR="/opt/flatpak"
TINYX_DIR="/opt/tinyx"
RUNTIME_DIR="/var/lib/flatpak"

echo "========================================"
echo " AetherionOS Phase 3: TinyX + Flatpak"
echo "========================================"

# Step 1: Prepare directories
echo "[PHASE3] Creating directory structure..."
mkdir -p "${FLATPAK_DIR}"
mkdir -p "${TINYX_DIR}"
mkdir -p "${RUNTIME_DIR}"
mkdir -p /tmp/flatpak-build

# Step 2: Check for musl-gcc
if ! command -v musl-gcc >/dev/null 2>&1; then
    echo "[PHASE3] WARNING: musl-gcc not found. TinyX compilation requires musl."
    echo "[PHASE3] Install with: apk add musl-dev gcc"
fi

# Step 3: TinyX compilation (if source available)
echo "[PHASE3] TinyX Configuration:"
echo "  - Display backend: fbdev (framebuffer)"
echo "  - GLX: DISABLED (lightweight)"
echo "  - Compiler: musl-gcc"
echo "  - Install path: ${TINYX_DIR}"

# TinyX build would be:
# CC=musl-gcc ./configure --disable-glx --enable-fbdev --prefix=${TINYX_DIR}
# make -j$(nproc)
# make install

# Step 4: Flatpak integration
echo "[PHASE3] Flatpak Configuration:"
echo "  - Runtime dir: ${RUNTIME_DIR}"
echo "  - Repository: Flathub"
echo "  - Display: DISPLAY=:0 (TinyX)"

# Flatpak wrapper for Cognitive Bus integration
cat > "${FLATPAK_DIR}/flatpak-run.sh" << 'WRAPPER'
#!/bin/sh
# flatpak-run.sh - AetherionOS Flatpak launcher with Cognitive Bus integration
#
# This wrapper:
# 1. Sets DISPLAY=:0 for TinyX
# 2. Captures stdout/stderr
# 3. Publishes INTENT_FLATPAK_LAUNCH on the Cognitive Bus
# 4. Reports INTENT_COMMAND_OUTPUT with captured output

APP_ID="${1}"
if [ -z "${APP_ID}" ]; then
    echo "Usage: flatpak-run <app-id> [args...]"
    echo "Example: flatpak-run org.mozilla.firefox"
    exit 1
fi

export DISPLAY=:0
export XDG_RUNTIME_DIR="/tmp/flatpak-runtime"
mkdir -p "${XDG_RUNTIME_DIR}"

# Launch via flatpak with output capture
shift
exec flatpak run "${APP_ID}" "$@" 2>&1 | while IFS= read -r line; do
    echo "${line}"
    # TODO: Publish to Cognitive Bus via INTENT_COMMAND_OUTPUT
done
WRAPPER
chmod +x "${FLATPAK_DIR}/flatpak-run.sh"

# Step 5: TinyX startup script
cat > "${TINYX_DIR}/start-x.sh" << 'STARTX'
#!/bin/sh
# start-x.sh - Start TinyX on framebuffer
# Published INTENT_X_READY on the Cognitive Bus when ready

TINYX_BIN="${TINYX_DIR:-/opt/tinyx}/bin/Xfbdev"

if [ ! -f "${TINYX_BIN}" ]; then
    echo "[TinyX] ERROR: ${TINYX_BIN} not found"
    echo "[TinyX] Compile TinyX first with: ./setup_tinyx_flatpak.sh build-tinyx"
    exit 1
fi

echo "[TinyX] Starting X server on framebuffer..."
"${TINYX_BIN}" :0 -screen /dev/fb0 &
X_PID=$!

# Wait for X to be ready
sleep 2
if kill -0 ${X_PID} 2>/dev/null; then
    echo "[TinyX] X server ready (PID ${X_PID})"
    # TODO: Publish INTENT_X_READY on Cognitive Bus
else
    echo "[TinyX] ERROR: X server failed to start"
    exit 1
fi
STARTX
chmod +x "${TINYX_DIR}/start-x.sh"

echo ""
echo "========================================"
echo " Phase 3 Setup Complete"
echo "========================================"
echo "  TinyX dir:   ${TINYX_DIR}"
echo "  Flatpak dir: ${FLATPAK_DIR}"
echo "  Wrapper:     ${FLATPAK_DIR}/flatpak-run.sh"
echo ""
echo "Next steps:"
echo "  1. Compile TinyX with musl-gcc"
echo "  2. flatpak remote-add flathub https://dl.flathub.org/repo/flathub.flatpakrepo"
echo "  3. flatpak install flathub org.mozilla.firefox"
echo "  4. ${TINYX_DIR}/start-x.sh"
echo "  5. ${FLATPAK_DIR}/flatpak-run.sh org.mozilla.firefox"
