# AetherionOS — Diagnostic SMP sous Limine (multi-core)

> Généré le 2026-06-22. Test : `qemu-system-x86_64 -smp 4 -cpu Haswell,+avx2,+fma`
> ISO release fraîche (binaire compilé avec le SMP câblé dans le chemin Limine).

## 1. Progrès obtenu

Le câblage de l'étape `[9f/12] ACPI + Local APIC + SMP bring-up` dans
`limine_entry.rs` **fonctionne** : l'étape s'exécute désormais (avant, elle
n'était appelée que depuis `main.rs`, chemin bootloader legacy non utilisé).

Logs réels (`/tmp/smp_boot.log`) :

```
[9f/12] ACPI + Local APIC + SMP bring-up...
[ACPI] MADT: Local APIC address = 0xFEE00000
[ACPI] MADT Parsed: Found 4 CPUs (APIC IDs: 0, 1, 2, 3)
[APIC] Local APIC initialized on BSP (ID=0)
[SMP] Jalon 103: Sequential AP Bootstrap with Mailbox
[SMP] BSP CR3 (PML4): 0x0000000000059000
[SMP] ERROR: PML4[0] not present! Cannot identity-map trampoline.
[SMP] CPUs online: 1
[SMP] ACPI reported: 4 CPU(s)
```

- ACPI/MADT détecte correctement **4 CPUs** (APIC IDs 0,1,2,3).
- Le BSP s'initialise.
- **Aucun triple-fault / panic** : le kernel dégrade proprement vers 1 CPU et
  continue (le LLM se charge ensuite normalement). Bon comportement de sûreté.

## 2. Cause racine (factuelle, lue dans le code)

`apic.rs:303-310`, `wake_application_processors()` :

```rust
// Step 2: Verify PML4[0] identity mapping
let pml4_virt = (phys_offset + bsp_cr3) as *const u64;
let pml4_0 = pml4_virt.read_volatile();
if pml4_0 & 0x01 == 0 {
    crate::serial_println!("[SMP] ERROR: PML4[0] not present! ...");
    return;                       // ← on s'arrête ici sous Limine
}
```

Le commentaire d'en-tête (`apic.rs:23-24`) l'avoue :

```
// accessible via PML4[0] which the bootloader already populates.
```

→ **C'était vrai pour le bootloader legacy (`bootloader_api`), qui identity-mappe
la mémoire basse.** Limine, lui, n'identity-mappe PAS le bas : il fournit un
HHDM (Higher Half Direct Map) à `PML4[256]` (`0xFFFF800000000000`). Donc
`PML4[0]` est **absent** sous Limine.

### Pourquoi le trampoline a besoin de PML4[0]

Le trampoline AP démarre en mode réel 16-bit à l'adresse **physique 0x8000**
(SIPI vector = `startup_page 0x08`). Il passe ensuite en mode protégé puis long
et **charge `bsp_cr3`** (CR3 partagé, `apic.rs:328-332`). À cet instant, le code
s'exécute encore aux **adresses physiques basses** (0x8000…). Pour que ces
adresses restent valides après le chargement de CR3, la table de pages doit
**identity-mapper** le bas de mémoire — c'est-à-dire que `PML4[0] → … → 0x8000`
doit exister. Sans cela, dès le `mov cr3`, l'AP ferait un page-fault → triple
fault. La vérification `PML4[0] present` est donc une **garde correcte** : sous
Limine, elle empêche un crash en refusant de réveiller les APs.

## 3. Ce qui MANQUE (le correctif)

Avant `wake_application_processors()`, il faut **créer l'identity-mapping du
premier ~2 MiB physique** (qui couvre 0x7000 stack temp + 0x8000 trampoline)
dans la table de pages active, ce qui peuple `PML4[0]`.

Le kernel possède déjà le pattern exact pour cela dans
`limine_entry.rs:525-596` : walk manuel PML4→PDPT→PD→PT, allocation des frames
manquantes via `mem_manager.frame_allocator.alloc_frame_kernel()`, écriture des
entrées avec l'offset HHDM, flush TLB. On réplique ce pattern pour mapper
`virt 0x0..0x200000 = phys 0x0..0x200000` (identity), flags `P|W` (kernel, pas
de bit User).

Contraintes de sûreté :
- Ne mapper QUE le bas nécessaire (0..2 MiB), pas plus.
- Vérifier que les frames 0x7000/0x8000 sont libres (Limine les place
  généralement en USABLE bas) — sinon le trampoline corromprait des données.
- Le mapping doit être créé dans **la même table CR3** que celle chargée par les
  APs (le CR3 courant du BSP), ce qui est le cas (CR3 partagé).

## 4. Statut

- Diagnostic : **terminé** (Cas : « SMP appelé, APs détectés, bring-up bloqué
  par PML4[0] manquant sous Limine »).
- Correctif : à implémenter (identity-map bas avant le réveil des APs).
- Règle utilisateur respectée : **on ne démarre PAS le parallélisme LLM par
  tête d'attention tant que les APs ne bootent pas proprement.** Ici, ils ne
  bootent pas encore → priorité = corriger le mapping bas, re-tester `-smp 4`,
  viser `[SMP] CPUs online: 4`.

## 5. RÉSULTAT APRÈS CORRECTIF (2026-06-22) — RÉSOLU ✓

Test `-smp 4` avec l'ISO release contenant le fix d'identity-map bas :

```
[SMP] Low 2 MiB identity-mapped (PML4[0] populated for AP trampoline)
[SMP] PML4[0] = 0x0000000018156003 (identity mapping OK)
[SMP] AP 1 (APIC ID 1) responded (sync=1, 0ms) — fully initialized ✓
[SMP] AP 2 (APIC ID 2) responded (sync=1, 0ms) — fully initialized ✓
[SMP] AP 3 (APIC ID 3) responded (sync=1, 0ms) — fully initialized ✓
[SMP] Results: 3 APs awakened, 4 total CPUs
[SMP] CPUs online: 4
```

- **4 CPUs en ligne** (BSP + 3 APs), tous répondent en 0 ms.
- **Aucun panic / triple-fault** (grep panic = 0). Le boot continue jusqu'au
  chargement LLM normalement.
- L'objectif mono-core → multi-core est **atteint**.

Prochaine étape (séparée) : vérifier que le multi-cœur **améliore réellement**
l'inférence LLM (cache, registres, jeu d'instructions, bande passante RAM/cache,
communication inter-cœurs, partage de tâches), pas juste "pour faire joujou".
