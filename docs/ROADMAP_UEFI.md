# AetherionOS UEFI Boot Roadmap

**Status**: Planning phase (no code changes)  
**Target Jalon**: 150+  
**Author**: MORNINGSTAR  
**Date**: 2026-04-21  

## Context

AetherionOS currently boots via legacy BIOS through QEMU (`-drive format=raw`).
The kernel uses `bootloader_api = "0.11"` for the boot protocol, which supports
both BIOS and UEFI. This document outlines the roadmap for adding UEFI boot
support to enable booting on modern hardware.

## Why UEFI

- **Standard on 99% of machines post-2010**: UEFI is the de facto firmware interface
- **GOP (Graphics Output Protocol)**: Native framebuffer without VGA text mode
- **ACPI tables**: Passed directly by firmware, no manual EBDA/RSDP scanning
- **No 16-bit real mode**: Firmware hands off in 64-bit long mode directly
- **Secure Boot**: Future-proof with signed kernel images
- **GPT partitioning**: Modern disk layout support

## Current Architecture (BIOS)

```
+-------------------+
|   BIOS Firmware   |  <-- QEMU legacy BIOS
|   (Real Mode)     |
+-------------------+
|  bootloader 0.11  |  <-- Rust bootloader crate (BIOS stages)
|  (16->32->64 bit) |
+-------------------+
|  AetherionOS      |  <-- kernel_main(boot_info: &'static mut BootInfo)
|  Kernel           |
+-------------------+
```

## Target Architecture (UEFI)

```
+-------------------+
|   UEFI Firmware   |  <-- QEMU + OVMF.fd or real hardware
|   (Boot Services) |
+-------------------+
|  bootloader 0.11  |  <-- Same crate, UEFI mode (already supported!)
|  (UEFI app .efi)  |
+-------------------+
|  ExitBootServices |  <-- Transition to bare-metal
+-------------------+
|  AetherionOS      |  <-- Same kernel_main, identical BootInfo
|  Kernel           |
+-------------------+
```

## Key Advantage

Because AetherionOS already uses `bootloader_api = "0.11"`, the kernel code is
**boot-protocol agnostic**. The `BootInfo` structure is identical whether booting
via BIOS or UEFI. The only changes needed are:

1. Build infrastructure (producing a UEFI disk image)
2. QEMU launch command (adding OVMF firmware)
3. Framebuffer handling (GOP vs VGA text mode)

## Migration Plan

### Phase 1: UEFI Boot Image Generation (Jalon 150)

**Goal**: Produce a UEFI-bootable disk image alongside the existing BIOS image.

**Tasks**:
- Add `features = ["bios", "uefi"]` to boot/Cargo.toml's bootloader dependency
- Add `ovmf-prebuilt` dependency for QEMU UEFI firmware
- Update `boot/build.rs` to produce both `bios.img` and `uefi.img`
- Update `boot/src/main.rs` to accept `bios` or `uefi` CLI argument
- Test with: `qemu-system-x86_64 -drive if=pflash,format=raw,file=OVMF_CODE.fd`

**Estimated effort**: 2-4 hours  
**Risk**: Low (bootloader crate already supports UEFI natively)

### Phase 2: Framebuffer Adaptation (Jalon 151)

**Goal**: Support GOP framebuffer in addition to VGA text mode.

**Tasks**:
- Add framebuffer detection in kernel init:
  ```rust
  if let Optional::Some(fb) = &boot_info.framebuffer {
      // Use GOP framebuffer (pixel-based)
      framebuffer::init_gop(fb);
  } else {
      // Fallback to VGA text mode (BIOS only)
      framebuffer::init_vga();
  }
  ```
- Adapt font rendering (PSF/bitmap font for pixel framebuffer)
- Update the Window Manager (agent_wm) for pixel-based output
- Update the Terminal (agent_terminal) for pixel-based text
- Preserve serial output as primary debug channel

**Estimated effort**: 8-16 hours  
**Risk**: Medium (affects visual output pipeline)

### Phase 3: ACPI/RSDP from UEFI (Jalon 152)

**Goal**: Use UEFI-provided RSDP instead of manual scanning.

**Tasks**:
- Check `boot_info.rsdp_addr` (provided by UEFI firmware)
- If available, skip EBDA/BIOS scanning in `arch/x86_64/acpi.rs`
- Parse ACPI tables for APIC configuration
- Enable IOAPIC-based interrupt routing (replacing legacy PIC 8259)

**Estimated effort**: 4-8 hours  
**Risk**: Low (additive, doesn't break existing BIOS path)

### Phase 4: Hardware Validation (Jalon 153+)

**Goal**: Boot on real UEFI hardware.

**Tasks**:
- Create USB bootable image with GPT partitioning
- Test on Intel/AMD hardware with Secure Boot disabled
- Profile boot time and memory usage
- Document hardware compatibility matrix

**Estimated effort**: Variable  
**Risk**: Medium-High (hardware-specific issues)

## Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| `bootloader` | 0.11.x | UEFI disk image creation (already in boot/) |
| `bootloader_api` | 0.11.x | Kernel-side types (already in kernel/) |
| `ovmf-prebuilt` | 0.2.x | QEMU UEFI firmware for testing |

## Risks and Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| GOP framebuffer != VGA text mode | High | Dual-mode driver with runtime detection |
| UEFI memory map differences | Medium | Already handled by bootloader_api abstraction |
| Secure Boot PE signature requirement | Low (future) | Defer to Phase 5+ |
| OVMF build complexity | Low | Use `ovmf-prebuilt` crate (pre-compiled) |
| Boot Services memory consumption | Medium | Adjust frame allocator reservations |

## Preserved Components (No Changes Required)

The following kernel subsystems are **boot-protocol agnostic** and require
no modifications for UEFI support:

- Cognitive Bus (IPC) — J56
- Linuxulator (Linux ABI compat) — J125-J135
- LLM Inference pipeline — J50-J66
- Process Manager (Matriarchal) — J100+
- Priority Scheduler with Aging — J77+
- Security hardening (TPM, W^X, KPTI) — J134+
- ELF64 Loader with per-process paging — J72+
- POSIX syscalls (open/read/write/fork/exec) — J90+

## QEMU Commands

### Current (BIOS):
```bash
qemu-system-x86_64 \
    -drive format=raw,file=target/aetherion-boot.img \
    -serial stdio -m 4G -smp 2
```

### Future (UEFI):
```bash
qemu-system-x86_64 \
    -drive format=raw,file=target/aetherion-uefi.img \
    -drive if=pflash,format=raw,unit=0,file=OVMF_CODE.fd,readonly=on \
    -drive if=pflash,format=raw,unit=1,file=OVMF_VARS.fd,snapshot=on \
    -serial stdio -m 4G -smp 2
```

## Timeline

| Phase | Jalon | Status | ETA |
|-------|-------|--------|-----|
| Phase 1: UEFI Image | 150 | Planned | After Front 3 (Limine) |
| Phase 2: Framebuffer | 151 | Planned | +1 sprint |
| Phase 3: ACPI/RSDP | 152 | Planned | +1 sprint |
| Phase 4: Hardware | 153+ | Planned | +2 sprints |

## References

- [bootloader crate documentation](https://docs.rs/bootloader/0.11)
- [bootloader_api crate documentation](https://docs.rs/bootloader_api/0.11)
- [UEFI Specification](https://uefi.org/specifications)
- [OSDev Wiki: UEFI](https://wiki.osdev.org/UEFI)
- [Rust UEFI Book](https://rust-osdev.github.io/uefi-rs/)
