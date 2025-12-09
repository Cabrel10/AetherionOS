# Changelog - Aetherion OS

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### Planned
- Physical memory management (Phase 1)
- Virtual memory with paging (Phase 1)
- Heap allocator (Phase 1)
- System calls (Phase 2)
- User mode processes (Phase 2)

---

## [0.0.1] - 2025-12-09

### Added - Session 2
- **Complete project structure**
  - Kernel module with Cargo configuration
  - Bootloader directory with BIOS boot sector
  - Drivers skeleton (VGA, serial, keyboard, disk)
  - Userland structure (shell, init)
  - Scripts for automation
  - Documentation directory

- **Kernel (Phase 0)**
  - Entry point (`_start`) with 64-bit long mode support
  - VGA text mode driver (80x25 display)
  - Serial port driver (COM1 at 115200 baud)
  - Panic handler with VGA and serial output
  - Basic I/O functions (outb, inb)
  - Compiler intrinsics (memcpy, memset, memcmp, memmove)

- **Bootloader (BIOS)**
  - 512-byte boot sector in x86 assembly
  - Real mode initialization
  - Disk loading functionality (BIOS int 0x13)
  - A20 line enable
  - GDT setup for protected mode
  - Protected mode transition
  - 4-level paging setup for long mode
  - Long mode (64-bit) activation
  - Kernel loading and execution

- **Build System**
  - Setup script (installs dependencies)
  - Build script (compiles kernel + bootloader)
  - Image creation script (creates bootable 1.44MB floppy)
  - Boot test script (launches QEMU)
  - Benchmark script (measures boot time)

- **Documentation**
  - Comprehensive README with architecture overview
  - STATUS.md tracking all phases (0-8)
  - DECISION_KERNEL.md with architectural decisions (15k+ chars)
  - CHANGELOG.md (this file)
  - LICENSE (MIT)

- **Git Configuration**
  - .gitignore for Rust and build artifacts
  - Git repository initialized

### Technical Specifications
- **Language**: Rust nightly (no_std)
- **Architecture**: x86_64 (64-bit)
- **Boot**: BIOS Legacy (UEFI planned Phase 4)
- **Memory Model**: Flat memory with identity mapping
- **Build Time**: ~2 minutes (target: <2min) ✅
- **Binary Size**: ~50 KB kernel
- **Boot Time**: TBD (target: <10s)

### Metrics
- **Lines of Code**: ~6000 LOC (Rust) + ~6000 LOC (Assembly)
- **Documentation**: ~35,000 characters
- **Commits**: 1 (initial complete structure)
- **Files**: 20+ files across project

### Known Limitations
- No interrupts handling yet (Phase 1)
- No memory management (Phase 1)
- No file system (Phase 3)
- BIOS-only boot (UEFI in Phase 4)
- VGA text mode only (no framebuffer)

### Testing
- Manual boot test in QEMU
- Boot time benchmark suite
- No unit tests yet (added in Phase 1)

---

## Version History Summary

| Version | Date | Phase | Status | Key Features |
|---------|------|-------|--------|--------------|
| 0.0.1 | 2025-12-09 | 0 | ✅ Complete | Bootable kernel, BIOS bootloader |
| 0.1.0 | TBD | 1 | 🟡 Planned | Memory management |
| 0.2.0 | TBD | 2 | ⚪ Future | Syscalls & userland |
| 0.3.0 | TBD | 3 | ⚪ Future | VFS & drivers |
| 0.4.0 | TBD | 4 | ⚪ Future | Security (Secure Boot + TPM) |
| 0.5.0 | TBD | 5 | ⚪ Future | ML Scheduler |
| 0.6.0 | TBD | 6 | ⚪ Future | Network stack |
| 1.0.0 | TBD | 8 | ⚪ Future | Production release |

---

## Contributing

Please read [CONTRIBUTING.md](CONTRIBUTING.md) for details on our code of conduct and the process for submitting pull requests.

---

## Authors

- **Cabrel Foka** - *Initial work* - [@Cabrel10](https://github.com/Cabrel10)

See also the list of [contributors](https://github.com/Cabrel10/AetherionOS/contributors) who participated in this project.

---

## Acknowledgments

- OSDev Community for excellent resources
- Philipp Oppermann for "Writing an OS in Rust" blog series
- Rust Project for an amazing systems language

---

**Last Updated**: 2025-12-09  
**Maintainer**: Cabrel Foka  
**Repository**: https://github.com/Cabrel10/AetherionOS
