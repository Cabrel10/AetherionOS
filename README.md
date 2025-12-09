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
| **0** | Fondations | 1 sem | 🟢 COMPLETE | Kernel minimal bootable |
| **1** | Memory Mgmt | 1 sem | 🟡 IN PROGRESS | Physical/Virtual allocators |
| **2** | Syscalls & User | 1 sem | ⚪ PLANNED | Ring 3 transitions |
| **3** | VFS & Drivers | 2 sem | ⚪ PLANNED | Filesystem + I/O |
| **4** | Sécurité Avancée | 2 sem | ⚪ PLANNED | Secure Boot + TPM |
| **5** | ML Scheduler | 2 sem | ⚪ PLANNED | Ordonnanceur intelligent |
| **6** | Réseau | 2 sem | ⚪ PLANNED | TCP/IP stack |
| **7** | Tests & QA | 2 sem | ⚪ PLANNED | Test suite complète |
| **8** | Optimisations | 2 sem | ⚪ PLANNED | Performance tuning |

**Durée Totale** : ~15 semaines (3.5 mois)

---

## 💻 Installation

### Prérequis

- **Rust** : nightly toolchain (≥ 1.75.0)
- **QEMU** : x86_64 system emulator
- **Build Tools** : nasm, ld, make
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

### v0.1.0 (Q1 2025) - Milestone "First Boot"
- [x] Kernel minimal bootable
- [x] Bootloader BIOS
- [x] VGA text output
- [ ] Memory management complet
- [ ] Basic syscalls

### v0.2.0 (Q2 2025) - Milestone "Userland"
- [ ] User mode processes
- [ ] Shell interactif
- [ ] Filesystem (FAT32)
- [ ] Driver keyboard

### v0.3.0 (Q2 2025) - Milestone "Network"
- [ ] TCP/IP stack
- [ ] virtio-net driver
- [ ] HTTP client
- [ ] DNS resolver

### v0.4.0 (Q3 2025) - Milestone "Security"
- [ ] Secure Boot
- [ ] TPM 2.0 integration
- [ ] ML anomaly detection
- [ ] ASLR kernel

### v1.0.0 (Q4 2025) - Milestone "Production Ready"
- [ ] ML Scheduler stable
- [ ] Test suite complète
- [ ] Documentation exhaustive
- [ ] Performance benchmarks publiés

---

## 📊 Métriques Actuelles

| Métrique | Valeur | Target | Status |
|----------|--------|--------|--------|
| Boot Time | TBD | <10s | 🟡 |
| Binary Size | ~50 KB | <5 MB | ✅ |
| RAM Usage | ~10 MB | <150 MB | ✅ |
| Test Coverage | TBD | ≥80% | 🟡 |
| Documentation | 1000+ lines | Complete | ✅ |

---

## 📜 License

Ce projet est sous licence **MIT**. Voir le fichier [LICENSE](LICENSE) pour plus de détails.

---

## 👨‍💻 Auteur

**Cabrel Foka**  
- GitHub: [@Cabrel10](https://github.com/Cabrel10)
- Email: cabrel@aetherion.dev
- LinkedIn: [Cabrel Foka](https://linkedin.com/in/cabrel-foka)

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
