# Changelog

All notable changes to AetherionOS will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased] - Jalon 146

### Fixed
- **ELF loader**: Resolved null pointer dereference in `sys_execve` when `AT_BASE` is zero
  (`kernel/src/elf/mod.rs`).
- **IDT panic handler**: Fixed `{{:X}}` format escape in PF-NULLPTR panic message
  (`kernel/src/arch/x86_64/idt.rs:1163`).
- **FAT32 filesystem**: Added iteration limit (5,000,000) to prevent infinite loop on
  corrupted directory entries (`kernel/src/fs/fat32.rs`).
- **Syscall validation**: Added `validate_user_ptr` and `read_user_string` guards to
  prevent kernel-mode access to invalid user pointers (`kernel/src/arch/x86_64/syscall.rs`).

### Added
- **Build script**: `scripts/full_build.sh` - single entry point for full reproducible
  builds (C apps, Rust agents, kernel bootimage).
- **Build documentation**: `BUILD.md` with toolchain versions, prerequisites, directory
  layout, and anti-corruption measures.
- **CI workflow**: `.github/workflows/build.yml` with cargo check, bootimage build, and
  C apps build jobs.
- **PR template**: `.github/pull_request_template.md` with testing checklist and risk
  assessment.
- **Changelog**: This file.

### Changed
- **Boot order** (Jalon 146): BusyBox serial console (STEP B0) now launches before
  agent_visual_term (STEP B) for faster interactive access.
- **Naked IRETQ**: `_start` function uses `#[naked]` with CR3/RIP/RSP debug print.

### Known Issues
- ~30 compiler warnings remain (unused imports, unnecessary unsafe blocks).
- ~40 userspace agents use stub ELF placeholders (call `exit(0)` only).
- `bootimage` crate (0.10.3) is deprecated; migration to `bootloader` v0.11 pending.
- Nightly toolchain pinned to 2023-08-01 (2.5+ years old).
