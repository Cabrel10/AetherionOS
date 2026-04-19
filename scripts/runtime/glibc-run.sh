#!/bin/sh
# glibc-run.sh - AetherionOS glibc runtime wrapper
#
# Purpose: Run glibc-compiled binaries via the Linuxulator by setting up
# the correct dynamic linker and library paths. This allows running tools
# like nmap, python3, git, curl, etc. that are compiled against glibc.
#
# Usage: glibc-run <command> [args...]
#
# Architecture:
#   Pillar 1: musl (system core)
#   Pillar 2: Linuxulator (glibc CLI tools) <- THIS WRAPPER
#   Pillar 3: Flatpak (graphical apps)
#
# The glibc runtime is expected at /lib/glibc/ with:
#   /lib/glibc/ld-linux-x86-64.so.2  (dynamic linker)
#   /lib/glibc/libc.so.6             (C library)
#   /lib/glibc/libm.so.6             (math library)
#   /lib/glibc/libpthread.so.0       (threading)
#   /lib/glibc/libdl.so.2            (dynamic loading)
#   /lib/glibc/librt.so.1            (realtime)

GLIBC_DIR="/lib/glibc"
GLIBC_LD="${GLIBC_DIR}/ld-linux-x86-64.so.2"

if [ $# -eq 0 ]; then
    echo "Usage: glibc-run <command> [args...]"
    echo "Runs a glibc-compiled binary via the AetherionOS Linuxulator."
    exit 1
fi

# Check if glibc runtime is installed
if [ ! -f "${GLIBC_LD}" ]; then
    echo "[glibc-run] ERROR: glibc runtime not found at ${GLIBC_DIR}"
    echo "[glibc-run] Install it with: pkg install glibc-runtime"
    echo "[glibc-run] Or extract from a Debian rootfs:"
    echo "    mkdir -p ${GLIBC_DIR}"
    echo "    cp /path/to/debian/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2 ${GLIBC_DIR}/"
    echo "    cp /path/to/debian/lib/x86_64-linux-gnu/libc.so.6 ${GLIBC_DIR}/"
    exit 2
fi

# Set up the environment for glibc binary execution
export LD_LIBRARY_PATH="${GLIBC_DIR}:${LD_LIBRARY_PATH}"

# Execute the binary via the glibc dynamic linker
exec "${GLIBC_LD}" --library-path "${GLIBC_DIR}" "$@"
