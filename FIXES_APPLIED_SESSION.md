# Corrections Appliquées - Session de Débogage

## Problème Initial
L'écran QEMU reste noir et le terminal se termine immédiatement après le boot.

## Analyse
1. Le terminal (agent_visual_term) a un safety valve de 50 000 itérations sans input clavier
2. Les processus sont créés avec un `preempt_state` sauvegardé AVANT leur démarrage
3. Quand un processus fait un SIGSEGV, le scheduler reprend le terminal à un `rip` corrompu
4. Le terminal ne démarre jamais correctement et sort immédiatement

## Corrections Appliquées

### Fix #1: Augmentation du Safety Valve
**Fichier:** `userspace/agent_visual_term/src/main.rs`  
**Ligne:** 65  
**Changement:** `MAX_IDLE_LOOPS: u64 = 50000` → `MAX_IDLE_LOOPS: u64 = 5000000`  
**Raison:** Donner plus de temps au terminal pour attendre l'input clavier

### Fix #2: Suppression des save_preempt_state Prématurés
**Fichier:** `kernel/src/main.rs`  
**Lignes:** 1798-1800, 1820-1822, 1844-1846, 1882-1884  
**Changement:** Suppression des appels à `process::save_preempt_state()` lors de la création des processus  
**Raison:** Le `preempt_state` ne doit être sauvegardé QUE par le timer IRQ quand un processus est réellement préempté, pas lors de sa création

**Processus affectés:**
- agent_visual_term (PID 11)
- agent_orchestrator (PID 12)
- agent_llama_core (PID 13)
- agent_llm_chat (PID 14)

## Résultat Attendu
- Le terminal devrait démarrer correctement depuis son `entry_point`
- L'interface graphique devrait s'afficher (barre bleue, texte blanc)
- Le prompt `aetherion:~$` devrait apparaître
- Le clavier devrait être lisible (une fois les autres bugs corrigés)

## Problèmes Restants (CURRENT_ISSUES.md)
1. `sys_read(fd=0)` busy-loop sans context switch réel
2. Page fault handler ne fait pas de context switch
3. Terminal ne lit jamais le clavier via `sys_read()` (boucle de test temporaire)

## Prochaines Étapes
1. Vérifier que le terminal s'affiche correctement
2. Implémenter un vrai context switch dans `schedule_next()`
3. Ajouter états BLOCKED/READY aux processus
4. Implémenter file d'attente de processus bloqués sur stdin
5. Faire réveiller les processus bloqués par l'IRQ clavier
