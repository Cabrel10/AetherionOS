# Aetherion OS - ISO Creation Guide

**Version**: 1.0.0  
**Date**: 2025-12-13

## Prerequisites

```bash
sudo apt install -y grub-pc-bin grub-common xorriso
```

## Build Steps

### 1. Create ISO Directory Structure

```bash
mkdir -p iso/boot/grub
```

### 2. Copy Kernel Binary

```bash
# Build kernel first
./scripts/build.sh

# Extract kernel binary
objcopy -O binary kernel/aetherion-kernel iso/boot/aetherion-kernel.bin
```

### 3. Create GRUB Configuration

```bash
cat > iso/boot/grub/grub.cfg << 'EOF'
set timeout=5
set default=0

menuentry "Aetherion OS v1.0.0" {
    multiboot /boot/aetherion-kernel.bin
    boot
}

menuentry "Aetherion OS v1.0.0 (Safe Mode)" {
    multiboot /boot/aetherion-kernel.bin nosmp noapic
    boot
}
EOF
```

### 4. Generate ISO Image

```bash
grub-mkrescue -o aetherion-os.iso iso/
```

## Test ISO in QEMU

```bash
qemu-system-x86_64 -cdrom aetherion-os.iso -m 256M
```

## Test ISO in VirtualBox

1. Create new VM (Type: Other, Version: Other/Unknown 64-bit)
2. Allocate 256 MB RAM minimum
3. Attach `aetherion-os.iso` as CD-ROM
4. Boot VM

## ISO Details

- **Size**: ~5-10 MB
- **Format**: ISO 9660 with El Torito boot
- **Bootloader**: GRUB 2
- **Architecture**: x86_64

## Expected Boot Output

```
AETHERION OS v1.0.0 - FINAL
============================
Kernel loaded successfully!
All Phases: COMPLETE
Status: INITIALIZING...

GDT: INITIALIZED
IDT: INITIALIZED
Syscalls: INITIALIZED
Processes & IPC: INITIALIZED
Drivers: INITIALIZED
Filesystem: INITIALIZED
Networking: INITIALIZED

Frame Allocator: INITIALIZED
Heap Allocator: INITIALIZED

Status: OPERATIONAL
System ready. Press Reset to reboot.
```

## Download Pre-built ISO

**GitHub Releases**: https://github.com/choe73/AetherionOS/releases

Download `aetherion-os-v1.0.0.iso` and boot directly!

---

**ISO Created Successfully! 💿**
