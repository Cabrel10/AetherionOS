#!/bin/bash
# Split Mistral model for FAT32 (4GB limit) and copy to disk.img

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

MISTRAL_PATH="/mnt/new_data/m0/models/mistral-7b-instruct-v0.3.Q4_K_M.gguf"
DISK_IMG="disk.img"
PART_SIZE="2000M"  # 2GB parts (safe for FAT32)

echo "========================================="
echo "Split & Copy Mistral to disk.img"
echo "========================================="
echo ""

# Check if model exists
if [ ! -f "$MISTRAL_PATH" ]; then
    echo "ERROR: Model not found at $MISTRAL_PATH"
    exit 1
fi

echo "[1/4] Model info:"
ls -lh "$MISTRAL_PATH"
echo ""

# Create temp directory
TEMP_DIR="/tmp/mistral_parts"
rm -rf "$TEMP_DIR"
mkdir -p "$TEMP_DIR"

echo "[2/4] Splitting model into ${PART_SIZE} parts..."
cd "$TEMP_DIR"
split -b "$PART_SIZE" -d "$MISTRAL_PATH" part
echo "✓ Model split complete"
ls -lh part*
echo ""

echo "[3/4] Creating /models/ directory on disk..."
mmd -i "$DISK_IMG" ::/models 2>/dev/null || echo "  (already exists)"
echo ""

echo "[4/4] Copying parts to disk.img..."
cd "$SCRIPT_DIR"
for part in "$TEMP_DIR"/part*; do
    partname=$(basename "$part")
    echo "  Copying $partname..."
    mcopy -o -i "$DISK_IMG" "$part" "::/models/$partname"
done
echo "✓ All parts copied"
echo ""

echo "========================================="
echo "Verification:"
echo "========================================="
mdir -i "$DISK_IMG" ::/models/
echo ""

# Cleanup
rm -rf "$TEMP_DIR"
echo "✓ Temp files cleaned"
echo ""
echo "SUCCESS! Model ready on disk.img"
