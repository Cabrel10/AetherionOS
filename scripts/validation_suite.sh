#!/bin/bash
# SESSION OS SUITE — cargo check + rebuild ISO + prouver Ring 3 

export PATH="$HOME/.cargo/bin:$HOME/.rustup/toolchains/nightly-2026-04-21-x86_64-unknown-linux-gnu/bin:$PATH"
cd /home/ubuntu/webapp/MORNINGSTAR/AetherionOS

echo "=== ÉTAPE 1 : Kernel cargo check ==="
CARGO_BUILD_JOBS=4 cargo check -p aetherion-kernel \
    --target x86_64-unknown-none \
    --features limine 2>&1 | tail -10
echo "cargo check RC=$?"

echo ""
echo "=== ÉTAPE 2 : Push GitHub (SKIP) ==="
# git remote set-url origin \
#     https://TOKEN@github.com/Cabrel10/AetherionOS.git
# git log --oneline -3
# git push origin genspark_ai_developer --force 2>&1
echo "Push skipped."

echo ""
echo "=== ÉTAPE 3 : Rebuild kernel avec nouveaux binaires ==="
CARGO_BUILD_JOBS=4 cargo build -p aetherion-kernel \
    --target x86_64-unknown-none \
    --release \
    --features limine 2>&1 | tail -10
echo "Build RC=$?"

echo ""
echo "=== ÉTAPE 4 : Rebuild ISO ==="
ISO_DIR="target/limine-iso"
rm -rf "$ISO_DIR"
mkdir -p "$ISO_DIR/boot/limine" "$ISO_DIR/EFI/BOOT"
cp target/x86_64-unknown-none/release/aetherion-kernel \
    "$ISO_DIR/boot/aetherion-kernel"
cp limine.conf "$ISO_DIR/boot/limine/limine.conf"
cp third_party/limine/bin/limine-bios-cd.bin "$ISO_DIR/boot/limine/"
cp third_party/limine/bin/limine-bios.sys    "$ISO_DIR/boot/limine/"
cp third_party/limine/bin/BOOTX64.EFI        "$ISO_DIR/EFI/BOOT/"
rm -f target/aetherion-os.iso
xorriso -as mkisofs \
    -b boot/limine/limine-bios-cd.bin \
    -no-emul-boot -boot-load-size 4 -boot-info-table \
    --efi-boot EFI/BOOT/BOOTX64.EFI \
    -efi-boot-part --efi-boot-image \
    --protective-msdos-label \
    "$ISO_DIR" -o target/aetherion-os.iso 2>&1 | tail -3
third_party/limine/bin/limine bios-install \
    target/aetherion-os.iso 2>&1 | tail -2
echo "ISO: $(ls -lh target/aetherion-os.iso | awk '{print $5}')"

echo ""
echo "=== ÉTAPE 5 : Boot test (45s) ==="
timeout 45 qemu-system-x86_64 \
    -cdrom target/aetherion-os.iso \
    -drive file=disk.img,format=raw,if=virtio \
    -m 4G -smp 4 -nographic -no-reboot \
    -serial mon:stdio \
    2>/dev/null > /tmp/boot_test.log
echo "--- Marqueurs boot ---"
grep -E "5a-RELOC|HEAP.*Ready|CI-TEST.*PASS|LLM-BENCH|prompt|\\\$" \
    /tmp/boot_test.log | tail -20

echo ""
echo "=== ÉTAPE 6 : Ring 3 exec agent_inference (90s) ==="
(sleep 35; printf 'exec /bin/agent_inference\r'; sleep 50) | \
timeout 90 qemu-system-x86_64 \
    -cdrom target/aetherion-os.iso \
    -drive file=disk.img,format=raw,if=virtio \
    -m 4G -smp 4 -nographic -no-reboot \
    -serial mon:stdio \
    2>/dev/null > /tmp/ring3_test.log
echo "--- Marqueurs Ring 3 ---"
grep -E "EXEC.*PID|AVX2|Model tier|Layer cap|Generated|LLM-INFERENCE-OK|PERF.*tok|POOL.*EXHAUST|SIGSEGV|PANIC|#PF" \
    /tmp/ring3_test.log
echo "--- 20 dernières lignes ---"
tail -20 /tmp/ring3_test.log

echo ""
echo "=============================="
echo "VERDICT FINAL"
echo "=============================="
BOOT_OK=$(grep -c "AetherionOS\|\\\$" /tmp/boot_test.log 2>/dev/null || echo 0)
RING3=$(grep -c "EXEC.*PID" /tmp/ring3_test.log 2>/dev/null || echo 0)
LLM=$(grep -c "LLM-INFERENCE-OK" /tmp/ring3_test.log 2>/dev/null || echo 0)
GENERATED=$(grep -c "Generated:" /tmp/ring3_test.log 2>/dev/null || echo 0)
POOL=$(grep -c "POOL.*EXHAUST" /tmp/ring3_test.log 2>/dev/null || echo 0)

echo "Boot prompt     : $BOOT_OK"
echo "Ring 3 PID      : $RING3"
echo "LLM-INFERENCE-OK: $LLM"
echo "Generated token : $GENERATED"
echo "Pool exhausted  : $POOL"

if [ "$LLM" -gt 0 ]; then
    echo "SUCCESS : [LLM-INFERENCE-OK] PROUVE EN RING 3"
elif [ "$GENERATED" -gt 0 ]; then
    echo "PARTIEL : token genere mais pas termine"
elif [ "$POOL" -gt 0 ]; then
    echo "BLOQUE : ELF pool epuise - besoin fix pool"
elif [ "$RING3" -gt 0 ]; then
    echo "PARTIEL : Ring 3 demarre mais LLM bloque"
else
    echo "ECHEC : Ring 3 ne demarre pas"
fi

echo ""
echo "=== LOGS COMPLETS POUR OPUS SI BESOIN ==="
echo "=== ring3_test.log ===" && cat /tmp/ring3_test.log
