# Checklist de Diagnostic AetherionOS - Terminal Interactif

## État Actuel du Problème
- ❌ Le terminal ne répond pas au clavier
- ❌ Aucun texte n'apparaît à l'écran
- ❌ L'interface graphique ne s'affiche pas correctement

## 1. VÉRIFICATION DU BOOT KERNEL

### 1.1 Boot Sequence
```bash
# Commande de test
strings /tmp/qemu_log.log | grep -E "^\[.*\]" | head -50

# Logs attendus
[AETHERION] Kernel v1.9.0
[1/12] Loading GDT (R0+R3)...
[OK] GDT + TSS + Ring 3 selectors
[2/12] Loading IDT...
[OK] IDT with 20 handlers
[3/12] Initializing PIC...
[OK] PIC remapped (32-47)
[3.5/12] Initializing PS/2 controller...
[OK] PS/2 keyboard enabled (IRQ1)
```

**✓ À vérifier:**
- [ ] Le kernel boot sans panic
- [ ] GDT/IDT chargés correctement
- [ ] PIC initialisé (IRQ0 timer, IRQ1 keyboard)
- [ ] PS/2 controller répond (0xAA = reset OK)

### 1.2 Memory & Heap
```bash
# Logs attendus
[MEMORY] Frame allocator: 1046046 frames (4086 MB)
[HEAP] Initialized: 65536 KB at 0x444444440000
[TEST] All heap tests PASSED!
```

**✓ À vérifier:**
- [ ] Heap alloué et fonctionnel
- [ ] Tests d'allocation réussis

---

## 2. VÉRIFICATION DU FRAMEBUFFER

### 2.1 Initialisation VGA
```bash
# Logs attendus
[FB] Bochs VGA adapter detected (ID=0xB0C5)
[FB] Mode set: 1024x768x32 bpp
[FB] Framebuffer: phys=0xFD000000, 1024x768, stride=4096, size=3072 KB
[OK] Framebuffer: 1024x768 @ 0xFD000000 (3072 KB)
```

**✓ À vérifier:**
- [ ] VGA adapter détecté
- [ ] Mode graphique 1024x768x32 configuré
- [ ] Adresse physique framebuffer = 0xFD000000
- [ ] Taille = 3072 KB (1024*768*4)

### 2.2 Test Framebuffer depuis Kernel
```bash
# Ajouter ce test dans kernel/src/main.rs après init framebuffer
// Test: dessiner un pixel blanc en haut à gauche
unsafe {
    let fb_ptr = 0xFD000000 as *mut u32;
    *fb_ptr = 0x00FFFFFF; // Blanc
    serial_println!("[TEST] Pixel blanc écrit à FB+0");
}
```

**✓ À vérifier:**
- [ ] Le pixel blanc apparaît en haut à gauche de l'écran QEMU

---

## 3. VÉRIFICATION DU CLAVIER (KERNEL SIDE)

### 3.1 IRQ1 Keyboard Handler
```bash
# Logs attendus quand on tape une touche
[KEYBOARD] Scancode: 0x1C  # 'a' en PS/2 Set 2
[KBD] ASCII pushed: 0x61   # 'a' en ASCII
```

**✓ À vérifier:**
- [ ] IRQ1 fire quand on tape une touche
- [ ] Scancodes reçus (0x1C, 0x1B, 0x23, etc.)
- [ ] Conversion scancode → ASCII fonctionne
- [ ] `kbd_push_byte()` appelé

### 3.2 Keyboard Buffer
```bash
# Ajouter dans kernel/src/process/mod.rs::kbd_push_byte
pub fn kbd_push_byte(byte: u8) {
    crate::serial_println!("[KBD_BUF] Push byte: 0x{:02X} ('{}')", 
        byte, 
        if byte >= 0x20 && byte < 0x7F { byte as char } else { '?' }
    );
    KBD_BUFFER.lock().push(byte);
}
```

**✓ À vérifier:**
- [ ] Les bytes sont ajoutés au buffer
- [ ] Le buffer ne déborde pas (max 256 bytes)

### 3.3 Test sys_read(fd=0)
```bash
# Ajouter dans kernel/src/arch/x86_64/syscall.rs::sys_read
if fd == 0 {
    let bytes_read = crate::process::kbd_read(&mut temp_buf, max_read);
    crate::serial_println!("[SYSCALL] sys_read(0) returned {} bytes", bytes_read);
    if bytes_read > 0 {
        crate::serial_print!("[SYSCALL] Data: ");
        for i in 0..bytes_read {
            crate::serial_print!("0x{:02X} ", temp_buf[i]);
        }
        crate::serial_println!("");
    }
    // ...
}
```

**✓ À vérifier:**
- [ ] sys_read(0) retourne > 0 quand il y a des données
- [ ] Les bytes lus correspondent aux touches tapées

---

## 4. VÉRIFICATION DU TERMINAL (USERSPACE)

### 4.1 Lancement du Terminal
```bash
# Logs attendus
[J65] Loading /bin/agent_visual_term.elf...
[ELF] Load complete: entry=0x8000001657, stack=0x7FFFFFFFF000
[J65] Visual Terminal PID=14 registered (launching first)
[J65] IRETQ -> Ring 3: Interactive Terminal launches NOW!
[SYSCALL] GS bases reset: GS_BASE=0, KERNEL_GS_BASE=0x...
```

**✓ À vérifier:**
- [ ] ELF chargé sans erreur
- [ ] PID assigné (ex: 14)
- [ ] IRETQ vers Ring 3 exécuté
- [ ] GS bases configurées

### 4.2 Terminal Startup Sequence
```bash
# Logs attendus depuis agent_visual_term/src/main.rs
[J65] ========================================
[J65] Persistent Interactive Terminal v2.0
[J65] Mapping framebuffer... OK
[DBG] checkpoint 1
[J65] Drawing terminal UI... 
[DBG] checkpoint 2
OK
[DBG] checkpoint 3
[J65] Initializing terminal state...
[DBG] checkpoint 4
[DBG] checkpoint 5
[J65] Terminal state initialized
```

**✓ À vérifier:**
- [ ] Tous les checkpoints 1-20 apparaissent
- [ ] sys_fb_get_info() retourne fb_ok != 0
- [ ] draw_chrome() ne crashe pas
- [ ] Terminal::new() ne crashe pas

### 4.3 Terminal Main Loop
```bash
# Logs attendus
[DBG] checkpoint 19
[J65] Entering main loop (non-blocking sys_read + sys_yield)
[DBG] checkpoint 20 - entering loop
```

**✓ À vérifier:**
- [ ] Le terminal entre dans la boucle principale
- [ ] sys_read(0) est appelé en boucle
- [ ] sys_yield() est appelé après chaque itération

### 4.4 Terminal Input Processing
```bash
# Ajouter dans agent_visual_term/src/main.rs main loop
loop {
    let n = sys_read(0, &mut read_buf);
    if n > 0 {
        sys_write(1, b"[TERM] Read byte: ");
        sys_write(1, &read_buf[0..1]);
        sys_write(1, b"\n");
        // ... traitement normal
    }
}
```

**✓ À vérifier:**
- [ ] sys_read(0) retourne n > 0 quand on tape
- [ ] Les caractères lus sont corrects

---

## 5. VÉRIFICATION DU SCHEDULER PRÉEMPTIF

### 5.1 Timer IRQ0
```bash
# Ajouter dans kernel/src/arch/x86_64/idt.rs::timer_interrupt_handler
extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    static mut TICK_COUNT: u64 = 0;
    unsafe { 
        TICK_COUNT += 1;
        if TICK_COUNT % 100 == 0 {
            crate::serial_println!("[TIMER] Tick {}", TICK_COUNT);
        }
    }
    // ... reste du code
}
```

**✓ À vérifier:**
- [ ] Timer IRQ fire régulièrement (~100 Hz)
- [ ] tick_preemptive() est appelé

### 5.2 Context Switch
```bash
# Logs attendus
[SCHED] Context switch: PID 14 -> PID 11 (rip=0x8000001234)
[SCHED] Saved state: PID 14 rip=0x8000001657 rsp=0x7FFFFFFFF000
```

**✓ À vérifier:**
- [ ] Context switches se produisent
- [ ] RIP/RSP sauvegardés dans la plage userspace (0x8000000000-0x9000000000)
- [ ] Pas de corruption de contexte

---

## 6. VÉRIFICATION DES CRASHES

### 6.1 Page Faults
```bash
# Logs de crash
[SIGSEGV] PF addr=0x202 rip=VirtAddr(0x8000001859) code=CAUSED_BY_WRITE | USER_MODE
[SIGSEGV] PID 13 terminated (addr 0x202)
```

**✓ À analyser:**
- [ ] addr=0x202 → c'est RFLAGS, corruption de contexte
- [ ] addr=0x0 → null pointer dereference
- [ ] addr=0x51338C → accès mémoire invalide

### 6.2 Stack Trace
```bash
# Ajouter dans page_fault_handler
crate::serial_println!("[SIGSEGV] Stack trace:");
crate::serial_println!("  RIP: 0x{:X}", stack_frame.instruction_pointer.as_u64());
crate::serial_println!("  RSP: 0x{:X}", stack_frame.stack_pointer.as_u64());
crate::serial_println!("  RBP: 0x{:X}", /* lire RBP depuis registres */);
```

---

## 7. TESTS MINIMAUX À AJOUTER

### 7.1 Test Framebuffer Direct
```rust
// Dans kernel/src/main.rs après framebuffer init
fn test_framebuffer_direct() {
    serial_println!("[TEST] Drawing test pattern...");
    unsafe {
        let fb = 0xFD000000 as *mut u32;
        // Ligne bleue en haut
        for x in 0..1024 {
            *fb.add(x) = 0x001F6FEB;
        }
        // Ligne verte au milieu
        for x in 0..1024 {
            *fb.add(384 * 1024 + x) = 0x003FB950;
        }
        // Ligne rouge en bas
        for x in 0..1024 {
            *fb.add(767 * 1024 + x) = 0x00F85149;
        }
    }
    serial_println!("[TEST] Pattern drawn - check QEMU window");
}
```

### 7.2 Test Clavier Minimal
```rust
// Dans kernel, après PS/2 init
fn test_keyboard_echo() {
    serial_println!("[TEST] Keyboard echo test - type something...");
    for _ in 0..100 {
        let mut buf = [0u8; 1];
        let n = crate::process::kbd_read(&mut buf, 1);
        if n > 0 {
            serial_println!("[TEST] Key pressed: 0x{:02X} ('{}')", 
                buf[0], 
                if buf[0] >= 0x20 && buf[0] < 0x7F { buf[0] as char } else { '?' }
            );
        }
        // Attendre un peu
        for _ in 0..1000000 { unsafe { core::arch::asm!("nop"); } }
    }
}
```

---

## 8. COMMANDES DE DIAGNOSTIC

### 8.1 Capturer le Log Complet
```bash
qemu-system-x86_64 \
  -cpu qemu64 -smp 2 -m 4G \
  -drive format=raw,file=kernel/target/x86_64-unknown-none/release/bootimage-aetherion-kernel.bin \
  -drive format=raw,file=disk.img,if=virtio \
  -display gtk,zoom-to-fit=on -vga std \
  -serial file:/tmp/aetherion_full.log \
  -monitor stdio
```

### 8.2 Analyser les Logs
```bash
# Boot sequence
grep "^\[" /tmp/aetherion_full.log | head -100

# Keyboard events
grep -i "keyboard\|scancode\|kbd" /tmp/aetherion_full.log

# Terminal events
grep -i "j65\|terminal\|checkpoint" /tmp/aetherion_full.log

# Crashes
grep -i "sigsegv\|page fault\|panic" /tmp/aetherion_full.log

# Syscalls
grep "SYSCALL" /tmp/aetherion_full.log | tail -50
```

---

## 9. PRIORITÉS DE DÉBOGAGE

### Phase 1: Framebuffer
1. ✓ Vérifier que le framebuffer est mappé
2. ✓ Dessiner un pattern de test depuis le kernel
3. ✓ Confirmer que l'écran QEMU affiche quelque chose

### Phase 2: Clavier Kernel
1. ✓ Vérifier que IRQ1 fire
2. ✓ Logger tous les scancodes reçus
3. ✓ Vérifier la conversion scancode → ASCII
4. ✓ Tester kbd_read() depuis le kernel

### Phase 3: Terminal Userspace
1. ✓ Vérifier que le terminal démarre (checkpoints)
2. ✓ Vérifier que sys_fb_get_info() fonctionne
3. ✓ Vérifier que draw_chrome() dessine
4. ✓ Vérifier que la boucle principale tourne

### Phase 4: Integration
1. ✓ Vérifier que sys_read(0) retourne des données
2. ✓ Vérifier que le terminal affiche les caractères
3. ✓ Tester la saisie interactive

---

## 10. ÉTAT ACTUEL DES MODIFICATIONS

### Modifications Appliquées
- ✅ PS/2 Scancode Set 2 → Set 1 conversion
- ✅ Timer handler avec context switching préemptif
- ✅ Validation RIP userspace avant sauvegarde
- ✅ Safety valve désactivé (MAX_IDLE_LOOPS = u64::MAX)
- ✅ Simplification du keyboard handler (ignore 0xF0, 0xE0)
- ✅ Tous les println → sys_write dans le terminal

### Problèmes Connus
- ❌ Terminal ne produit aucun output visible
- ❌ Clavier ne répond pas
- ❌ Écran reste noir ou affiche du bruit
- ❌ Processus crashent avec page faults

### Prochaines Étapes
1. Ajouter les logs de diagnostic détaillés
2. Tester le framebuffer directement depuis le kernel
3. Tester le clavier directement depuis le kernel
4. Isoler le problème: kernel ou userspace?
