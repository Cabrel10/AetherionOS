# AetherionOS Build Guide

## Prerequisites

### Rust Toolchain
```bash
# Install exact nightly version (pinned in rust-toolchain.toml)
rustup install nightly-2026-04-21
rustup default nightly-2026-04-21

# Required components
rustup component add rust-src
rustup component add llvm-tools-preview
rustup component add rustfmt
rustup target add x86_64-unknown-none
```

### System Packages (Ubuntu/Debian)
```bash
sudo apt-get install gcc nasm mtools qemu-system-x86 xorriso
```

### Toolchain Versions (Pinned)
| Tool | Version | Notes |
|------|---------|-------|
| Rust | nightly-2026-04-21 | Pinned in `rust-toolchain.toml` |
| bootloader_api | 0.11 | Kernel-side boot protocol types |
| bootloader | 0.11 | Boot image builder (needs >= 2 GB RAM) |
| limine | 0.6.3 | Alternative boot protocol (optional) |
| QEMU | 6.0+ | For testing with `-serial stdio` |
| GCC | 9+ | For C userspace apps |
| xorriso | any | For Limine ISO creation (optional) |

## Quick Build

### Kernel Only (Fastest)
```bash
# Check compilation (no binary output)
cargo check -p aetherion-kernel --lib

# Build kernel ELF
cargo build -p aetherion-kernel --target x86_64-unknown-none

# Build kernel (resource-constrained machines)
CARGO_BUILD_JOBS=1 cargo build -p aetherion-kernel --target x86_64-unknown-none
```

### Boot Image (BIOS via bootloader 0.11)
```bash
# Requires >= 2 GB RAM
bash scripts/build-boot-011.sh

# Or manually:
CARGO_BUILD_JOBS=1 cargo build -p aetherion-boot
```

### Limine ISO (Alternative boot)
```bash
# Switch to Limine branch
git checkout migrate/limine

# Build ISO (requires xorriso + Limine binaries)
bash scripts/build-limine.sh
```

## Build Architecture

### Directory Layout
```
AetherionOS/
  Cargo.toml                  # Workspace root (resolver = "2")
  .cargo/config.toml          # bindeps = true
  rust-toolchain.toml         # nightly-2026-04-21
  kernel/                     # Kernel crate (bare-metal, no_std)
    Cargo.toml                # bootloader_api 0.11, limine 0.6 (optional)
    .cargo/config.toml        # target = x86_64-unknown-none, build-std
    src/
      main.rs                 # Entry point (bootloader_api::entry_point!)
      lib.rs                  # Library interface for tests
      boot/                   # Boot protocol abstraction
        mod.rs                # Feature-gated boot modules
        limine_entry.rs       # Limine entry point (--features limine)
      arch/x86_64/            # Architecture (GDT, IDT, APIC, syscalls)
      memory/                 # Memory management (frames, paging, heap)
      process/                # Process manager + scheduler
      fs/                     # VFS + FAT32
      net/                    # TCP/IP, DNS, HTTP
      elf/                    # ELF64 loader with per-process paging
      ipc/                    # Cognitive Bus (lock-free MPMC)
      drivers/                # PS/2, USB xHCI, etc.
      security/               # TPM, KPTI, stack protector
      compat/                 # Linuxulator (Linux ABI)
    linker-x86_64.ld          # Limine linker script
  boot/                       # Boot runner crate (host-side)
    Cargo.toml                # bootloader 0.11 + kernel artifact dep
    build.rs                  # Creates BIOS disk image
    src/main.rs               # QEMU launcher
  userspace/                  # Userspace binaries
    c_apps/                   # C apps (cat, ls, sh, wget, etc.)
    agent_*/                  # Rust agents
    hello.elf, shell.elf      # Pre-built ELF binaries
  bin_cache/                  # Compiled agent binaries
  third_party/
    limine/                   # Pre-built Limine binaries (optional)
  scripts/
    build-boot-011.sh         # Full boot image build
    build-kernel-only.sh      # Kernel ELF only
    build-limine.sh           # Limine ISO build
  docs/
    ROADMAP_UEFI.md           # UEFI boot roadmap
  MIGRATION_LOG.md            # Jalon 148 migration details
```

### Build Order
1. **C SDK** (`sdk/c/`) -> `libaetherion.a`
2. **C apps** (`userspace/c_apps/`) -> `*.elf` (linked against C SDK)
3. **Rust agents** (`userspace/agent_*/`) -> `bin_cache/` binaries
4. **Kernel** (`kernel/`) -> kernel ELF (embeds all userspace via `include_bytes!`)
5. **Boot image** (`boot/`) -> `target/aetherion-boot.img` or ISO

The kernel embeds all userspace binaries via `include_bytes!` macros in `main.rs`.
This means **all userspace binaries must exist before the kernel binary can compile**.
(`cargo check --lib` works without them since lib.rs doesn't include the binaries.)

## Boot Protocols

### 1. Bootloader 0.11 (Default)
```
Firmware (BIOS/UEFI) -> bootloader 0.11 -> kernel_main(BootInfo)
```
- Entry: `bootloader_api::entry_point!(kernel_main)`
- Config: `BootloaderConfig { Mapping::Dynamic, 128 KiB stack }`
- Build: `cargo build -p aetherion-boot`

### 2. Limine (Feature Flag)
```
Firmware (BIOS/UEFI) -> Limine -> kmain() [reads HHDM/Memmap/FB/RSDP]
```
- Entry: `#[no_mangle] unsafe extern "C" fn kmain()`
- Config: `limine.conf` in ISO root
- Build: `bash scripts/build-limine.sh` on `migrate/limine` branch
- Linker: `kernel/linker-x86_64.ld` (base 0xffffffff80000000)

### 3. UEFI (Planned — Jalon 150+)
See `docs/ROADMAP_UEFI.md` for details.

## Testing with QEMU

### Bootloader 0.11 (BIOS)
```bash
qemu-system-x86_64 \
    -drive format=raw,file=target/aetherion-boot.img \
    -serial stdio -m 4G -smp 2 -no-reboot
```

### Limine (BIOS ISO)
```bash
qemu-system-x86_64 \
    -cdrom target/aetherion-limine.iso \
    -serial stdio -m 4G -smp 2 -no-reboot
```

### Expected Boot Output
- Banner: `AetherionOS Kernel Boot Sequence`
- GDT, IDT, PIC initialization logs
- Memory manager initialization
- Shell prompt: `$`

### Validation Checklist
- [ ] No `PANIC` or `SIGSEGV` in output
- [ ] Shell prompt `$` appears on serial
- [ ] `exec /bin/hello.elf` prints test message
- [ ] `cargo check --lib`: 0 errors, 0 warnings

## Anti-Corruption Measures

The sandbox environment can corrupt files during parallel builds.
Apply these mitigations:

```bash
# 1. Always use single-threaded builds on constrained machines
export CARGO_BUILD_JOBS=1

# 2. Pre-fetch and lock Cargo registry
cargo fetch
chmod -R a-w ~/.cargo/registry/src/

# 3. Harden git
git config core.fsync objects,derived-metadata,reference
git config core.preloadIndex false
```

## Known Technical Debt

1. **include_bytes! embedding**: ~48 binaries embedded in kernel. Should migrate to initramfs.
2. **Stub binaries**: Placeholder ELFs in sandbox for missing userspace apps.
3. **Bootloader 0.11 RAM**: Disk image creation needs >= 2 GB RAM.
4. **Limine memory init**: Full memory manager adaptation for Limine pending.
5. **1 unused-import warning**: In `process/mod.rs` (non-critical).
