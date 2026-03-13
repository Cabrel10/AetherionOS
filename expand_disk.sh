#!/bin/bash
# Expand disk.img to accommodate Mistral model

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

DISK_IMG="disk.img"
NEW_SIZE_MB=8192  # 8 GB

echo "========================================="
echo "Expanding disk.img to ${NEW_SIZE_MB}MB"
echo "========================================="
echo ""

# Backup current disk
echo "[1/5] Creating backup..."
cp "$DISK_IMG" disk_backup_$(date +%Y%m%d_%H%M%S).img
echo "✓ Backup created"
echo ""

# Create new larger disk
echo "[2/5] Creating new ${NEW_SIZE_MB}MB disk..."
dd if=/dev/zero of=disk_new.img bs=1M count=$NEW_SIZE_MB status=progress
mkfs.fat -F 32 -n "AETHERION" disk_new.img
echo "✓ New disk created"
echo ""

# Mount both disks
echo "[3/5] Mounting disks..."
mkdir -p /tmp/fat_old /tmp/fat_new
sudo mount -o loop "$DISK_IMG" /tmp/fat_old
sudo mount -o loop disk_new.img /tmp/fat_new
echo "✓ Disks mounted"
echo ""

# Copy all files
echo "[4/5] Copying files..."
sudo cp -rv /tmp/fat_old/* /tmp/fat_new/ 2>/dev/null || true
echo "✓ Files copied"
echo ""

# Unmount and replace
echo "[5/5] Finalizing..."
sudo umount /tmp/fat_old /tmp/fat_new
mv "$DISK_IMG" disk_old.img
mv disk_new.img "$DISK_IMG"
echo "✓ disk.img replaced (old saved as disk_old.img)"
echo ""

# Show new disk info
echo "========================================="
echo "New disk info:"
echo "========================================="
ls -lh "$DISK_IMG"
mdir -i "$DISK_IMG" ::/
echo ""
echo "SUCCESS! Disk expanded to ${NEW_SIZE_MB}MB"
