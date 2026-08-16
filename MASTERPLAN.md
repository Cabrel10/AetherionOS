# AetherionOS — MASTERPLAN V5

> **Auteur** : Cabrel10 (Chef de Projet)
> **Date** : 2026-05-14
> **Principe** : Aucune phase n'est validee sans preuve QEMU dans les logs CI.
> **Methode** : Pushes incrementaux cibles. 1 push = 1 objectif. Pas de mega-commits.

---

## Etat actuel (base : commit eadc489)

| Composant         | Statut     | Preuve CI                                    |
|-------------------|------------|----------------------------------------------|
| KPTI + IRETQ      | OK         | 8 paths HHDM mini-stack, 0 triple fault      |
| Scheduler          | OK         | PID 1 exit propre                            |
| Syscalls POSIX     | OK         | 200+ syscalls, fork/execve/waitpid           |
| Dynamic Linker     | OK         | ld-musl charge python3 + libpython3.12.so    |
| ext2 lecture       | OK         | stat, readdir, read_file_chunk               |
| ext2 ecriture      | OK         | wget -> ext2 write -> read-back              |
| VirtIO-Net         | OK         | ICMP ping, DNS, HTTP, HTTPS/TLS 1.3          |
| VirtIO-BLK         | OK         | 512 MiB disk, ext2 mount                     |
| Python3 Ring 3     | **PROUVE** | `1764` dans logs CI (run 25843946148)         |
| APK Ring 3         | PARTIEL    | PID=2 loaded+launched (SIGSEGV crypto init)   |
| LLM GGUF Parse     | CODE PRET  | Bloque par KERN_SIZE fix, code complet         |
| LLM Forward Pass   | CODE PRET  | 2-layer forward + greedy sample, attente CI   |
| Node.js            | NON TENTE  | -                                             |
| GCC compile+exec   | NON TENTE  | -                                             |

---

## PHASE 1 — Stabilite Runtime Lourd

**Objectif** : L'OS execute des environnements complexes avec allocation dynamique et .so

### Etape 1.1 — APK Ring 3 (Push 1)
- Charger `/sbin/apk --version` comme PID 2 apres Python3
- **Marqueur CI** : `apk-tools` ou `APK-RING3-OK` dans qemu-serial.log
- **Diff attendu** : ~30 lignes dans `limine_entry.rs`

### Etape 1.2 — Node.js (futur)
- Installer node via Alpine, executer `node -e "console.log(6*7)"`
- **Marqueur CI** : `42` dans qemu-serial.log

### Etape 1.3 — GCC compile + execute (futur)
- `gcc -o hello hello.c && ./hello`
- **Marqueur CI** : `Hello from GCC` dans qemu-serial.log

---

## PHASE 2 — Reseau Souverain

**Objectif** : L'OS s'enrichit seul depuis Internet via APK

### Etape 2.1 — apk update en Ring 3
- Executer `/sbin/apk update` avec reseau VirtIO actif
- **Marqueur CI** : `OK:` + nombre de paquets

### Etape 2.2 — apk add curl
- Telecharger et installer un vrai paquet depuis les depots Alpine
- **Marqueur CI** : `Installing curl` ou equivalent

---

## PHASE 3 — Inference LLM Bare-Metal

**Objectif** : Le cerveau de l'agent pense vraiment avec des poids reels

### Etape 3.1 — GGUF Parser (Push 2)
- Parser complet GGUF v3 : header, KV pairs, tensor info, data_offset
- **Marqueur CI** : `[LLM] GGUF v3 tensors=N` + `[LLM] LLM-LOAD-OK`
- **Diff attendu** : ~450 lignes dans `gguf.rs` + ~20 lignes dans `limine_entry.rs`

### Etape 3.2 — Forward Pass + Vocab (Push 3)
- Charger les poids reels (Q4_0 dequantization), extraire le vocabulaire
- Tokeniser "The capital of France is", forward pass 4 layers, greedy sampling
- **Marqueur CI** : `[LLM] Generated: {texte}`
- **Diff attendu** : ~120 lignes dans `inference.rs` + ~100 lignes dans `limine_entry.rs`

### Etape 3.3 — LLM en Ring 3 (futur)
- `agent_inference.elf` charge GGUF via mmap, utilise AVX2/SSE
- Execution complete en espace utilisateur

---

## PHASE 4 — Boucle Agentique ReAct

**Objectif** : L'IA compile un programme C et l'execute

### Etape 4.1 — Agent genere du code
- LLM recoit un prompt, genere du code Python ou C
- Ecrit le fichier sur ext2

### Etape 4.2 — Agent execute et observe
- fork() + execve() du programme genere
- Capture stdout, analyse le resultat
- Boucle ReAct : Think -> Act -> Observe -> Think

---

## Regles de CI

1. **1 push = 1 objectif** — jamais de mega-commit multi-feature
2. **On ne passe pas a la phase suivante** tant que la precedente ne crache pas le log attendu
3. **On n'attend pas le CI** pour coder le push suivant, mais on recolte les logs avant de merger
4. **Correction ciblee** : si un push echoue, on corrige UNIQUEMENT ce push
5. **Pas de pretention sans preuve** : sans marqueur CI, le feature est "NON PROUVE"

---

## Historique des Pushes

| Push | Commit  | Objectif              | CI Resultat | Marqueur |
|------|---------|-----------------------|-------------|----------|
| 0    | eadc489 | IRETQ fix (base)      | SUCCESS     | 1764, 8/9 tests |
| 1    | 3414a24 | APK Ring 3 seul       | SUCCESS     | 1764, APK PID=2 loaded+launched |
| 2    | a932d81 | GGUF parser seul      | FAILURE     | PF-KERN-FIX loop (kernel trop gros) |
| 3    | f220ca7 | Tokenize + from_gguf  | FAILURE     | Meme cause que Push 2 |

## Diagnostic Session 9 (2026-05-14)

### Push 1 — SUCCESS
- Python3 `1764` confirme
- APK PID=2 se lance, charge libcrypto.so.3, puis SIGSEGV (addr=0x62)
- Le multi-process fonctionne : Python3 exit -> launch_next -> APK demarre

### Push 2/3 — FAILURE : Kernel binary trop gros
- Ajouter ~450 lignes de GGUF parser pousse le kernel au-dela des pages mappees par Limine
- PF-KERN-FIX tente de mapper la page mais calcule une physique incorrecte -> boucle infinie
- Adresse fautive : 0xFFFFFFFF802BF... (PML4[511], kernel image region)
- `KERN_SIZE_PAGES = 1536` (6 MiB) dans ensure_kernel_pages_mapped() est TROP PETIT

---

## Session 10 — Corrections et F3 Forward Pass (2026-05-14)

### Fix 1: KERN_SIZE_PAGES 1536 → 2560 (commit 878cac5)
- **Root cause resolue**: `ensure_kernel_pages_mapped()` ne mappait que 6 MiB
- Kernel binaire avec LLM subsystem depasse 6 MiB → page fault boucle infinie
- Fix: 2560 pages = 10 MiB, couvre kernel + gguf.rs (573 lignes) + inference.rs (700 lignes)

### Fix 2: HEAP_SIZE 64 → 256 MB (commit 5011e65)
- token_embd dequantise Q4_0→f32 = 576×49152×4 = 112 MB
- Ancien heap 64 MB = OOM garanti
- Nouveau heap 256 MB laisse ~260 MB pour kernel + page tables + ring 3

### Fix 3: Tied output weights (commit 5011e65)
- SmolLM2-135M n'a pas de output.weight (embeddings liees)
- Ancien code: clone de token_embedding = +112 MB inutile
- Nouveau: flag `tied_output` dans TransformerWeights, forward() reutilise token_embedding
- Economie: 112 MB
- Pic memoire during from_gguf(): 78 MB file + 112 MB embd + 28 MB layers = 218 MB < 256 MB

### Push 4: F3 Forward Pass (commit 908eced)
Pipeline complet en 6 phases:
1. Parse GGUF v3 metadata (4 MB read)
2. Tokenize "The capital of France is" via BPE greedy (49152 tokens vocab)
3. Lire fichier GGUF complet (jusqu'a 100 MB) pour tensor data
4. `TransformerWeights::from_gguf()` — dequantize Q4_0 → f32, 2 layers max
5. `forward()` — passe avant LLaMA: RMSNorm→QKV→RoPE→GQA→SwiGLU→residual
6. `sample_greedy()` × 5 tokens → decode via vocab → `[LLM] Generated: <text>`

### Pushes Session 10

| Push | Commit  | Objectif                  | Fichiers |
|------|---------|---------------------------|----------|
| 4    | 878cac5 | KERN_SIZE_PAGES fix       | elf/mod.rs |
| 5    | 908eced | F3 forward pass           | limine_entry.rs |
| 6    | 5011e65 | Heap 256 MB + tied weights | heap.rs, inference.rs |

### CI attendu (squashed)
- F1: `1764` + `APK-RING3-OK` ou PID 2 loaded
- F2: `[LLM] GGUF v3 tensors=272` + `[LLM] Config:` + `[LLM] LLM-LOAD-OK`
- F3: `[LLM] TOKENIZE-OK` + `[LLM] Generated: <text>`
- **Note**: Si download GGUF echoue dans CI → `[LLM] No GGUF model on disk`, F2/F3 skipped
