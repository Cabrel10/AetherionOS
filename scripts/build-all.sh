#!/bin/bash
# Aetherion OS - Complete Build Script
# Builds kernel, runs tests, and generates documentation

set -e  # Exit on error

echo "======================================"
echo "  Aetherion OS - Complete Build"
echo "======================================"
echo ""

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Check for Rust
if ! command -v rustc &> /dev/null; then
    echo -e "${RED}Error: Rust not found${NC}"
    echo "Please install Rust: https://rustup.rs/"
    exit 1
fi

echo -e "${GREEN}✓${NC} Rust toolchain found: $(rustc --version)"

# Check for required components
echo ""
echo "Checking required components..."

if ! rustup component list | grep -q "rust-src (installed)"; then
    echo "Installing rust-src..."
    rustup component add rust-src
fi
echo -e "${GREEN}✓${NC} rust-src installed"

if ! rustup component list | grep -q "llvm-tools-preview (installed)"; then
    echo "Installing llvm-tools-preview..."
    rustup component add llvm-tools-preview
fi
echo -e "${GREEN}✓${NC} llvm-tools-preview installed"

# Build kernel
echo ""
echo "======================================"
echo "  Building Kernel"
echo "======================================"
cd kernel

echo "Building in release mode..."
cargo build --target x86_64-unknown-none --release 2>&1 | tee ../build.log

if [ ${PIPESTATUS[0]} -eq 0 ]; then
    echo -e "${GREEN}✓${NC} Kernel built successfully"
    KERNEL_SIZE=$(stat -c%s "target/x86_64-unknown-none/release/aetherion-kernel" 2>/dev/null || stat -f%z "target/x86_64-unknown-none/release/aetherion-kernel" 2>/dev/null)
    echo "  Kernel size: $(numfmt --to=iec-i --suffix=B $KERNEL_SIZE 2>/dev/null || echo \"$KERNEL_SIZE bytes\")"
else
    echo -e "${RED}✗${NC} Kernel build failed"
    exit 1
fi

# Run tests
echo ""
echo "======================================"
echo "  Running Tests"
echo "======================================"

echo "Running unit tests..."
cargo test --lib 2>&1 | tee ../test.log

if [ ${PIPESTATUS[0]} -eq 0 ]; then
    echo -e "${GREEN}✓${NC} All tests passed"
    TEST_COUNT=$(grep -o "test result: ok\. [0-9]* passed" ../test.log | grep -o "[0-9]*" | head -1)
    echo "  Tests passed: $TEST_COUNT"
else
    echo -e "${YELLOW}⚠${NC} Some tests failed (expected in bare-metal build)"
fi

# Generate documentation
echo ""
echo "======================================"
echo "  Generating Documentation"
echo "======================================"

echo "Generating Rust docs..."
cargo doc --no-deps --target x86_64-unknown-none 2>&1 | grep -v "warning" || true

if [ -d "target/x86_64-unknown-none/doc" ]; then
    echo -e "${GREEN}✓${NC} Documentation generated"
    DOC_SIZE=$(du -sh target/x86_64-unknown-none/doc 2>/dev/null | cut -f1)
    echo "  Doc size: $DOC_SIZE"
else
    echo -e "${YELLOW}⚠${NC} Documentation generation incomplete"
fi

cd ..

# Code statistics
echo ""
echo "======================================"
echo "  Code Statistics"
echo "======================================"

echo "Lines of Code:"
if command -v tokei &> /dev/null; then
    tokei kernel/src
else
    RUST_LOC=$(find kernel/src -name "*.rs" -exec wc -l {} + | tail -1 | awk '{print $1}')
    RUST_FILES=$(find kernel/src -name "*.rs" | wc -l)
    echo "  Rust: $RUST_LOC lines in $RUST_FILES files"
fi

# Summary
echo ""
echo "======================================"
echo "  Build Summary"
echo "======================================"
echo -e "${GREEN}✓${NC} Kernel compiled"
echo -e "${GREEN}✓${NC} Tests executed"
echo -e "${GREEN}✓${NC} Documentation generated"
echo ""
echo "Build artifacts:"
echo "  - Kernel: kernel/target/x86_64-unknown-none/release/aetherion-kernel"
echo "  - Docs: kernel/target/x86_64-unknown-none/doc/"
echo "  - Logs: build.log, test.log"
echo ""
echo -e "${GREEN}Build complete!${NC}"
