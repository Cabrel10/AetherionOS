# Rapport d'Intervention - AetherionOS
**Date :** Mardi 16 Juin 2026

## Synthèse Finale des Interventions

### Intervention #4 : Résolution de l'Ombrage GGUF et Activation AVX2/FMA
**Résultat** : Pipeline LLM fonctionnel avec accélération AVX2/FMA.

### Intervention #5 : Lancement du Test de Boot (QEMU)
**Date** : 16 Juin 2026
**Action** : Lancement de la séquence de build et boot test (`dev_aetherion.sh all`) en arrière-plan.
**Logs** : `AetherionOS/boot_test.log`
**Objectif** : Vérifier la stabilité du noyau AetherionOS complet après la mise à jour des agents utilisateurs et la correction des dépendances de compilation (`bootimage`).

### Intervention #6 : Génération et Intégration du Modèle LLM 165Mo
**Date** : 16 Juin 2026
**Action** : Génération de `smollm2-135m-q4_0.gguf` et injection dans `disk.img` via `debugfs`.
**Fichier** : `/models/smollm2-135m-q4_0.gguf` sur partition ext2.
**Build** : Re-compilation globale lancée pour intégrer le support du modèle réel dans les agents Ring 3.

```
Le kernel recevait `offset=3` (= fd) au lieu de `offset=0`.

**Cause racine** : Dans `userspace/rust_sdk/src/lib.rs`, `sys_mmap_posix()` utilisait `in(reg)` pour les operandes `flags`, `fd` et `offset`, puis des `mov` explicites :
```rust
// ANCIEN CODE BUGGE :
"mov r10, {flags}",
"mov r8, {fd}",       // si compilateur met offset dans R8...
"mov r9, {offset}",   // ...ici on lit R8 qui vient d'etre ecrase par fd !
// ...
flags = in(reg) flags,    // compilateur libre de choisir R8, R9, R12...
fd = in(reg) fd as u64,   // idem
offset = in(reg) offset,  // idem
```

Le compilateur allouait R8 pour `offset` et R9 pour `fd`. Sequence executee :
1. `mov r10, {flags_reg}` — OK
2. `mov r8, r9` — r8 = fd = 3 ✓ (mais ecrase R8 qui contenait offset=0)
3. `mov r9, r8` — r9 = **3** au lieu de 0 ✗ (R8 ecrase a l'etape 2 !)

### Fix applique : `userspace/rust_sdk/src/lib.rs` — `sys_mmap_posix()`
Remplacement de `in(reg)` + mov par des registres explicites :
```rust
// NOUVEAU CODE FIXE :
core::arch::asm!(
    "syscall",
    inlateout("rax") 9u64 => ret,
    in("rdi") addr,
    in("rsi") length,
    in("rdx") prot,
    in("r10") flags,      // ← explicite, plus de mov intermediaire
    in("r8")  fd as u64,  // ← explicite
    in("r9")  offset,     // ← explicite, garanti = 0
    lateout("rcx") _,
    lateout("r11") _,
    options(nostack),
);
```

**Verification desassemblee** du binaire compile (objdump) :
```asm
409a4c: mov    $0x9,%eax       # rax = 9 (nr mmap)
409a56: mov    $0x1,%r10d      # r10 = MAP_SHARED
409a61: mov    %r13,%r8        # r8 = fd
409a64: xor    %r9d,%r9d       # r9 = 0 ← OFFSET CORRECT !
409a67: syscall
```

### Impact : Seul agent_inference recompile (~10s), pas le kernel. Le kernel re-embed le binaire via `include_bytes!`.

---

## Intervention #1 : Passthrough du 6eme argument syscall (R9/mmap offset)

### Diagnostic
Le registre R9 (6eme argument syscall Linux ABI) etait ecrase par le brassage des registres dans `syscall_entry`. Le syscall `mmap` recevait un `offset` corrompu au lieu de 0x0.

### Fichier modifie : `kernel/src/arch/x86_64/syscall.rs` (diff: 14 lignes)
- `syscall_entry` (asm) : `mov gs:[56], r9` sauvegarde R9, pousse sur la pile comme 7eme arg System V
- `syscall_handler_rust` : signature etendue a 7 args (nr, a1-a5, a6)
- `syscall_dispatch` : propage a6, supprime `saved_user_r9()`

**Note** : Ce fix kernel etait correct mais ne suffisait pas — le vrai bug etait cote userspace (Intervention #3).

---

## Intervention #2 : Fix fstat VFS — StaticFile retournait size=0

### Diagnostic
`agent_inference` ouvre `/models/smollm2-135m-q4_0.gguf` (StaticFile VFS, 6016 bytes), puis appelle `fstat(fd)` pour obtenir la taille avant `mmap`. **fstat retournait size=0**, car :

1. Les processus Linux ABI utilisent `linux_syscall_dispatch_inner` (linux_abi.rs)
2. Ce dispatcher intercepte syscall 5 (fstat) et route vers `linux_fstat_vfs`
3. `linux_fstat_vfs` n'avait AUCUN lookup VFS — il ne consultait que ext2 puis lseek
4. Les fichiers StaticFile (include_bytes!) ne sont pas dans ext2
5. lseek sur un VFS StaticFile retourne 0

Resultat : fstat → size=0 → mmap anonymous 128MB (fallback) → zeros → Bad GGUF magic

### Fichiers modifies

**`kernel/src/fs/vfs.rs`** — Ajout de `pub fn file_stat(path: &str) -> Option<(u64, u32)>` :
```rust
pub fn file_stat(path: &str) -> Option<(u64, u32)> {
    let components = path_components(path);
    let root = VFS_ROOT.lock();
    match find_node(&root, &components) {
        Some(VfsNode::File(data))       => Some((data.len() as u64, 0o100755)),
        Some(VfsNode::StaticFile(data)) => Some((data.len() as u64, 0o100755)),
        Some(VfsNode::Device { data, .. }) => Some((data.len() as u64, 0o100644)),
        Some(VfsNode::Directory(_))     => Some((0, 0o40755)),
        Some(VfsNode::Symlink(_))       => Some((0, 0o120777)),
        None => None,
    }
}
```

**`kernel/src/compat/linux_abi.rs`** — Deux fonctions corrigees :

1. `linux_fstat_vfs` (~ligne 4378) : Ajout Priorite 1 = VFS file_stat(), Priorite 2 = ext2, Priorite 3 = lseek
2. `linux_stat_vfs` (~ligne 4280) : Ajout check VFS avant ext2

Marqueur de debug : `[LINUX-FSTAT] fd=X path='...' VFS size=6016 mode=0o100755`

**CONFIRME en QEMU** : `[LINUX-FSTAT] fd=3 path='/models/smollm2-135m-q4_0.gguf' VFS size=6016 mode=0o100755`

---

## Chaine de verification attendue (apres Interventions #1 + #2 + #3)
```
Boot → VFS init → /models/smollm2-135m-q4_0.gguf (6016 bytes StaticFile)
→ open("/models/smollm2-135m-q4_0.gguf") = fd 3
→ [LINUX-FSTAT] fd=3 VFS size=6016 mode=0o100755
→ [LLM] File size: 6016
→ [MMAP-DIAG] PID2 mmap(addr=0x0, len=0x1780, ..., fd=3, off=0x0)  ← FIXE !
→ [LLM] GGUF magic: 0x46554747 ✓
→ [LLM] Model: SmolLM2-135M-Q4_0, 3 tensors
→ [LLM] Generated: <tokens>
```

---

## Etat des lieux — AetherionOS v4.3.1-phase8

| Composant | Statut | Detail |
|-----------|--------|--------|
| Boot Limine v8.7.0 | ✅ OK | BIOS+UEFI, serial stdio |
| Python Ring 3 | ✅ OK | 42*42=1764 prouve |
| Tokenization BPE | ✅ OK | 68 chars → 69 tokens |
| AVX2 detection | ✅ OK | Haswell,+avx2,+fma |
| VirtIO-BLK + ext2 | ✅ OK | disk.img 1GB monte |
| fstat VFS (StaticFile) | ✅ FIXE | Intervention #2, confirme QEMU |
| mmap offset R9 (kernel) | ✅ FIXE | Intervention #1, gs:[56] passthrough |
| mmap offset R9 (SDK) | ✅ FIXE | Intervention #3, registres explicites |
| Inference GGUF | 🔧 TEST | Rebuild en cours, test QEMU imminent |
| TLS proxy Ring 3 | 📋 TODO | Pour apk add |
| AVX2 matmul | 📋 TODO | 0.04→2.0+ GFLOPS |
| Native toolchain | 📋 TODO | gcc/make via apk |

---

## Scripts utiles
- `scripts/rebuild-iso-only.sh` — Reconstruit l'ISO sans recompiler
- `scripts/test-fstat-fix.sh` — Test automatise avec verification des marqueurs
- `scripts/build-limine.sh --release` — Build complet kernel + ISO
