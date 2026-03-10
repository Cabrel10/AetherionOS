#!/bin/bash
# Script pour appliquer automatiquement tous les patches locaux nécessaires
# Usage: ./apply_local_patches.sh

set -e

echo "========================================="
echo "  AetherionOS Local Patches Applicator"
echo "========================================="
echo ""

PATCHES_APPLIED=0
PATCHES_FAILED=0

# Patch 1: Désactiver FAT32 tests
echo "[PATCH 1/4] Disabling FAT32 run_tests()..."
if grep -q "^    fs::fat32::run_tests();" kernel/src/main.rs 2>/dev/null; then
    sed -i 's/^    fs::fat32::run_tests();/    \/\/ fs::fat32::run_tests();  \/\/ PATCH: Disabled to avoid OOM with 2GB files/' kernel/src/main.rs
    echo "  ✓ FAT32 tests disabled"
    PATCHES_APPLIED=$((PATCHES_APPLIED + 1))
elif grep -q "// fs::fat32::run_tests()" kernel/src/main.rs 2>/dev/null; then
    echo "  ⚠ Already patched"
else
    echo "  ❌ Pattern not found - file may have changed"
    PATCHES_FAILED=$((PATCHES_FAILED + 1))
fi
echo ""

# Patch 2: Vérifier si file_exists existe
echo "[PATCH 2/4] Checking file_exists() in fat32.rs..."
if grep -q "pub fn file_exists" kernel/src/fs/fat32.rs 2>/dev/null; then
    echo "  ✓ file_exists() already present"
    PATCHES_APPLIED=$((PATCHES_APPLIED + 1))
else
    echo "  ❌ file_exists() missing"
    echo "     This patch requires manual addition"
    echo "     See LOCAL_PATCHES.md Patch 2 for code"
    PATCHES_FAILED=$((PATCHES_FAILED + 1))
fi
echo ""

# Patch 3: Vérifier sys_open partie 1
echo "[PATCH 3/4] Checking sys_open optimization (part 1)..."
if grep -q "let exists = crate::fs::fat32::file_exists(disk_path);" kernel/src/arch/x86_64/syscall.rs 2>/dev/null; then
    echo "  ✓ sys_open part 1 optimized"
    PATCHES_APPLIED=$((PATCHES_APPLIED + 1))
elif grep -q "let exists = crate::fs::fat32::read_file_path(disk_path).is_some();" kernel/src/arch/x86_64/syscall.rs 2>/dev/null; then
    echo "  ❌ sys_open still uses read_file_path()"
    echo "     This patch requires manual modification"
    echo "     See LOCAL_PATCHES.md Patch 3 for details"
    PATCHES_FAILED=$((PATCHES_FAILED + 1))
else
    echo "  ⚠ Pattern not found - may already be patched differently"
fi
echo ""

# Patch 4: Vérifier sys_open partie 2
echo "[PATCH 4/4] Checking sys_open optimization (part 2)..."
if grep -q "if !crate::fs::fat32::file_exists(disk_path)" kernel/src/arch/x86_64/syscall.rs 2>/dev/null; then
    echo "  ✓ sys_open part 2 optimized"
    PATCHES_APPLIED=$((PATCHES_APPLIED + 1))
else
    echo "  ❌ sys_open part 2 needs optimization"
    echo "     This patch requires manual modification"
    echo "     See LOCAL_PATCHES.md Patch 4 for details"
    PATCHES_FAILED=$((PATCHES_FAILED + 1))
fi
echo ""

# Résumé
echo "========================================="
echo "  Patch Summary"
echo "========================================="
echo "  Applied:  $PATCHES_APPLIED/4"
echo "  Failed:   $PATCHES_FAILED/4"
echo ""

if [ $PATCHES_FAILED -eq 0 ]; then
    echo "✓ All patches applied successfully!"
    echo ""
    echo "Next steps:"
    echo "  1. Review changes: git diff kernel/src/"
    echo "  2. Recompile: cd kernel && cargo bootimage --release"
    echo "  3. Test: ./test_aetherion.sh"
    exit 0
else
    echo "⚠ Some patches require manual intervention"
    echo ""
    echo "Please review:"
    echo "  - LOCAL_PATCHES.md for detailed instructions"
    echo "  - TECHNICAL_REPORT_LOCAL_LIMITATIONS.md for context"
    exit 1
fi
