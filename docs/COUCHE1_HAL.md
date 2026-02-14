# Aetherion OS - Couche 1 HAL Documentation

## Version: v0.1.0-HAL

---

## 1. Vue d'ensemble

La **Couche 1 HAL** constitue le fondement d'Aetherion OS avec bootloader 0.11.7, gestion mémoire, et sécurité TPM.

---

## 2. Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    COUCHE 1 - HAL                           │
├─────────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐  │
│  │ Bootloader  │  │   Memory    │  │    Security         │  │
│  │  0.11.7     │  │  Frame/Heap │  │  TPM 2.0 + PCR      │  │
│  └─────────────┘  └─────────────┘  └─────────────────────┘  │
├─────────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐  │
│  │    CPU      │  │  Interrupts │  │   Output            │  │
│  │    GDT      │  │    IDT      │  │   Serial/VGA        │  │
│  │    IDT      │  │   Timer     │  │                     │  │
│  └─────────────┘  └─────────────┘  └─────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
```

---

## 3. Configuration Bootloader

```toml
[dependencies]
bootloader = { version = "0.11.7", features = ["bios", "uefi"] }
bootloader-locator = "0.0.4"  # build-dependency
```

```rust
pub static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.kernel_stack_size = 64 * 1024;
    config
};

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);
```

---

## 4. Tests HAL

| Test | Description | Status |
|------|-------------|--------|
| test_gdt_load | Chargement GDT | PASS |
| test_idt_load | Chargement IDT | PASS |
| test_timer_interrupt | Timer IRQ | PASS |
| test_tpm_detection | Detection TPM | PASS/FAIL |
| test_pcr_measurement | Mesure PCR0 | PASS |

---

## 5. Performance

| Métrique | Valeur |
|----------|--------|
| Boot time | < 500 ms |
| Kernel size | ~50 KB |
| IRQ latency | < 10 µs |

---

## 6. Commandes

```bash
# Build
cd kernel && cargo build --release

# Image bootable
cargo install bootimage --version 0.11.4
cargo bootimage --release

# QEMU test
qemu-system-x86_64 -drive format=raw,file=target/release/bootimage-aetherion_os.bin -serial stdio -display none

# QEMU avec TPM
qemu-system-x86_64 ... -tpmdev emulator,id=tpm0 -device tpm-tis,tpmdev=tpm0
```

---

**Version**: v0.1.0-HAL  
**Auteur**: Cabrel Foka <cabrel@aetherion.dev>  
**Licence**: MIT
