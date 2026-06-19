#!/bin/bash
export PATH="$HOME/.cargo/bin:$HOME/.rustup/toolchains/nightly-2026-04-21-x86_64-unknown-linux-gnu/bin:$PATH"
cd /home/ubuntu/webapp/MORNINGSTAR/AetherionOS

echo "=== PHASE FINALE : Validation Ring 3 ==="

# 1. Verifier le binaire
if [ -f "target/x86_64-unknown-none/release/aetherion-kernel" ]; then
    SIZE=$(ls -lh target/x86_64-unknown-none/release/aetherion-kernel | awk '{print $5}')
    echo "Kernel binaire detecte : $SIZE"
else
    echo "ERREUR : Kernel non trouve. Tentative de build rapide..."
    CARGO_BUILD_JOBS=4 cargo build -p aetherion-kernel --target x86_64-unknown-none --release --features limine
fi

# 2. Rebuild ISO
echo "Creation de l'ISO..."
ISO_DIR="target/limine-iso"
rm -rf "$ISO_DIR"
mkdir -p "$ISO_DIR/boot/limine" "$ISO_DIR/EFI/BOOT"
cp target/x86_64-unknown-none/release/aetherion-kernel "$ISO_DIR/boot/aetherion-kernel"
cp limine.conf "$ISO_DIR/boot/limine/limine.conf"
cp third_party/limine/bin/limine-bios-cd.bin "$ISO_DIR/boot/limine/"
cp third_party/limine/bin/limine-bios.sys "$ISO_DIR/boot/limine/"
cp third_party/limine/bin/BOOTX64.EFI "$ISO_DIR/EFI/BOOT/"

xorriso -as mkisofs \
    -b boot/limine/limine-bios-cd.bin \
    -no-emul-boot -boot-load-size 4 -boot-info-table \
    --efi-boot EFI/BOOT/BOOTX64.EFI \
    -efi-boot-part --efi-boot-image \
    --protective-msdos-label \
    "$ISO_DIR" -o target/aetherion-os.iso 2>&1 | tail -n 3

third_party/limine/bin/limine bios-install target/aetherion-os.iso

# 3. Test de Boot et Ring 3 (Automatique)
echo "Lancement du test de boot (90s)..."
(sleep 40; printf 'exec /bin/agent_inference\r'; sleep 45) | \
timeout 100 qemu-system-x86_64 \
    -cdrom target/aetherion-os.iso \
    -drive file=disk.img,format=raw,if=virtio \
    -m 4G -smp 4 -nographic -no-reboot \
    -serial mon:stdio \
    2>/dev/null > /tmp/ring3_final.log

echo "=== RESULTATS DU TEST ==="
grep -E "AetherionOS|prompt|EXEC.*PID|AVX2|Model tier|Layer cap|Generated|LLM-INFERENCE-OK|POOL.*EXHAUST|#PF|PANIC" /tmp/ring3_final.log

echo "=== VERDICT ==="
if grep -q "LLM-INFERENCE-OK" /tmp/ring3_final.log; then
    echo "VERDICT: SUCCESS - LLM Ring 3 operationnel !"
elif grep -q "Generated" /tmp/ring3_final.log; then
    echo "VERDICT: PARTIAL - Inference commencee mais interrompue"
elif grep -q "EXEC.*PID" /tmp/ring3_final.log; then
    echo "VERDICT: RING3_START - L'agent a demarre mais a bloque"
else
    echo "VERDICT: FAIL - Echec du demarrage"
fi
