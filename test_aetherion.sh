#!/bin/bash
set -e

REPO_DIR="$(cd "$(dirname "$0")" && pwd)"
BOOTIMG="$REPO_DIR/kernel/target/x86_64-unknown-none/release/bootimage-aetherion-kernel.bin"
LOG="/tmp/aetherion_test_$(date +%s).log"

echo "=== AetherionOS Local Test ==="
echo "Bootimage: $BOOTIMG"
echo "Log: $LOG"

if [ ! -f "$BOOTIMG" ]; then
  echo "ERROR: Bootimage not found. Run build first."
  exit 1
fi

# Lancer QEMU avec timeout
timeout 120 qemu-system-x86_64 \
  -drive format=raw,file=$BOOTIMG \
  -drive file=$REPO_DIR/disk.img,format=raw,if=none,id=disk0 \
  -device virtio-blk-pci,drive=disk0 \
  -m 256M \
  -display none \
  -serial stdio \
  -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
  -no-reboot \
  2>/dev/null > $LOG
EXIT=$?

echo ""
echo "=== RÉSULTATS ==="
echo "Exit code: $EXIT (124=timeout=OK, 33=success exit)"

# Vérifier les jalons
for jalon in J33 J34 J35 J36 J37 J38 J39 J40; do
  if strings $LOG | grep -q "${jalon}-OK"; then
    echo "✅ $jalon : VALIDÉ"
  elif strings $LOG | grep -q "${jalon}"; then
    echo "⚠️  $jalon : PARTIEL"
  fi
done

# Vérifier les panics
if strings $LOG | grep -q "PANIC\|page fault\|triple fault"; then
  echo "❌ CRASH DÉTECTÉ"
  strings $LOG | grep -E "PANIC|fault" | tail -5
else
  echo "✅ Pas de crash"
fi

echo ""
echo "Log complet: $LOG"
echo "Commande: strings $LOG | grep -E 'OK|FAIL|PANIC'"
