#!/bin/bash

REPO_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$REPO_DIR"

# Fonction build rapide (kernel seulement)
build_kernel() {
  echo "=== Building kernel ==="
  cd kernel
  CARGO_BUILD_JOBS=4 cargo bootimage --release 2>&1 | tail -5
  cd ..
}

# Fonction build agent
build_agent() {
  local agent=$1
  echo "=== Building $agent ==="
  cd userspace/$agent
  cargo build --release \
    --target ../../x86_64-aetherion-user.json \
    -Zbuild-std=core,alloc \
    -Zbuild-std-features=compiler-builtins-mem 2>&1 | tail -3
  cd ../..
}

# Fonction test rapide
test_quick() {
  local BOOTIMG=$(ls -t kernel/target/x86_64-*/release/bootimage-*.bin | head -1)
  echo "=== Testing with $BOOTIMG ==="
  timeout 60 qemu-system-x86_64 \
    -drive format=raw,file=$BOOTIMG \
    -drive file=disk.img,format=raw,if=none,id=disk0 \
    -device virtio-blk-pci,drive=disk0 \
    -m 256M -display none -serial stdio \
    -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
    -no-reboot 2>/dev/null | tee /tmp/aetherion_last.log
}

# Fonction test avec GUI
test_gui() {
  local BOOTIMG=$(ls -t kernel/target/x86_64-*/release/bootimage-*.bin | head -1)
  echo "=== Launching GUI with $BOOTIMG ==="
  qemu-system-x86_64 \
    -drive format=raw,file=$BOOTIMG \
    -drive file=disk.img,format=raw,if=none,id=disk0 \
    -device virtio-blk-pci,drive=disk0 \
    -m 512M \
    -display gtk,zoom-to-fit=on \
    -serial file:/tmp/aetherion_serial.log \
    -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
    -no-reboot &
  sleep 2
  tail -f /tmp/aetherion_serial.log
}

# Exécuter selon l'argument
case "$1" in
  build)   build_kernel ;;
  agent)   build_agent $2 ;;
  test)    test_quick ;;
  gui)     test_gui ;;
  all)     build_kernel && test_quick ;;
  *)
    echo "Usage: $0 {build|agent <name>|test|gui|all}"
    echo "Exemples:"
    echo "  $0 all              # Build + test"
    echo "  $0 agent agent_gguf # Build un agent"
    echo "  $0 gui              # Test avec interface"
    ;;
esac
