# AetherionOS Build Guide

## Prerequisites

### Rust Toolchain
```bash
# Install exact nightly version
rustup install nightly-2023-08-01
rustup default nightly-2023-08-01

# Required components
rustup component add rust-src
rustup component add llvm-tools-preview
rustup target add x86_64-unknown-none

# Build tools
cargo install bootimage
```

### System Packages (Ubuntu/Debian)
```bash
sudo apt-get install gcc nasm mtools qemu-system-x86
```

### Toolchain Versions (Pinned)
| Tool | Version | Notes |
|------|---------|-------|
| Rust | nightly-2023-08-01 | Pinned in `kernel/rust-toolchain.toml` |
| bootimage | 0.10.3 | Legacy; migration to bootloader v0.11 planned |
| QEMU | 6.0+ | For testing with `-serial stdio` |
| GCC | 9+ | For C userspace apps |

## Quick Build

```bash
# Full build (userspace + kernel)
bash scripts/full_build.sh

# Kernel only (uses stubs for missing userspace binaries)
bash scripts/full_build.sh --kernel

# Check compilation only (fastest)
bash scripts/full_build.sh --kernel --check
```

## Build Architecture

### Directory Layout
```
AetherionOS/
  kernel/                    # Kernel crate (bare-metal, no_std)
    src/main.rs              # Entry point, includes ~48 ELF binaries via include_bytes!
    Cargo.toml               # Dependencies: bootloader 0.9.23, x86_64, etc.
    rust-toolchain.toml      # Pins nightly-2023-08-01
  userspace/
    c_apps/                  # C apps (cat, ls, sh, wget, etc.)
    agent_*/                 # Rust agents (each is a separate crate)
    rust_sdk/                # Shared Rust SDK for userspace agents
    hello.elf, shell.elf     # Pre-built or stub ELF binaries
    busybox.elf              # BusyBox binary (external)
  bin_cache/                 # Compiled agent binaries for kernel embedding
  sdk/c/                     # C SDK (aetherion.h, crt0, libc stubs)
  x86_64-aetherion.json      # Kernel target spec
  x86_64-aetherion-user.json # Userspace target spec
  scripts/
    full_build.sh            # Main build script
    build_c.sh               # C apps build
    build_all_agents.sh      # Rust agents build
```

### Build Order
1. **C SDK** (`sdk/c/`) -> `libaetherion.a`
2. **C apps** (`userspace/c_apps/`) -> `*.elf` (linked against C SDK)
3. **Rust agents** (`userspace/agent_*/`) -> `bin_cache/` and `target/` binaries
4. **Kernel** (`kernel/`) -> `bootimage-aetherion-kernel.bin`

The kernel embeds all userspace binaries via `include_bytes!` macros in `main.rs`.
This means **all userspace binaries must exist before the kernel can compile**.

## Anti-Corruption Measures

The sandbox environment can corrupt files with NUL bytes during parallel builds.
Apply these mitigations:

```bash
# 1. Use tmpfs for kernel build target (prevents disk corruption)
sudo mkdir -p /mnt/aetherion-build
sudo mount -t tmpfs -o size=3g,mode=1777 tmpfs /mnt/aetherion-build
cd kernel && rm -rf target && ln -s /mnt/aetherion-build target

# 2. Pre-fetch and lock Cargo registry
cd kernel && CARGO_BUILD_JOBS=1 cargo fetch
chmod -R a-w ~/.cargo/registry/src/

# 3. Always use single-threaded builds
export CARGO_BUILD_JOBS=1

# 4. Harden git against index corruption
cd /path/to/repo
git config core.fsync objects,derived-metadata,reference
git config core.preloadIndex false
```

## Testing with QEMU

```bash
# Basic boot test (60s timeout)
timeout 60 qemu-system-x86_64 \
  -enable-kvm -cpu host -smp 2 -m 1024M \
  -drive format=raw,file=kernel/target/x86_64-aetherion/release/bootimage-aetherion-kernel.bin \
  -display none -serial stdio -no-reboot

# With disk image (for FAT32 filesystem)
timeout 60 qemu-system-x86_64 \
  -enable-kvm -cpu host -smp 2 -m 1024M \
  -drive format=raw,file=kernel/target/x86_64-aetherion/release/bootimage-aetherion-kernel.bin \
  -drive format=raw,file=disk.img,if=virtio \
  -display none -serial stdio -no-reboot
```

### Expected Boot Output
- `[BOOT] AetherionOS Couche 19 READY`
- BusyBox shell on `/dev/ttyS0` (serial console)
- `agent_visual_term` launched for graphical output

### What to Check
- No `PANIC` or `SIGSEGV` in output
- `execve` syscall logs showing binary loading
- `ld-musl` dynamic linker messages for `hello_dyn.elf`
- BusyBox interactive shell prompt

## Known Technical Debt

1. **bootimage crate** (0.10.3): Obsolete, unmaintained. Migration to `bootloader` v0.11 planned but requires entry point changes.
2. **Nightly pinning** (2023-08-01): 2.5 years behind upstream. Update requires auditing all `asm!` blocks and `core::intrinsics` usage.
3. **include_bytes! embedding**: ~48 binaries embedded in kernel image. Should migrate to initramfs/VFS for smaller kernel and faster builds.
4. **Stub binaries**: `full_build.sh` creates placeholder ELFs for agents that fail to compile. These are valid ELFs that call `exit(0)` but provide no functionality.
