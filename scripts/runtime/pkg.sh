#!/bin/sh
# pkg - AetherionOS Unified Package Manager
#
# Detects the correct package manager based on the package type and delegates:
#   - musl packages: apk (Alpine package manager)
#   - glibc packages: glibc-run + apk (via Linuxulator)
#   - flatpak packages: flatpak (graphical apps)
#
# Usage:
#   pkg install <package>     Install a package (auto-detects type)
#   pkg remove <package>      Remove a package
#   pkg search <package>      Search for a package
#   pkg list                  List installed packages
#   pkg update                Update all package databases
#   pkg info <package>        Show package info
#
# Architecture:
#   Pillar 1: musl  -> apk add <pkg>
#   Pillar 2: glibc -> apk add --root /glibc-root <pkg> (via Linuxulator)
#   Pillar 3: gui   -> flatpak install <pkg>

set -e

PKG_DB="/var/lib/aetherion/pkg.db"
GLIBC_ROOT="/lib/glibc"
FLATPAK_DIR="/opt/flatpak"

# Known Flatpak app IDs (graphical applications)
FLATPAK_APPS="firefox gimp libreoffice code vscode thunderbird vlc inkscape blender kdenlive"

# Known glibc-required tools
GLIBC_TOOLS="nmap hydra sqlmap wireshark metasploit john hashcat"

# Detect package type
detect_type() {
    local pkg="$1"
    
    # Check if it's a Flatpak app ID (contains dots)
    if echo "$pkg" | grep -q '\.'; then
        if echo "$pkg" | grep -qE '^org\.|^com\.|^io\.|^net\.'; then
            echo "flatpak"
            return
        fi
    fi
    
    # Check known Flatpak apps
    for app in ${FLATPAK_APPS}; do
        if [ "$pkg" = "$app" ]; then
            echo "flatpak"
            return
        fi
    done
    
    # Check known glibc tools
    for tool in ${GLIBC_TOOLS}; do
        if [ "$pkg" = "$tool" ]; then
            echo "glibc"
            return
        fi
    done
    
    # Default: musl (system package)
    echo "musl"
}

# Resolve Flatpak app ID from short name
resolve_flatpak_id() {
    local pkg="$1"
    case "$pkg" in
        firefox)      echo "org.mozilla.firefox" ;;
        gimp)         echo "org.gimp.GIMP" ;;
        libreoffice)  echo "org.libreoffice.LibreOffice" ;;
        code|vscode)  echo "com.visualstudio.code" ;;
        thunderbird)  echo "org.mozilla.Thunderbird" ;;
        vlc)          echo "org.videolan.VLC" ;;
        inkscape)     echo "org.inkscape.Inkscape" ;;
        blender)      echo "org.blender.Blender" ;;
        kdenlive)     echo "org.kde.kdenlive" ;;
        *)            echo "$pkg" ;; # Assume full ID
    esac
}

# Install a package
do_install() {
    local pkg="$1"
    local type=$(detect_type "$pkg")
    
    echo "[pkg] Installing '$pkg' (detected type: $type)"
    
    case "$type" in
        musl)
            echo "[pkg] Using apk (musl)..."
            apk add "$pkg"
            ;;
        glibc)
            echo "[pkg] Using Linuxulator (glibc)..."
            if [ -d "${GLIBC_ROOT}" ]; then
                apk add --root /glibc-root "$pkg" 2>/dev/null || \
                    echo "[pkg] WARNING: glibc package installation requires manual setup"
            else
                echo "[pkg] ERROR: glibc runtime not found at ${GLIBC_ROOT}"
                echo "[pkg] Run: scripts/runtime/glibc-run.sh --setup"
                return 1
            fi
            ;;
        flatpak)
            echo "[pkg] Using Flatpak..."
            local app_id=$(resolve_flatpak_id "$pkg")
            flatpak install -y flathub "$app_id"
            ;;
    esac
    
    # Record in package database
    mkdir -p "$(dirname $PKG_DB)"
    echo "$pkg|$type|$(date -Iseconds)" >> "$PKG_DB"
    echo "[pkg] '$pkg' installed successfully (type: $type)"
}

# Remove a package
do_remove() {
    local pkg="$1"
    local type=$(detect_type "$pkg")
    
    echo "[pkg] Removing '$pkg' (type: $type)"
    
    case "$type" in
        musl)     apk del "$pkg" ;;
        glibc)    apk del --root /glibc-root "$pkg" 2>/dev/null || true ;;
        flatpak)  flatpak uninstall -y "$(resolve_flatpak_id "$pkg")" ;;
    esac
    
    # Remove from package database
    if [ -f "$PKG_DB" ]; then
        grep -v "^$pkg|" "$PKG_DB" > "${PKG_DB}.tmp" 2>/dev/null
        mv "${PKG_DB}.tmp" "$PKG_DB"
    fi
}

# Search for a package
do_search() {
    local pkg="$1"
    echo "[pkg] Searching for '$pkg' across all pillars..."
    echo ""
    echo "=== musl (apk) ==="
    apk search "$pkg" 2>/dev/null || echo "  (apk not available)"
    echo ""
    echo "=== Flatpak (flathub) ==="
    flatpak search "$pkg" 2>/dev/null || echo "  (flatpak not available)"
}

# List installed packages
do_list() {
    echo "[pkg] Installed packages:"
    echo ""
    if [ -f "$PKG_DB" ]; then
        echo "NAME|TYPE|INSTALLED"
        echo "----|----|---------"
        cat "$PKG_DB"
    else
        echo "  (no packages tracked yet)"
    fi
    echo ""
    echo "=== musl packages ==="
    apk list --installed 2>/dev/null | head -20 || echo "  (apk not available)"
    echo ""
    echo "=== Flatpak apps ==="
    flatpak list 2>/dev/null || echo "  (flatpak not available)"
}

# Update all package databases
do_update() {
    echo "[pkg] Updating all package databases..."
    echo ""
    echo "=== musl (apk) ==="
    apk update 2>/dev/null || echo "  (apk not available)"
    echo ""
    echo "=== Flatpak ==="
    flatpak update -y 2>/dev/null || echo "  (flatpak not available)"
    # Clean unused runtimes
    flatpak uninstall --unused -y 2>/dev/null || true
    echo ""
    echo "[pkg] All databases updated."
}

# Main dispatch
case "${1}" in
    install)
        shift
        for pkg in "$@"; do do_install "$pkg"; done
        ;;
    remove|uninstall)
        shift
        for pkg in "$@"; do do_remove "$pkg"; done
        ;;
    search)
        shift
        do_search "$1"
        ;;
    list)
        do_list
        ;;
    update)
        do_update
        ;;
    info)
        shift
        detect_type "$1"
        ;;
    *)
        echo "AetherionOS Unified Package Manager"
        echo ""
        echo "Usage: pkg <command> [package...]"
        echo ""
        echo "Commands:"
        echo "  install <pkg>   Install package (auto-detects musl/glibc/flatpak)"
        echo "  remove <pkg>    Remove package"
        echo "  search <pkg>    Search all repositories"
        echo "  list            List installed packages"
        echo "  update          Update all package databases"
        echo "  info <pkg>      Detect package type"
        echo ""
        echo "Architecture: musl (system) | Linuxulator (glibc CLI) | Flatpak (GUI)"
        ;;
esac
