# Aetherion OS - Décisions d'Architecture Kernel

**Document Version** : 1.0  
**Date** : 2025-12-09  
**Auteur** : MORNINGSTAR  
**Status** : Living Document (mis à jour régulièrement)

---

## 📋 Table des Matières

1. [Vue d'Ensemble](#vue-densemble)
2. [Choix Fondamentaux](#choix-fondamentaux)
3. [Architecture Hybride](#architecture-hybride)
4. [Gestion Mémoire](#gestion-mémoire)
5. [Ordonnancement](#ordonnancement)
6. [Sécurité](#sécurité)
7. [Système de Fichiers](#système-de-fichiers)
8. [Réseau](#réseau)
9. [Décisions Rejetées](#décisions-rejetées)
10. [Évolution Future](#évolution-future)

---

## 🎯 Vue d'Ensemble

Aetherion OS est conçu avec une philosophie claire : **combiner la sécurité de Rust avec les avancées modernes en architecture système**. Chaque décision architecturale est guidée par trois principes :

1. **Sécurité First** : Memory safety, capability-based security
2. **Performance Optimale** : Zero-cost abstractions, prédictions ML
3. **Maintenabilité** : Code clean, documentation exhaustive

---

## 🔧 Choix Fondamentaux

### 1. Langage : Rust (no_std)

**Décision** : Utiliser Rust en mode `no_std` (bare-metal)

#### ✅ Avantages
- **Memory Safety** : Aucun buffer overflow, use-after-free éliminé au compile-time
- **Zero-Cost Abstractions** : Performance équivalente au C sans sacrifier la lisibilité
- **Ownership Model** : Gestion mémoire sans GC (idéal pour kernel)
- **Concurrent Safety** : Send/Sync traits préviennent les data races
- **Modern Tooling** : Cargo, rustfmt, clippy, excellent écosystème

#### ⚠️ Challenges
- Courbe d'apprentissage Rust
- Ecosystem bare-metal moins mature que C
- Besoin de unsafe blocks pour hardware access
- Compilation plus lente que C

#### 🎯 Justification
Le coût initial de la courbe d'apprentissage est largement compensé par :
- Réduction drastique des bugs mémoire (70% des CVE Linux/Windows)
- Maintenance simplifiée grâce au système de types
- Communauté active (OSDev in Rust en croissance)

**Alternatives Rejetées** :
- C/C++ : Trop de risques mémoire
- Zig : Ecosystem trop jeune
- Go : GC incompatible avec kernel

---

### 2. Architecture Cible : x86_64

**Décision** : Cibler exclusivement x86_64 (AMD64) pour v1.0

#### ✅ Raisons
- **Ubiquité** : >90% des desktops/servers
- **Documentation** : Excellente (Intel/AMD manuals)
- **Tooling** : QEMU, BOCHS, support mature
- **Features** : 4-level paging, IOMMU, virtualization

#### 🔮 Future
- ARM64 envisagé pour v2.0 (Raspberry Pi, cloud)
- RISC-V considéré pour v3.0 (open ISA)

---

### 3. Boot : BIOS → UEFI (Phase 0-4)

**Décision** : Commencer avec BIOS Legacy, migrer vers UEFI Phase 4

#### Phase 0-3 : BIOS Legacy
```
Bootloader (512 bytes) → Load Kernel → Long Mode → Kernel main()
```

**Avantages** :
- Simplicité initiale (MBR boot sector)
- Compatibilité hardware étendue
- Debugging facile (QEMU -bios)

#### Phase 4+ : UEFI
```
UEFI Firmware → Bootloader (EFI App) → Secure Boot → Kernel
```

**Avantages UEFI** :
- Secure Boot natif
- GOP (Graphics Output Protocol) vs VGA
- Meilleure gestion 64-bit
- TPM integration plus simple

---

## 🏗️ Architecture Hybride

**Décision** : Microkernel Philosophy + Monolithic Performance

### Concept : "Flexible Microkernel"

```
┌─────────────────────────────────────────┐
│         Kernel Space (Ring 0)           │
├──────────────────┬──────────────────────┤
│   Microkernel    │   In-Kernel Drivers  │
│   (Core Only)    │   (Performance)      │
├──────────────────┴──────────────────────┤
│  - IPC Basique        - VGA Driver      │
│  - Memory Mgmt        - Serial Driver   │
│  - Scheduler          - Keyboard        │
│  - Syscalls           - Disk (ATA)      │
└──────────────────────────────────────────┘
         ↕ Syscalls ↕
┌──────────────────────────────────────────┐
│         User Space (Ring 3)              │
├──────────────────────────────────────────┤
│  - User Processes                        │
│  - Optional User Drivers (future)        │
│  - Shell & Utils                         │
└──────────────────────────────────────────┘
```

### Rationale

#### Microkernel (Core)
- **Minimal TCB** (Trusted Computing Base) : ~10k LOC
- **Isolation** : Bugs drivers n'affectent pas kernel core
- **Modularity** : Facile d'ajouter/retirer composants

#### Monolithic (Drivers)
- **Performance** : Pas de context switch pour I/O
- **Latency** : Accès direct hardware
- **Simplicité** : Moins de IPC overhead

### Trade-off Accepté
- Bugs drivers peuvent crash kernel
- **Mitigation** : Tests exhaustifs + isolation future (IOMMU)

---

## 💾 Gestion Mémoire

### Architecture 3-Niveaux

```
1. Physical Memory Manager
   ├─ Bitmap Allocator (4KB frames)
   └─ Detect via Multiboot memory map

2. Virtual Memory Manager
   ├─ 4-Level Paging (PML4 → PDP → PD → PT)
   ├─ Mapper/Unmapper pages
   └─ TLB management

3. Heap Allocator
   ├─ GlobalAlloc trait implementation
   ├─ Bump allocator (Phase 1)
   └─ Slab allocator (Phase 8)
```

### Décisions Clés

#### 1. Page Size : 4 KB (small pages)

**Pourquoi pas 2MB huge pages ?**
- Fragmentation réduite (4KB granularité)
- Protection fine-grained
- Huge pages ajoutées Phase 8 (optimisation)

#### 2. Allocator : Bump → Slab

**Phase 1** : Bump Allocator
- Ultra simple (pointeur++)
- Pas de free() (acceptable initialement)
- Boot rapide

**Phase 8** : Slab Allocator
- Free() supporté
- Moins de fragmentation
- Caching objets fréquents

#### 3. Memory Layout

```
0x0000_0000_0000_0000 → 0x0000_7FFF_FFFF_FFFF : User Space
0xFFFF_8000_0000_0000 → 0xFFFF_FFFF_FFFF_FFFF : Kernel Space

Kernel Layout:
├─ 0xFFFF_8000_0000_0000 : Kernel Code (.text)
├─ 0xFFFF_8000_0010_0000 : Kernel Data (.data)
├─ 0xFFFF_8000_0020_0000 : Kernel Heap
├─ 0xFFFF_8000_1000_0000 : Physical Memory Map (Identity)
└─ 0xFFFF_FFFF_8000_0000 : Kernel Stack (grows down)
```

**Justification** :
- Séparation claire user/kernel (bit 47)
- Identity mapping physique (accès direct frames)
- Stack guard page (detect overflow)

---

## ⚙️ Ordonnancement

### Évolution en 3 Phases

#### Phase 1-2 : Round-Robin Naïf
```rust
fn schedule() -> &'static Task {
    current_task = (current_task + 1) % tasks.len();
    &tasks[current_task]
}
```
- Simple à implémenter
- Équitable (chaque task reçoit égal CPU)
- Pas de priorités

#### Phase 5 : Priority-Based + Préemption
```rust
struct Task {
    priority: u8,      // 0 (lowest) - 255 (highest)
    time_slice: u64,   // Quantum en µs
    state: TaskState,  // Ready/Running/Blocked
}
```
- Préemption timer-based (PIT interrupt)
- Priorités dynamiques (anti-starvation)

#### Phase 5 : ML-Powered Scheduler (INNOVATION)

**Concept** : Prédire le comportement des processus pour optimiser scheduling

```
[Input Features]
├─ CPU Usage History (5 derniers timeslices)
├─ I/O Wait Time
├─ Priority
├─ Process Age
└─ Cache Miss Rate

        ↓
   [ML Model]
   (Decision Tree)
        ↓
   
[Predictions]
├─ Prochaine durée CPU burst
├─ Probabilité I/O imminente
└─ Optimal time slice

        ↓
[Scheduler Decision]
- Allocation CPU
- Time slice ajusté
- Priorité dynamique
```

**Dataset** : Collecté runtime (~1000 samples/process)

**Model** : Decision Tree (lightweight, pas de float ops lourdes)

**Expected Gains** :
- Réduction latence I/O : 20-30%
- Meilleur throughput : 10-15%
- Overhead ML : <5% CPU

**Fallback** : Si prédictions mauvaises (accuracy <60%), retour Priority-Based

---

## 🔒 Sécurité

### Architecture Multi-Couches

```
Layer 1: Hardware (TPM, IOMMU)
    ↓
Layer 2: Boot Security (Secure Boot, Measured Boot)
    ↓
Layer 3: Kernel Security (ASLR, DEP, W^X)
    ↓
Layer 4: Process Isolation (Capabilities, Sandboxing)
    ↓
Layer 5: ML Anomaly Detection
```

### Décisions Clés

#### 1. Secure Boot (Phase 4)

**Chain of Trust** :
```
UEFI Firmware (Platform Key)
    ↓ verifies
Bootloader Signature (Key Exchange Key)
    ↓ verifies
Kernel Signature (Aetherion Signing Key)
    ↓ loads
Kernel (vérifié, trusted)
```

**Implementation** :
- EFI_IMAGE_SECURITY_DATABASE
- SHA-256 hashes
- RSA-2048 signatures

#### 2. TPM 2.0 Integration (Phase 4)

**Use Cases** :
- **Measured Boot** : PCRs 0-7 contiennent hashes boot components
- **Sealed Storage** : Keys chiffrées, unsealed si PCRs correctes
- **Attestation** : Remote attestation (cloud scenarios)

**TPM Operations** :
```rust
tpm.extend_pcr(0, bootloader_hash);
tpm.extend_pcr(4, kernel_hash);
let sealed_key = tpm.seal(key, pcr_selection);
```

#### 3. ASLR Kernel (Phase 4)

**Décision** : Randomiser adresses kernel à chaque boot

**Technique** :
- Base kernel aléatoire dans `0xFFFF_8000_XXXX_XXXX`
- Entropy : 28 bits (256M positions)
- Stack/Heap aussi randomisés

**Challenges** :
- Relocations kernel (PIE)
- Performance impact minimal (<1%)

#### 4. Capability-Based Security (Phase 2+)

**Concept** : Pas de permissions globales, mais capabilities explicites

```rust
struct Process {
    capabilities: HashSet<Capability>,
}

enum Capability {
    ReadFile(FileHandle),
    WriteFile(FileHandle),
    NetworkAccess,
    SpawnProcess,
}
```

**Avantages** :
- Least Privilege par défaut
- Pas de confused deputy problem
- Révocation facile

#### 5. ML Anomaly Detection (Phase 4)

**Monitoring** :
- Syscalls patterns (séquences inhabituelles)
- Network traffic (DDoS detection)
- File access (ransomware behavior)

**Model** : Isolation Forest (unsupervised)

**Actions** :
- Log anomaly
- Rate-limit process
- Kill process (si score critique)

---

## 📁 Système de Fichiers

### VFS (Virtual File System) - Phase 3

**Architecture** :

```
┌──────────────────────────────────────┐
│    VFS Layer (Abstraction)           │
├──────────────────────────────────────┤
│  struct Inode { ... }                │
│  struct Dentry { ... }               │
│  trait FileSystem { ... }            │
└──────────────────────────────────────┘
         ↓ implémentations ↓
┌──────────┬───────────┬───────────┐
│  FAT32   │   Ext2    │  TmpFS    │
└──────────┴───────────┴───────────┘
```

### Décisions FS

#### Phase 3 : FAT32 Primary

**Pourquoi FAT32 ?**
- ✅ Simple à implémenter (pas de journaling)
- ✅ Interopérabilité (Windows/Linux/macOS)
- ✅ Bien documenté
- ⚠️ Pas de permissions (acceptable Phase 3)

#### Phase 6+ : Ext2 Support

**Ajout Ext2** :
- Permissions Unix (owner/group/other)
- Hard links / Symlinks
- Meilleure performance (block groups)

#### Phase 8 : AetherionFS (Custom)

**Features Uniques** :
- Copy-on-Write (ZFS-like)
- Checksums (intégrité données)
- Snapshots
- Compression (LZ4)

---

## 🌐 Réseau

### Stack TCP/IP - Phase 6

**Architecture Layers** :

```
Layer 7: Applications (HTTP, DNS)
    ↓
Layer 4: TCP / UDP
    ↓
Layer 3: IP (IPv4 → IPv6 Phase 7)
    ↓
Layer 2: Ethernet (802.3)
    ↓
Layer 1: Driver (virtio-net, e1000)
```

### Décisions Réseau

#### 1. Driver : virtio-net (QEMU)

**Pourquoi virtio ?**
- Optimisé pour VMs (paravirtualization)
- Performance excellente (vs e1000 émulé)
- QEMU support natif

**Fallback** : e1000 driver (real hardware)

#### 2. IPv4 First, IPv6 Later

**Phase 6** : IPv4 uniquement
- Simplicité (32-bit addresses)
- Tests plus faciles

**Phase 7** : Dual-stack IPv4/IPv6
- IPv6 natif (128-bit)
- Tunneling 6to4

#### 3. TCP Implementation

**État Machine** :
```
CLOSED → LISTEN → SYN_RECEIVED → ESTABLISHED
         ↓                            ↓
       CLOSE                        FIN_WAIT
```

**Challenges** :
- Gestion buffers (send/recv queues)
- Retransmissions (timeouts)
- Congestion control (Reno algorithm)

#### 4. HTTP/3 (Phase 7+)

**Décision Ambitieuse** : Support QUIC + HTTP/3

**Raison** :
- Moderne (Google/Cloudflare use)
- Meilleure latence (0-RTT)
- Multiplexing sans head-of-line blocking

**Challenge** : Complexité UDP-based + TLS 1.3

---

## ❌ Décisions Rejetées

### 1. ❌ Microkernel Pur (Minix-style)

**Raison Rejet** : Performance overhead inacceptable
- Chaque I/O = 2+ context switches
- Latency critique pour VGA/Serial

**Leçon** : Pureté architecturale < Performance réelle

---

### 2. ❌ Cooperative Scheduling (No Preemption)

**Raison Rejet** : Un process malveillant peut monopoliser CPU
- Besoin de préemption pour fairness
- Timer interrupts nécessaires

---

### 3. ❌ No Memory Protection (Single Address Space)

**Raison Rejet** : Bugs = system crash
- Isolation essentielle pour stabilité
- Paging overhead acceptable (<5%)

---

### 4. ❌ Exokernel (Minimal Abstraction)

**Raison Rejet** : Trop complexe pour applications
- Chaque app devrait gérer hardware
- Pas de portabilité

---

### 5. ❌ Real-Time OS (RTOS)

**Raison Rejet** : Pas de besoins hard real-time
- General-purpose OS suffit
- RTOS = sacrifices throughput

---

## 🔮 Évolution Future

### Phase 9+ : Post-v1.0

#### 1. Multi-Architecture
- ARM64 support (Phase 9)
- RISC-V port (Phase 10)

#### 2. GUI
- Framebuffer driver
- Compositor simple
- X11/Wayland compatibility ?

#### 3. Package Manager
- `apm` (Aetherion Package Manager)
- Binary packages
- Source compilation

#### 4. Containers
- Namespaces (PID, Network, Mount)
- Cgroups (Resource limits)
- OCI-compatible runtime

#### 5. Cloud-Ready
- AWS/GCP kernel optimizations
- virtio full support
- Cloud-init integration

---

## 📊 Tableau Récapitulatif

| Composant | Décision | Phase | Alternatives Rejetées |
|-----------|----------|-------|-----------------------|
| **Langage** | Rust | 0 | C, C++, Zig |
| **Architecture** | x86_64 | 0 | ARM64 (future), RISC-V |
| **Boot** | BIOS → UEFI | 0-4 | Direct UEFI |
| **Kernel Style** | Hybrid | 0 | Microkernel pur, Monolithic |
| **Memory** | 4KB Pages | 1 | 2MB Huge pages |
| **Allocator** | Bump → Slab | 1-8 | Buddy, Best-fit |
| **Scheduler** | RR → ML | 1-5 | CFS, O(1) |
| **Security** | Secure Boot + ML | 4 | Pas de Secure Boot |
| **Filesystem** | FAT32 → Ext2 | 3-6 | Ext4, Btrfs |
| **Network** | virtio-net | 6 | e1000, rtl8139 |
| **TCP/IP** | Custom Stack | 6 | lwIP (port) |

---

## 🎓 Principes Directeurs

### 1. Simplicité d'Abord
> "Simple first, optimize later"

- Phase 0-3 : Implémentations naïves mais fonctionnelles
- Phase 8 : Optimisations basées sur profiling

### 2. Security by Design
> "Prevention > Detection > Response"

- Rust memory safety (compile-time)
- Capabilities (runtime)
- ML detection (reactive)

### 3. Documentation = Code
> "Undocumented feature = non-existent feature"

- Chaque fonction documentée (rustdoc)
- Décisions architecturales justifiées (ce doc)

### 4. Tests Exhaustifs
> "Untested code = broken code"

- Unit tests (≥80% coverage)
- Integration tests (end-to-end)
- Fuzz testing (AFL++)

---

## 📚 Références

### Livres
- "Operating Systems: Three Easy Pieces" (OSTEP)
- "The Rustonomicon" (Unsafe Rust)
- "Modern Operating Systems" (Tanenbaum)

### Projets Inspirants
- **SerenityOS** : Architecture propre
- **Redox OS** : Rust microkernel
- **Linux** : Pragmatisme
- **MINIX 3** : Pureté microkernel

### Papers
- "The UNIX Time-Sharing System" (Ritchie & Thompson, 1974)
- "Meltdown & Spectre" (2018) - Side-channel attacks
- "Defeating Kernel ASLR" (2013) - Security

---

## ✍️ Changelog du Document

| Version | Date | Changements |
|---------|------|-------------|
| 1.0 | 2025-12-09 | Création initiale complète |

---

**Auteur** : MORNINGSTAR  
**Contact** : morningstar@aetherion.dev  
**Status** : Living Document (updated with project evolution)

---

<p align="center">
  <i>« Une architecture bien pensée vaut mille optimisations »</i>
</p>
