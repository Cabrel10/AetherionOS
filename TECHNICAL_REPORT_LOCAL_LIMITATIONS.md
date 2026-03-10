# Rapport Technique: Limitations et Améliorations Nécessaires
## AetherionOS - Tests Locaux et Production

**Date:** 8 Mars 2026  
**Environnement:** Machine locale (8GB RAM, 4 CPU, 120GB disque)  
**Contexte:** Transition du prototypage vers un système de production robuste

---

## 1. PROBLÈMES CRITIQUES RENCONTRÉS

### 1.1 Out of Memory (OOM) avec Fichiers Volumineux

**Symptôme:**
```
[FATAL] Heap allocation failed! size=2147483648, align=1
[FATAL] System halted - out of memory.
```

**Cause Racine:**
- Le test unitaire `fs::fat32::run_tests()` appelle `read_file_path()` qui tente de charger **l'intégralité** d'un fichier en mémoire
- Avec Mistral 7B (2GB par partie), le kernel heap de 8MB ne peut pas allouer 2GB
- Le système crash **avant même** d'atteindre Ring 3

**Impact:**
- Impossible de tester avec de vrais modèles LLM
- Le système ne boot pas si des fichiers >8MB existent sur disk.img
- Les tests unitaires deviennent des bombes à retardement

**Solution Temporaire Appliquée:**
```rust
// kernel/src/main.rs ligne 1430
// fs::fat32::run_tests();  // PATCH: Disabled to avoid OOM with 2GB Mistral files
```

**PROBLÈME:** Ce patch doit être réappliqué manuellement après chaque `git pull` !

---

### 1.2 Absence de Gestion de Taille de Fichier dans sys_open

**Symptôme:**
```rust
// Ancien code dans sys_open
let exists = crate::fs::fat32::read_file_path(disk_path).is_some();
```

**Problème:**
- `sys_open()` charge le fichier entier juste pour vérifier son existence
- Même avec chunked read disponible (J52), sys_open ignore cette fonctionnalité
- Chaque ouverture de fichier >8MB cause un OOM

**Solution Appliquée:**
```rust
// Nouveau code
pub fn file_exists(disk_path: &str) -> bool {
    fs.find_directory_entry(disk_path).is_some()
}

// Dans sys_open
let exists = crate::fs::fat32::file_exists(disk_path);
```

**PROBLÈME:** Cette fonction n'existe pas dans le code upstream, patch manuel requis !

---

### 1.3 Disk.img Écrasé par Git

**Symptôme:**
```bash
$ ls -lh disk.img
-rw-rw-r-- 1 user user 64M  8 mars  10:41 disk.img  # Devrait être 8GB !
```

**Cause:**
- disk.img est tracké par Git
- Chaque `git pull` écrase notre version locale (8GB avec Mistral) par la version upstream (64MB vide)
- Perte de 4.1GB de données (Mistral 7B) à chaque synchronisation

**Impact:**
- Workflow de développement cassé
- Impossible de maintenir un environnement de test stable
- Recopie manuelle de 4GB nécessaire après chaque pull

---

### 1.4 Absence de Configuration de Lancement d'Agent

**Symptôme:**
```rust
// kernel/src/main.rs ligne 1756 (hardcodé)
let elf_binary = AGENT_HTTP_ELF;
let elf_name = "/bin/agent_http.elf";
```

**Problème:**
- L'agent lancé au boot est hardcodé dans le kernel
- Changer d'agent nécessite recompilation complète du kernel (8 secondes)
- Impossible de tester rapidement différents agents
- Pas de fallback si l'agent crash

---

### 1.5 Tests Unitaires Non Adaptés à la Production

**Problème Structurel:**
```rust
// TEST 5/5 dans fat32.rs
match read_file_path(&path) {  // Charge TOUT le fichier
    Some(data) => {
        // Affiche les 16 premiers bytes
        let preview_len = core::cmp::min(data.len(), 16);
    }
}
```

**Absurdité:**
- Le test charge 2GB en RAM pour afficher 16 bytes
- Aucune vérification de taille avant allocation
- Pas de limite de sécurité (safety limit)

---

## 2. AMÉLIORATIONS CRITIQUES NÉCESSAIRES

### 2.1 Système de Configuration Runtime

**Besoin:**
```toml
# /disk/boot.toml (ou boot.conf)
[boot]
default_agent = "agent_http"
fallback_agent = "agent_orchestrator"
timeout_seconds = 30

[disk]
max_file_preload_mb = 1  # Ne jamais charger >1MB en RAM automatiquement

[debug]
enable_fat32_tests = false
log_level = "info"
```

**Avantages:**
- Changement d'agent sans recompilation
- Configuration persistante sur disque
- Désactivation des tests dangereux via config

**Implémentation Suggérée:**
1. Lire `/disk/boot.toml` au démarrage (avant tests)
2. Parser avec un parser TOML minimal (no_std)
3. Appliquer la config aux modules (FAT32, scheduler, etc.)

---

### 2.2 Refactoring des Tests FAT32

**Proposition:**
```rust
pub fn run_tests_safe() {
    // Test 1-4: inchangés
    
    // Test 5: Lecture sécurisée avec limite
    serial_write("  [TEST 5/5] Read file from subdirectory (chunked)... ");
    if !sub_entries.is_empty() {
        let first_file = sub_entries.iter().find(|e| !e.is_directory);
        if let Some(entry) = first_file {
            let path = alloc::format!("models/{}", entry.name);
            
            // SÉCURITÉ: Ne jamais charger >1MB en test
            let max_test_size = 1024 * 1024; // 1MB
            if entry.file_size > max_test_size {
                serial_println!("SKIP (file too large: {} bytes, using chunked read)", 
                    entry.file_size);
                
                // Test chunked read à la place
                match read_file_path_chunk(&path, 0, 4096) {
                    Some(chunk) => {
                        serial_println!("OK (chunked: {} bytes read from {} total)", 
                            chunk.len(), entry.file_size);
                    }
                    None => serial_println!("FAIL (chunked read failed)"),
                }
            } else {
                // Fichier petit, lecture normale OK
                match read_file_path(&path) {
                    Some(data) => serial_println!("OK ({} bytes)", data.len()),
                    None => serial_println!("FAIL"),
                }
            }
        }
    }
}
```

**Bénéfices:**
- Tests ne crashent jamais, quelle que soit la taille des fichiers
- Validation du chunked read en conditions réelles
- Logs informatifs au lieu de panics

---

### 2.3 Gestion de disk.img Hors Git

**Solution 1: .gitignore**
```bash
# .gitignore
disk.img
disk_*.img
*.gguf
models/
```

**Solution 2: Script de Setup**
```bash
#!/bin/bash
# setup_disk.sh

if [ ! -f "disk.img" ] || [ $(stat -f%z disk.img) -lt 8000000000 ]; then
    echo "Creating 8GB disk.img..."
    dd if=/dev/zero of=disk.img bs=1M count=8192
    mkfs.vfat -F 32 disk.img
    
    # Mount et créer structure
    sudo mount -o loop disk.img /mnt/aetherion
    sudo mkdir -p /mnt/aetherion/models /mnt/aetherion/var
    
    # Copier Mistral si disponible
    if [ -f "mistral-7b-q4.gguf" ]; then
        echo "Splitting Mistral 7B..."
        split -b 2G mistral-7b-q4.gguf /mnt/aetherion/models/part
    fi
    
    sudo umount /mnt/aetherion
    echo "✓ disk.img ready (8GB)"
fi
```

**Solution 3: Disk Template**
```bash
# Fournir disk_template.img (64MB) dans Git
# Utilisateur crée son propre disk.img localement
cp disk_template.img disk.img
./expand_disk.sh  # Script pour passer à 8GB
```

---

### 2.4 API de Gestion de Fichiers Volumineux

**Nouveau Module: `kernel/src/fs/large_file.rs`**
```rust
/// Gestion sécurisée des fichiers volumineux
pub struct LargeFileHandle {
    path: String,
    size: u64,
    current_offset: u64,
    chunk_size: usize,
}

impl LargeFileHandle {
    /// Ouvre un fichier sans le charger en mémoire
    pub fn open(path: &str) -> Result<Self, FsError> {
        let entry = fat32::find_directory_entry(path)?;
        
        if entry.file_size > MAX_DIRECT_LOAD {
            Ok(Self {
                path: path.to_string(),
                size: entry.file_size,
                current_offset: 0,
                chunk_size: 4096,
            })
        } else {
            Err(FsError::FileTooSmall)
        }
    }
    
    /// Lit le prochain chunk
    pub fn read_next(&mut self) -> Option<Vec<u8>> {
        if self.current_offset >= self.size {
            return None;
        }
        
        let chunk = fat32::read_file_path_chunk(
            &self.path, 
            self.current_offset, 
            self.chunk_size as u64
        )?;
        
        self.current_offset += chunk.len() as u64;
        Some(chunk)
    }
    
    /// Seek vers une position
    pub fn seek(&mut self, offset: u64) -> Result<(), FsError> {
        if offset > self.size {
            return Err(FsError::InvalidOffset);
        }
        self.current_offset = offset;
        Ok(())
    }
}

/// Constante de sécurité
const MAX_DIRECT_LOAD: u64 = 1024 * 1024; // 1MB
```

**Usage dans sys_open:**
```rust
fn sys_open(path_addr: u64, flags: u32) -> u64 {
    // ...
    
    if path.starts_with("/disk/") {
        let disk_path = &path[6..];
        
        // Vérifier la taille AVANT de décider comment ouvrir
        if let Some(entry) = fat32::find_directory_entry(disk_path) {
            if entry.file_size > MAX_DIRECT_LOAD {
                // Fichier volumineux: utiliser handle spécial
                let handle = LargeFileHandle::open(disk_path)?;
                return alloc_large_file_fd(handle);
            } else {
                // Fichier petit: comportement normal
                // ...
            }
        }
    }
}
```

---

### 2.5 Système de Logs Structurés

**Problème Actuel:**
```rust
crate::serial_println!("[FAT32] read_file_path: '{}' ...", path);
```

**Proposition:**
```rust
// kernel/src/log.rs
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

pub fn log(level: LogLevel, module: &str, msg: &str) {
    let prefix = match level {
        LogLevel::Debug => "[DEBUG]",
        LogLevel::Info  => "[INFO ]",
        LogLevel::Warn  => "[WARN ]",
        LogLevel::Error => "[ERROR]",
        LogLevel::Fatal => "[FATAL]",
    };
    
    serial_println!("{} [{}] {}", prefix, module, msg);
    
    // Optionnel: écrire dans /disk/var/kernel.log
    if level >= LogLevel::Warn {
        append_to_log_file(msg);
    }
}

// Usage
log(LogLevel::Warn, "FAT32", 
    &format!("Large file detected: {} ({} bytes)", path, size));
```

---

### 2.6 Tests d'Intégration Automatisés

**Script: `test_integration.sh`**
```bash
#!/bin/bash
set -e

echo "=== AetherionOS Integration Tests ==="

# Test 1: Compilation
echo "[1/5] Kernel compilation..."
cd kernel
cargo bootimage --release --quiet
cd ..

# Test 2: Boot rapide (timeout 10s)
echo "[2/5] Boot test..."
timeout 10 qemu-system-x86_64 \
    -enable-kvm -cpu host -smp 4 -m 8G \
    -drive format=raw,file=kernel/target/x86_64-unknown-none/release/bootimage-aetherion-kernel.bin \
    -drive format=raw,file=disk.img,if=virtio \
    -display none -serial stdio \
    -device virtio-net-pci,netdev=net0 \
    -netdev user,id=net0 \
    | tee /tmp/boot.log

# Test 3: Vérifier absence de FATAL
echo "[3/5] Checking for crashes..."
if grep -q "FATAL" /tmp/boot.log; then
    echo "❌ FATAL error detected!"
    grep "FATAL" /tmp/boot.log
    exit 1
fi

# Test 4: Vérifier agent lancé
echo "[4/5] Checking agent launch..."
if ! grep -q "Ring 3 process PID" /tmp/boot.log; then
    echo "❌ Agent failed to launch!"
    exit 1
fi

# Test 5: Vérifier exit propre
echo "[5/5] Checking clean exit..."
if ! grep -q "exited (code 0)" /tmp/boot.log; then
    echo "⚠️  Agent did not exit cleanly"
fi

echo "✓ All tests passed!"
```

---

## 3. ARCHITECTURE PROPOSÉE POUR PRODUCTION

### 3.1 Séparation des Préoccupations

```
kernel/
├── src/
│   ├── fs/
│   │   ├── fat32.rs          # Driver FAT32 de base
│   │   ├── large_file.rs     # Gestion fichiers >1MB
│   │   └── vfs.rs            # Virtual File System
│   ├── config/
│   │   ├── boot_config.rs    # Parser boot.toml
│   │   └── runtime_config.rs # Config modifiable runtime
│   └── test/
│       ├── unit_tests.rs     # Tests unitaires sûrs
│       └── integration.rs    # Tests d'intégration
```

### 3.2 Niveaux de Sécurité

```rust
// kernel/src/config/limits.rs
pub const KERNEL_HEAP_SIZE: usize = 8 * 1024 * 1024; // 8MB

pub const MAX_DIRECT_FILE_LOAD: usize = 1 * 1024 * 1024; // 1MB
pub const MAX_CHUNK_SIZE: usize = 4 * 1024; // 4KB
pub const MAX_OPEN_FILES: usize = 256;

pub const SAFETY_MARGIN: usize = 2 * 1024 * 1024; // 2MB toujours libre

/// Vérifie si une allocation est sûre
pub fn can_allocate(size: usize) -> bool {
    let used = heap_usage();
    let available = KERNEL_HEAP_SIZE - used;
    
    available > (size + SAFETY_MARGIN)
}
```

---

## 4. PLAN D'IMPLÉMENTATION PRIORITAIRE

### Phase 1: Stabilisation (Urgent)
1. ✅ Ajouter `file_exists()` dans fat32.rs
2. ✅ Patcher `sys_open()` pour éviter OOM
3. ✅ Désactiver `run_tests()` par défaut
4. ⚠️ Ajouter disk.img au .gitignore
5. ⚠️ Créer script setup_disk.sh

### Phase 2: Robustesse (Important)
6. Implémenter `LargeFileHandle`
7. Refactorer `run_tests_safe()`
8. Ajouter limites de sécurité dans allocations
9. Créer système de logs structurés
10. Tests d'intégration automatisés

### Phase 3: Production (Souhaitable)
11. Parser boot.toml
12. Configuration runtime
13. Métriques de performance
14. Monitoring de heap usage
15. Documentation complète

---

## 5. MÉTRIQUES DE SUCCÈS

### Avant Améliorations
- ❌ Boot échoue avec fichiers >8MB
- ❌ Tests unitaires crashent le système
- ❌ Recompilation nécessaire pour changer d'agent
- ❌ disk.img écrasé à chaque git pull
- ❌ Pas de logs structurés

### Après Améliorations
- ✅ Boot réussi avec fichiers jusqu'à 8GB
- ✅ Tests unitaires toujours sûrs
- ✅ Changement d'agent via config file
- ✅ disk.img persistant localement
- ✅ Logs structurés avec niveaux

---

## 6. CONCLUSION

Le système actuel est un **excellent prototype** mais nécessite des améliorations critiques pour devenir **production-ready**:

1. **Sécurité Mémoire**: Aucun code ne devrait pouvoir allouer >1MB sans vérification explicite
2. **Configuration**: Hardcoder l'agent de boot dans le kernel est inacceptable
3. **Tests**: Les tests unitaires ne doivent JAMAIS crasher le système
4. **Workflow**: Git ne doit pas gérer les fichiers binaires volumineux

**Recommandation Immédiate:**
Implémenter Phase 1 (5 tâches) avant le prochain Jalon pour éviter la régression continue du workflow de développement local.

---

**Auteur:** Équipe de Test Local  
**Contact:** Pour questions techniques sur ce rapport
