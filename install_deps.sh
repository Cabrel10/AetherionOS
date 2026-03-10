#!/bin/bash
set -e

echo "========================================="
echo "AetherionOS - Installation des dépendances"
echo "========================================="

# Vérifier si on est root pour apt
if [ "$EUID" -ne 0 ]; then 
  echo "⚠️  Ce script nécessite sudo pour installer les paquets système"
  echo "Relancez avec: sudo $0"
  exit 1
fi

echo ""
echo "=== Étape 1/3: Installation des paquets système ==="
apt update
apt install -y \
  qemu-system-x86 \
  gcc \
  nasm \
  mtools \
  git \
  curl \
  python3 \
  build-essential \
  pkg-config \
  libssl-dev \
  ovmf \
  gdb \
  screen \
  xterm

echo "✅ Paquets système installés"

# Vérifier QEMU
echo ""
echo "=== Vérification QEMU ==="
qemu-system-x86_64 --version | head -1

echo ""
echo "========================================="
echo "✅ Installation terminée!"
echo "========================================="
echo ""
echo "Prochaines étapes (en tant qu'utilisateur normal):"
echo "  1. Installer Rust: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
echo "  2. Configurer Rust: source ~/.cargo/env"
echo "  3. Installer nightly: rustup toolchain install nightly-2023-08-01"
echo "  4. Ajouter composants: rustup component add rust-src --toolchain nightly-2023-08-01"
echo "  5. Ajouter target: rustup target add x86_64-unknown-none --toolchain nightly-2023-08-01"
echo "  6. Installer bootimage: cargo install bootimage --version 0.10.3"
