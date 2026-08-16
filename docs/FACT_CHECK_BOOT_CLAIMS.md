# AetherionOS — Vérification factuelle des affirmations de boot

> Document de vérité terrain. Objectif : séparer ce qui est **prouvé par le code +
> build** de ce qui est seulement **présent dans le binaire (strings)** ou
> **affirmé dans un rapport** sans exécution end-to-end. Établi après relecture
> critique des logs de boot et inspection directe des sources.

Date : 2026-06-23
Méthode : lecture des sources `kernel/src/**`, build réel
(`cargo build -p aetherion-kernel --target x86_64-unknown-none`), inspection des
tailles de fichiers sur disque.

---

## 1. Build — FAIT VÉRIFIÉ ✅

Le kernel compile proprement sur `x86_64-unknown-none`, toolchain
`nightly-2026-04-21`, en ~8-13 s. Aucune erreur, uniquement 9 warnings
(variables inutilisées dans `syscall.rs`). Ce n'est PAS une affirmation de log :
c'est reproductible localement.

## 2. Modèle LLM — l'anomalie « 6 KB » EXPLIQUÉE ✅

L'audit a relevé que le boot logge un modèle de ~6 KB (`include_bytes!` du
placeholder embarqué) tout en prétendant « LLM model loaded ». Vérité terrain :

| Fichier | Taille réelle | Où |
|---|---|---|
| placeholder embarqué (`MINI_MODEL_GGUF`) | ~6 KB | dans le binaire kernel |
| `smollm2-135m-q4_0.gguf` | **173 MB** | sur le disque hôte / Ext2 |
| `smollm2-135m-instruct-q8_0.gguf` | **144 MB** | sur le disque hôte |

→ Les **vrais poids existent** (≈317 MB cumulés). Ils ne sont PAS embarqués dans
l'ISO (normal : on n'inclut pas 173 MB dans un kernel). Ils ne sont chargés QUE si
VirtIO-BLK est attaché et Ext2 monté. Le boot log « 6 KB » était donc trompeur.

**Correctif appliqué (commit 6c01abb)** : le boot cherche désormais le vrai modèle
sous 3 noms candidats sur Ext2 et, à défaut, logge explicitement un `[WARN] …
embedded PLACEHOLDER … real model NOT loaded (Ext2/VirtIO-BLK absent)`. Plus de
faux « model loaded ».

## 3. PLOGIT — RÉEL dans le code, NON exécuté au boot ⚠️

- `kernel/src/llm/parallel.rs` (348 lignes) : argmax parallèle sur logits quantifiés
  Q8, chemin `argmax_range_q8_avx2` + fallback scalaire, partition par worker.
  C'est du **vrai code**, pas un stub.
- MAIS : les marqueurs `PLOGIT …` n'apparaissent au runtime QUE si une inférence est
  lancée. Au boot, aucune inférence end-to-end n'est déclenchée automatiquement.
- Verdict : « PLOGIT operational » est **prématuré**. Correct serait : « PLOGIT
  implémenté, à valider par une inférence réelle ».

## 4. Compute backend — RÉEL, générique ⚠️

- `kernel/src/compute/mod.rs` : trait `ComputeBackend`, `CpuBackend` (scalar +
  AVX2 optionnel), sélection unique au boot (phase 1c/12).
- Le log `cpu-scalar (gpu=false, quantized=true)` est exact. `quantized=true` ne
  garantit PAS « INT8 » — l'audit avait raison, c'est une extrapolation. Aucune
  inférence n'a validé le backend end-to-end.

## 5. Moteur d'inférence — SUBSTANTIEL ✅ (exécution non prouvée ⚠️)

- `kernel/src/llm/inference.rs` = **77 KB**, `matmul.rs` = 16 KB,
  `gguf.rs` = 19 KB (parser GGUF). Code dense et réel.
- `agent_inference` (ELF Ring 3) embarqué via `include_bytes!` et monté dans le
  VFS — mais **monté ≠ exécuté**. L'audit avait raison : la présence dans le VFS
  ne prouve pas le fonctionnement.

## 6. APK / réseau — code RÉEL, déjà avancé ✅

Contrairement au plan « à faire », c'est **déjà implémenté** :
- `kernel/src/fs/apk.rs` (461 lignes) : `parse_apkindex`, `extract_apkindex_text`
  (gunzip+untar), `apk_update`, `find_package`, `apk_add`, `install_apk_from_disk`.
  Aucun `todo!`/`unimplemented!`.
- `kernel/src/fs/tar.rs` : gunzip + untar.
- `kernel/src/net/http.rs` : client HTTP (wget câblé au shell, commit d741467).
- Limite réelle : pas de VirtIO-Net dans l'environnement QEMU courant → réseau
  désactivé au runtime. C'est une contrainte d'ENV, pas un trou de code.

## 7. DRM / framebuffer — nodes RÉELS, pas de rendu prouvé ⚠️

- `ade384d` : `/dev/dri/card0` + `/dev/dri/renderD128`,
  `DRM_IOCTL_VERSION/GET_CAP/SET_VERSION` (dans `vfs.rs` + `syscall.rs`).
- Framebuffer 1280x800@32bpp fourni par **Limine** (bootloader), pas par
  `aether_swrast`. L'audit avait raison : `aether_swrast` est dans les strings,
  pas actif au boot. Aucun rendu graphique end-to-end prouvé.

---

## Tableau de vérité consolidé

| Affirmation rapport | Réalité terrain | Statut |
|---|---|---|
| Kernel build OK | Build reproductible, 0 erreur | ✅ VRAI |
| « LLM model loaded » (6 KB) | placeholder embarqué ; vrai modèle 173 MB sur Ext2 | ⚠️ corrigé (6c01abb) |
| PLOGIT operational | code réel (parallel.rs) ; non exécuté au boot | ⚠️ prématuré |
| aether_swrast active | strings only ; framebuffer = Limine | ❌ faux |
| Inference engine ready | inference.rs 77KB réel ; jamais exécuté au boot | ⚠️ non prouvé |
| APK parser à faire | DÉJÀ implémenté (apk.rs 461 l.) | ✅ mieux que prévu |
| Réseau « pas connecté » | HTTP/wget câblés ; VirtIO-Net absent en QEMU | ⚠️ contrainte env |
| DRM nodes | réels (card0/renderD128 + ioctls) ; pas de rendu | ⚠️ partiel |

## Conclusion honnête

Le boot **réussit réellement** (build prouvé, kernel mature : KPTI, scheduler,
VFS, IPC). Le code LLM/APK/DRM est **substantiel et réel**, pas des stubs. MAIS la
chaîne **inférence end-to-end n'est PAS prouvée au boot** (agents montés non
exécutés, modèle réel absent sans VirtIO-BLK, PLOGIT non déclenché). Score honnête :
**boot 9/10, fonctionnalités LLM validées ≈ 4/10** (code prêt, exécution à prouver).

## Prochain incrément réaliste (le plus court chemin vers une preuve)

Le vrai bloquant n'est ni APK ni Mesa : c'est **prouver UNE inférence LLM réelle**.
1. Attacher VirtIO-BLK + disk.img (Ext2 avec le vrai GGUF 173 MB) au boot QEMU.
2. Vérifier que le boot logge « Real GGUF model found on Ext2 (…) » (et non le WARN
   placeholder).
3. Lancer `agent_inference` (ou la fonction `run_kernel_llm_benchmark`) sur un prompt
   court et capturer les marqueurs `PLOGIT-DONE argmax=tok…` dans le log série.
4. Si ça produit ne serait-ce qu'UN token correct → la chaîne compute→GGUF→PLOGIT
   est prouvée end-to-end, ce qui débloque toute la crédibilité LLM du projet.

C'est plus court et plus décisif que le port Mesa softpipe (semaines) ou le solveur
de dépendances APK (déjà partiellement là).
