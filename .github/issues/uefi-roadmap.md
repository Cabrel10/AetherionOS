# UEFI Boot Support Roadmap

## Summary
Add UEFI boot support to AetherionOS alongside existing BIOS and Limine boot paths.

## Status
**Planned** - See `docs/ROADMAP_UEFI.md` for detailed plan.

## Context
AetherionOS Jalon 148 migrated the kernel to `bootloader_api = "0.11"`, which already supports UEFI natively. The kernel code is boot-protocol agnostic - the same `BootInfo` structure is used regardless of firmware type.

## Phases

### Phase 1: UEFI Boot Image Generation (Jalon 150)
- [ ] Add `features = ["bios", "uefi"]` to boot/Cargo.toml
- [ ] Add `ovmf-prebuilt` dependency for QEMU testing
- [ ] Update `boot/build.rs` to produce `uefi.img`
- [ ] Update `boot/src/main.rs` for `uefi` CLI argument
- [ ] Add QEMU UEFI launch command to build scripts

### Phase 2: Framebuffer (Jalon 151)
- [ ] Detect GOP framebuffer from `boot_info.framebuffer`
- [ ] Add pixel-based font rendering (PSF/bitmap)
- [ ] Dual-mode driver: GOP (UEFI) / VGA text mode (BIOS)

### Phase 3: ACPI/RSDP (Jalon 152)
- [ ] Use UEFI-provided RSDP (`boot_info.rsdp_addr`)
- [ ] Skip EBDA scanning when RSDP is available
- [ ] Enable IOAPIC routing (replace legacy PIC 8259)

### Phase 4: Hardware Validation (Jalon 153+)
- [ ] Create USB bootable GPT image
- [ ] Test on real Intel/AMD UEFI hardware
- [ ] Document hardware compatibility matrix

## Prerequisites
- [x] bootloader_api 0.11 migration (Jalon 148, Front 2)
- [x] Kernel compiles for x86_64-unknown-none (Jalon 148, Front 1)
- [ ] bootloader 0.11 full build tested on machine with >= 2 GB RAM

## Labels
`enhancement`, `boot`, `uefi`, `roadmap`

## References
- `docs/ROADMAP_UEFI.md` - Detailed technical roadmap
- [bootloader 0.11 docs](https://docs.rs/bootloader/0.11)
- [UEFI Specification](https://uefi.org/specifications)
