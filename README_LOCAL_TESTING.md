# Guide de Test Local - AetherionOS

Ce guide explique comment tester AetherionOS en local avec de vrais fichiers LLM volumineux.

---

## 🚀 Quick Start

```bash
# 1. Appliquer les patches nécessaires
./apply_local_patches.sh

# 2. Compiler le kernel
cd kernel && cargo bootimage --release && cd ..

# 3. Lancer le test
qemu-system-x86_64 \
  -enable-kvm -cpu host -smp 4 -m 8G \
  -drive format=raw,file=kernel/target/x86_64-unknown-none/release/bootimage-aetherion-kernel.bin \
  -drive format=raw,file=disk.img,if=virtio \
  -device virtio-net-pci,netdev=net0 \
  -netdev user,id=net0 \
  -display none \
  -serial stdio
```

---

## 📋 Prérequis

### Système
- Linux (testé sur Kali)
- 8GB RAM minimum
- 120GB disque (33GB libre minimum)
- KVM activé

### Logiciels
- Rust nightly (1.73.0+)
- QEMU (qemu-system-x86_64)
- GCC, NASM, Git
- sudo (pour monter disk.img)

### Vérification
```bash
rustc --version  # nightly
qemu-system-x86_64 --version
kvm-ok  # Doit afficher "KVM acceleration can be used"
```

---

## 🔧 Configuration Initiale

### 1. Créer disk.img avec Mistral 7B

```bash
# Créer disque 8GB
dd if=/dev/zero of=disk.img bs=1M count=8192
mkfs.vfat -F 32 disk.img

# Monter
sudo mkdir -p /mnt/aetherion
sudo mount -o loop disk.img /mnt/aetherion

# Créer structure
sudo mkdir -p /mnt/aetherion/models /mnt/aetherion/var

# Copier Mistral (si disponible)
# Option A: Fichier unique
if [ -f "mistral-7b-q4.gguf" ]; then
    split -b 2G mistral-7b-q4.gguf /mnt/aetherion/models/part
fi

# Option B: Parties déjà splitées
sudo cp mistral_part_aa /mnt/aetherion/models/part1
sudo cp mistral_part_ab /mnt/aetherion/models/part2
sudo cp mistral_part_ac /mnt/aetherion/models/part3

# Démonter
sudo umount /mnt/aetherion

echo "✓ disk.img ready (8GB with Mistral 7B)"
```

### 2. Appliquer les Patches

```bash
./apply_local_patches.sh
```

**Important:** Ces patches doivent être réappliqués après chaque `git pull` !

---

## 🧪 Tests Disponibles

### Test 1: Boot Basique
```bash
# Boot rapide sans GUI
timeout 30 qemu-system-x86_64 \
  -enable-kvm -cpu host -smp 4 -m 8G \
  -drive format=raw,file=kernel/target/x86_64-unknown-none/release/bootimage-aetherion-kernel.bin \
  -drive format=raw,file=disk.img,if=virtio \
  -display none -serial stdio \
  | tee /tmp/boot.log

# Vérifier succès
grep "Ring 3 process PID" /tmp/boot.log && echo "✓ Boot OK"
grep "FATAL" /tmp/boot.log && echo "❌ Boot FAILED"
```

### Test 2: Lecture GGUF (J54)
```bash
# Modifier kernel/src/main.rs ligne ~1756
# let elf_binary = AGENT_WEIGHT_LOADER_ELF;

# Recompiler et lancer
cd kernel && cargo bootimage --release && cd ..
qemu-system-x86_64 ... | grep "GGUF"

# Attendu:
# [J54] GGUF v3 | tensors=291 | kv_pairs=...
# [J54] Total parameters: ...
```

### Test 3: HTTP Client (J58)
```bash
# Agent par défaut = agent_http
qemu-system-x86_64 ... | grep "J58"

# Attendu:
# [J58] Connected!
# [TCP] Connection ESTABLISHED to 1.1.1.1:80
# [J58] Sent 52
# [J58] Simulated status: HTTP 200
```

### Test 4: Persistent State (J57)
```bash
# Lancer plusieurs fois
for i in {1..3}; do
    echo "=== Boot #$i ==="
    timeout 10 qemu-system-x86_64 ... | grep "boot #"
done

# Attendu:
# Boot #1: [J57] Persistent state loaded: boot #1
# Boot #2: [J57] Persistent state loaded: boot #2
# Boot #3: [J57] Persistent state loaded: boot #3
```

---

## 🐛 Dépannage

### Problème: OOM au boot
```
[FATAL] Heap allocation failed! size=2147483648
```

**Cause:** Tests FAT32 non désactivés

**Solution:**
```bash
grep "fs::fat32::run_tests" kernel/src/main.rs
# Si non commenté:
./apply_local_patches.sh
cd kernel && cargo bootimage --release
```

### Problème: disk.img écrasé
```bash
$ ls -lh disk.img
-rw-rw-r-- 1 user user 64M  # Devrait être 8GB!
```

**Cause:** `git pull` a écrasé le fichier

**Solution:**
```bash
# Recréer disk.img (voir section Configuration Initiale)
# Ou restaurer backup
cp disk.img.backup disk.img
```

### Problème: Compilation échoue
```
error[E0599]: no method named `file_exists`
```

**Cause:** Patch 2 non appliqué

**Solution:**
```bash
# Ajouter manuellement file_exists() dans kernel/src/fs/fat32.rs
# Voir LOCAL_PATCHES.md Patch 2
```

### Problème: Agent ne démarre pas
```
[SYSCALL] Looking for next userspace process: None
```

**Cause:** Agent a crashé ou n'est pas compilé

**Solution:**
```bash
# Vérifier compilation agent
ls -lh userspace/agent_http/target/x86_64-aetherion-user/release/agent_http

# Recompiler si nécessaire
cd userspace/agent_http
cargo build --release --target ../../x86_64-aetherion-user.json \
  -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem
```

---

## 📊 Métriques de Succès

### Boot Réussi
- ✅ Tous les tests système passent (VFS, Scheduler, GPU, etc.)
- ✅ FAT32 détecte les fichiers (part1: 2GB, part2: 2GB, part3: 75MB)
- ✅ Agent lancé en Ring 3 (PID 11)
- ✅ Pas de message FATAL
- ✅ Exit propre (code 0)

### Performance
- Boot time: ~2-3 secondes
- Kernel heap usage: ~2MB / 8MB
- Network ping: <10ms vers gateway
- TCP connection: ~2 retransmissions vers Internet

---

## 🔄 Workflow Recommandé

### Développement Quotidien
```bash
# 1. Synchroniser code
git pull origin main

# 2. Réappliquer patches
./apply_local_patches.sh

# 3. Compiler agents modifiés
cd userspace/agent_xxx
cargo build --release --target ../../x86_64-aetherion-user.json \
  -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem
cd ../..

# 4. Recompiler kernel
cd kernel
CARGO_BUILD_JOBS=4 cargo bootimage --release
cd ..

# 5. Tester
qemu-system-x86_64 ... | tee /tmp/test.log
```

### Avant Commit
```bash
# 1. Vérifier que les patches sont documentés
git status
# Ne pas commiter les patches dans kernel/src/

# 2. Tester boot
./apply_local_patches.sh
cd kernel && cargo bootimage --release && cd ..
timeout 30 qemu-system-x86_64 ... | grep "exited (code 0)"

# 3. Commit uniquement le code nouveau
git add userspace/agent_xxx/
git commit -m "feat: Add agent_xxx"
```

---

## 📚 Documentation Complète

- `EXECUTIVE_SUMMARY.md` - Résumé court (1 page)
- `TECHNICAL_REPORT_LOCAL_LIMITATIONS.md` - Analyse détaillée
- `LOCAL_PATCHES.md` - Procédure de patch
- `AGENT_FEEDBACK.md` - Retour d'expérience
- `apply_local_patches.sh` - Script automatique

---

## 🆘 Support

### Logs Utiles
```bash
# Boot complet
qemu-system-x86_64 ... 2>&1 | tee /tmp/full_boot.log

# Seulement erreurs
qemu-system-x86_64 ... 2>&1 | grep -E "FATAL|ERROR|FAIL"

# Métriques système
qemu-system-x86_64 ... 2>&1 | grep -E "METRICS|Heap|RAM"
```

### Vérification État
```bash
# Patches appliqués?
./apply_local_patches.sh

# Disk.img correct?
ls -lh disk.img  # Doit être 8GB
sudo mount -o loop disk.img /mnt/aetherion
ls -lh /mnt/aetherion/models/
sudo umount /mnt/aetherion

# Kernel compilé?
ls -lh kernel/target/x86_64-unknown-none/release/bootimage-aetherion-kernel.bin
```

---

**Dernière mise à jour:** 8 Mars 2026  
**Version:** 1.0 (Jalons 52-58)
