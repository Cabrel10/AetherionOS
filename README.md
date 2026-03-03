# 🌌 Aetherion OS

**A Next-Generation Operating System written in Rust**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust Version](https://img.shields.io/badge/rust-nightly-orange.svg)](https://www.rust-lang.org)
[![Architecture](https://img.shields.io/badge/arch-x86__64-green.svg)](https://en.wikipedia.org/wiki/X86-64)
[![Status](https://img.shields.io/badge/status-alpha-yellow.svg)](STATUS.md)

---

## 🎯 Vision

Aetherion OS est un système d'exploitation expérimental visant à repousser les limites de la sécurité, de la performance et de l'architecture système moderne. Conçu entièrement en Rust, il combine les avantages d'un microkernel modulaire avec la puissance du machine learning pour l'ordonnancement et la sécurité prédictive.

### 🌟 Caractéristiques Uniques

- **🔒 Sécurité Proactive** : Secure Boot, TPM 2.0, détection ML d'anomalies
- **⚡ Performance Optimale** : Boot <10s, ordonnanceur ML adaptatif, ASLR avancé
- **🧩 Architecture Hybride** : Microkernel + drivers en espace noyau pour performance
- **🌐 Réseau Moderne** : Stack TCP/IP, virtio, HTTP/3 natif
- **🔬 ML Intégré** : Ordonnanceur prédictif, détection d'intrusions, optimisation ressources

---

## 📋 Table des Matières

- [Architecture](#architecture)
- [Phases de Développement](#phases-de-développement)
- [Installation](#installation)
- [Compilation](#compilation)
- [Tests](#tests)
- [Documentation Technique](#documentation-technique)
- [Contribution](#contribution)
- [Roadmap](#roadmap)
- [License](#license)

---

## 🏗️ Architecture

### Vue d'Ensemble

```
┌─────────────────────────────────────────────────────┐
│                  USERLAND (Ring 3)                  │
├─────────────────────────────────────────────────────┤
│  Applications  │  Shell  │  System Utils  │  IPC   │
├─────────────────────────────────────────────────────┤
│              System Call Interface                  │
├─────────────────────────────────────────────────────┤
│                 KERNEL (Ring 0)                     │
├──────────────┬──────────────┬──────────────────────┤
│   Scheduler  │   Memory     │    VFS & Drivers    │
│   (ML Core)  │   Manager    │    (virtio/ATA)     │
├──────────────┴──────────────┴──────────────────────┤
│           Security Layer (ASLR/Secure Boot)        │
├─────────────────────────────────────────────────────┤
│              Hardware Abstraction Layer             │
└─────────────────────────────────────────────────────┘
```

### Composants Principaux

1. **Kernel Core** (`kernel/`)
   - Scheduler ML-based
   - Memory Manager (Physical + Virtual)
   - Interrupt Handling (IDT/GDT)
   - System Call Interface

2. **Drivers** (`drivers/`)
   - VGA Text Mode
   - Serial Port (COM1)
   - Keyboard (PS/2)
   - Disk (ATA/SATA)
   - Network (virtio-net)

3. **Userland** (`userland/`)
   - Init process
   - Shell interactif
   - Utilitaires système

4. **Security** (intégré)
   - Secure Boot + TPM
   - ASLR kernel-space
   - ML Anomaly Detection
   - Capability-based security

---

## 🚀 Phases de Développement

| Phase | Nom | Durée | Status | Détails |
|-------|-----|-------|--------|---------|
| **0** | Fondations | 1 sem | COMPLETE | Kernel minimal bootable |
| **1** | HAL (Couche 1) | 1 sem | COMPLETE | GDT/IDT/PIC/Security |
| **2** | Memory (Couche 2) | 1 sem | COMPLETE | Frame alloc/Paging/Heap (8 MiB) |
| **3** | Cognitive Bus (Couche 3) | 1 sem | COMPLETE | Lock-free MPMC IPC |
| **4** | VFS (Couche 4) | 1 sem | COMPLETE | Virtual Filesystem + Security |
| **5** | Verifier (Couche 5) | 1 sem | COMPLETE | Policy engine + Syscall filtering |
| **6** | Process Manager (Couche 6) | 1 sem | COMPLETE | Matriarchal hierarchy + Ring 3 |
| **7** | Scheduler (Couche 7) | 1 sem | COMPLETE | Priority scheduler + aging |
| **8** | GPU Stub (Couche 8) | 1 sem | COMPLETE | PCI GPU detection + VRAM alloc |
| **9** | Syscalls + Context Switch (Couche 9) | 1 sem | COMPLETE | SYSCALL/SYSRET + MSR config |
| **11** | ELF Loader (Couche 11) | 1 sem | COMPLETE | ELF64 loader + per-process paging |
| **13** | POSIX Syscalls (Couche 13) | 1 sem | COMPLETE | Full POSIX table + FD management |
| **16** | C Userspace (Couche 16) | 1 sem | COMPLETE | libc_stub + hello_c.elf |
| **17** | Network Stack (Couche 17) | 2 sem | COMPLETE | Ethernet/ARP/IPv4/UDP/TCP/DNS |
| **18** | HTTP Client (Couche 18) | 1 sem | COMPLETE | wget.elf (DNS + TCP + HTTP) |
| **19** | Storage Layer (Couche 19) | 2 sem | COMPLETE | VirtIO-Block + FAT32 + ls/cat/j19_test |

**Durée Totale** : ~19 couches en 16 semaines

### Couche 1 HAL - COMPLETE

| Composant | Fichier | Status |
|-----------|---------|--------|
| **GDT** | `arch/x86_64/gdt.rs` | DONE |
| **IDT** | `arch/x86_64/idt.rs` | DONE |
| **PIC** | `arch/x86_64/interrupts.rs` | DONE |
| **Security** | `security/mod.rs` | DONE |

### Couche 2 Memory - COMPLETE

| Composant | Fichier | Status |
|-----------|---------|--------|
| **Frame Allocator** | `memory/frame.rs` | DONE |
| **Paging** | `memory/paging.rs` | DONE |
| **Heap** | `memory/heap.rs` | DONE |

### Couche 3 Cognitive Bus - COMPLETE

| Composant | Fichier | Status |
|-----------|---------|--------|
| **Bus** | `ipc/bus.rs` | DONE |
| **IntentMessage** | `ipc/mod.rs` | DONE |

### Couche 4 VFS - COMPLETE

| Composant | Fichier | Status |
|-----------|---------|--------|
| **VFS Core** | `fs/vfs.rs` | DONE |
| **Manifests** | `fs/manifest.rs` | DONE |
| **Path Security** | `fs/vfs.rs` | DONE |
| **Metrics** | `fs/vfs.rs` | DONE |

**Build Metrics:** 0 errors, 0 warnings, 21+ tests passing

---

## 💻 Installation

### Prerequis

- **Rust** : nightly-2023-08-01 (strict version)
- **bootimage** : 0.10.3 (strict version)
- **QEMU** : x86_64 system emulator (qemu-system-x86)
- **Build Tools** : gcc, nasm, mtools, ld
- **Git** : pour cloner le repo

### Installation Automatique

```bash
# Cloner le repository
git clone https://github.com/Cabrel10/AetherionOS.git
cd AetherionOS

# Installer les dépendances
./scripts/setup.sh

# Compiler et tester
./scripts/build.sh
./scripts/boot-test.sh
```

### Installation Manuelle

```bash
# Installer Rust nightly
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup default nightly
rustup component add rust-src llvm-tools-preview

# Installer QEMU
sudo apt install qemu-system-x86 nasm

# Ajouter target bare-metal
rustup target add x86_64-unknown-none
```

---

## 🔨 Compilation

### Build du Kernel

```bash
cd kernel
cargo build --target x86_64-unknown-none --release
```

### Build du Bootloader

```bash
cd bootloader
nasm -f bin src/boot.asm -o boot.bin
```

### Créer l'Image Bootable

```bash
./scripts/create-image.sh
# Génère: aetherion.img (1.44 MB floppy image)
```

---

## 🧪 Tests

### Tests Unitaires

```bash
# Tests kernel
cd kernel
cargo test --lib

# Tests drivers
cd drivers
cargo test
```

### Tests d'Intégration

```bash
# Boot test dans QEMU
./scripts/boot-test.sh

# Tests réseau (Phase 6+)
./scripts/test-network.sh
```

### Benchmarks

```bash
# Benchmark boot time
./scripts/benchmark-boot.sh

# Benchmark memory allocator
./scripts/benchmark-memory.sh
```

---

## 📚 Documentation Technique

### Documents Clés

- [STATUS.md](STATUS.md) - État d'avancement détaillé
- [DECISION_KERNEL.md](docs/DECISION_KERNEL.md) - Choix architecturaux
- [MEMORY_LAYOUT.md](docs/MEMORY_LAYOUT.md) - Organisation mémoire
- [SYSCALL_API.md](docs/SYSCALL_API.md) - Interface système
- [SECURITY.md](docs/SECURITY.md) - Modèle de sécurité
- [CHANGELOG.md](CHANGELOG.md) - Historique des versions

### API Documentation

```bash
# Générer la doc Rust
cd kernel
cargo doc --open
```

---

## 🤝 Contribution

Les contributions sont bienvenues ! Veuillez suivre ces étapes :

1. **Fork** le projet
2. Créer une branche feature (`git checkout -b feature/AmazingFeature`)
3. Commit vos changements (`git commit -m 'feat: Add AmazingFeature'`)
4. Push vers la branche (`git push origin feature/AmazingFeature`)
5. Ouvrir une **Pull Request**

### Standards de Code

- **Format** : `cargo fmt` (Rust standard)
- **Lint** : `cargo clippy` (zéro warnings)
- **Tests** : Couverture ≥ 80%
- **Commits** : Convention [Conventional Commits](https://www.conventionalcommits.org/)

---

## 🗺️ Roadmap

### v0.1.0 - Milestone "First Boot" - COMPLETE
- [x] Kernel minimal bootable + HAL (GDT/IDT/PIC/Security)

### v0.2.0 - Milestone "Memory" - COMPLETE
- [x] Frame allocator + Paging + Heap (8 MiB)

### v0.3.0 - Milestone "IPC" - COMPLETE
- [x] Cognitive Bus (lock-free MPMC, Intent-based messages)

### v0.4.0 - Milestone "VFS" - COMPLETE
- [x] Virtual Filesystem with security hardening
- [x] Path traversal + null byte + overflow protection
- [x] Capability-based device access + Metrics
- [x] 14 tests (7 functional + 7 security), 0 warnings

### v0.5.0 - Milestone "Verifier + Process" - COMPLETE
- [x] Policy engine + Syscall filtering + VFS hooks
- [x] Matriarchal process hierarchy (Couche 6)

### v0.8.0 - Milestone "Ring 3 Userspace" - COMPLETE
- [x] SYSCALL/SYSRET + ELF64 loader + per-process paging
- [x] Priority scheduler with aging + GPU stub
- [x] C userspace (libc_stub + hello_c.elf)

### v0.9.0 - Milestone "Jalon 19 - Storage" - COMPLETE
- [x] Full TCP/IP + DNS + HTTP network stack (Couches 17-18)
- [x] VirtIO-Block driver + FAT32 read-only filesystem (Couche 19)
- [x] Ring 3 apps: wget.elf, ls.elf, cat.elf, j19_test.elf
- [x] TSS RSP0 for Ring 3 interrupt safety
- [x] 256 KiB kernel syscall stack (deep VFS/FAT32/VirtIO chains)
- [x] All 12 test suites passing + J19 comprehensive validation 5/5

### v1.0.0 - Milestone "Production Ready" (NEXT)
- [ ] Writable FAT32 (sys_write to /disk/)
- [ ] Multithreading (clone syscall)
- [ ] ML Scheduler + Full test suite + Documentation

---

## 📊 Metriques Actuelles (Jalon 19 Stable)

| Metrique | Valeur | Target | Status |
|----------|--------|--------|--------|
| Boot Time | ~2s (QEMU) | <10s | OK |
| Binary Size | ~2.0 MB (release) | <5 MB | OK |
| Kernel Heap | 8 MiB | - | OK |
| Syscall Stack | 256 KiB | - | OK |
| User Stack | 1 MiB (256 pages) | - | OK |
| ELF Frame Pool | 16 MiB (4096 frames) | - | OK |
| RAM Usage | ~32 MB (QEMU 256M) | <150 MB | OK |
| Test Suites | 12/12 pass | 100% | OK |
| J19 Validation | 5/5 pass | 100% | OK |
| Ring 3 Programs | 6 (hello_c, wget, ls, cat, j19_test, shell) | - | OK |
| Toolchain | nightly-2023-08-01 + bootimage 0.10.3 | strict | OK |

---

## 📜 License

Ce projet est sous licence **MIT**. Voir le fichier [LICENSE](LICENSE) pour plus de détails.

---

## 👨‍💻 Auteur

**MORNINGSTAR**  
- GitHub: [@MORNINGSTAR-OS](https://github.com/Cabrel10)
- Email: morningstar@aetherion.dev
- Project: [AetherionOS](https://github.com/Cabrel10/AetherionOS)

---

## 🙏 Remerciements

- **OSDev Community** : Pour les ressources et la documentation
- **Rust Project** : Pour un langage système moderne
- **Philipp Oppermann** : Pour son excellent tutoriel "[Writing an OS in Rust](https://os.phil-opp.com/)"
- **SerenityOS** : Pour l'inspiration architecturale

---

## 🔗 Liens Utiles

- [Documentation Officielle](https://aetherion-os.dev/docs)
- [Wiki](https://github.com/Cabrel10/AetherionOS/wiki)
- [Discord Community](https://discord.gg/aetherion-os)
- [Twitter](https://twitter.com/AetherionOS)

---

<p align="center">
  <b>✨ Construisons le futur des systèmes d'exploitation ✨</b>
</p>

<p align="center">
  Made with 💙 and Rust 🦀
</p>
