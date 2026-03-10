#!/bin/bash
set -e

REPO_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$REPO_DIR"

echo "=== Vérification du disk.img ==="
echo ""

# Info disque
echo "Taille du disk.img:"
ls -lh disk.img

echo ""
echo "Type de système de fichiers:"
file disk.img

echo ""
echo "=== Montage et vérification du contenu ==="

# Créer point de montage
sudo mkdir -p /mnt/aetherion_verify

# Monter
sudo mount -o loop disk.img /mnt/aetherion_verify

echo ""
echo "Contenu racine:"
ls -lh /mnt/aetherion_verify/

echo ""
echo "Contenu /models/:"
ls -lh /mnt/aetherion_verify/models/

echo ""
echo "Espace disponible:"
df -h /mnt/aetherion_verify

# Démonter
sudo umount /mnt/aetherion_verify

echo ""
echo "✅ Vérification terminée"
