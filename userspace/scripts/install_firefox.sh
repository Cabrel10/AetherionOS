#!/bin/sh
# AetherionOS Firefox Installation via Flatpak
# Requires: network access, TLS support, APK package manager
#
# This script handles the full chain:
# 1. Install flatpak via apk
# 2. Configure flathub remote
# 3. Install Firefox
# 4. Create a wrapper that bypasses sandbox restrictions

set -e

echo "=== AetherionOS Firefox Installer ==="
echo ""

# Step 1: Install prerequisites
echo "[1/4] Installing flatpak and dependencies..."
apk update
apk add flatpak xdg-desktop-portal bubblewrap 2>/dev/null || {
    echo "WARNING: Some dependencies may not be available"
    apk add flatpak 2>/dev/null || {
        echo "ERROR: flatpak not available in APK repos"
        echo "Trying manual installation..."
        # Fallback: download flatpak binary
        wget -q "https://flathub.org/repo/appstream/org.mozilla.firefox.flatpakref" \
            -O /tmp/firefox.flatpakref 2>/dev/null || true
    }
}

# Step 2: Add Flathub remote
echo "[2/4] Configuring Flathub repository..."
flatpak remote-add --if-not-exists flathub https://flathub.org/repo/flathub.flatpakrepo

# Step 3: Install Firefox
echo "[3/4] Installing Firefox (this may take a while)..."
flatpak install -y flathub org.mozilla.firefox

# Step 4: Create wrapper script
echo "[4/4] Creating Firefox launcher..."
cat > /usr/bin/firefox << 'EOF'
#!/bin/sh
# Firefox launcher for AetherionOS
# Uses --no-sandbox if bubblewrap namespaces aren't available

if flatpak run --command=echo org.mozilla.firefox "test" 2>/dev/null; then
    exec flatpak run org.mozilla.firefox "$@"
else
    echo "Running Firefox with relaxed sandbox..."
    exec flatpak run --no-sandbox org.mozilla.firefox "$@"
fi
EOF
chmod +x /usr/bin/firefox

echo ""
echo "=== Firefox installed successfully ==="
echo "Launch with: firefox"
echo "Or: flatpak run org.mozilla.firefox"
