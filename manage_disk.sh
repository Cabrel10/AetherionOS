#!/bin/bash
set -e

REPO_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$REPO_DIR"

DISK_IMG="disk.img"
MISTRAL_PATH="/mnt/new_data/m0/models/mistral-7b-instruct-v0.3.Q4_K_M.gguf"

# Fonction pour afficher l'usage
show_usage() {
  echo "Usage: $0 {info|expand|add-mistral|add-file}"
  echo ""
  echo "Commandes:"
  echo "  info         - Afficher le contenu du disk.img"
  echo "  expand SIZE  - Agrandir disk.img (ex: expand 8192 pour 8GB)"
  echo "  add-mistral  - Copier Mistral 7B sur le disk"
  echo "  add-file SRC DST - Copier un fichier (ex: add-file model.gguf ::/models/)"
}

# Fonction info
disk_info() {
  echo "=== Disk Info ==="
  ls -lh $DISK_IMG
  echo ""
  echo "=== Root Directory ==="
  mdir -i $DISK_IMG ::/
  echo ""
  echo "=== Models Directory ==="
  mdir -i $DISK_IMG ::/models/ 2>/dev/null || echo "Dossier /models/ n'existe pas"
}

# Fonction expand
disk_expand() {
  local SIZE_MB=$1
  if [ -z "$SIZE_MB" ]; then
    echo "ERROR: Spécifiez la taille en MB (ex: 8192 pour 8GB)"
    exit 1
  fi
  
  echo "=== Expanding disk.img to ${SIZE_MB}MB ==="
  
  # Backup
  cp $DISK_IMG disk_backup.img
  echo "✅ Backup créé: disk_backup.img"
  
  # Créer nouveau disk
  dd if=/dev/zero of=disk_large.img bs=1M count=$SIZE_MB status=progress
  mkfs.fat -F 32 -n "AETHERION" disk_large.img
  echo "✅ Nouveau disk créé: ${SIZE_MB}MB"
  
  # Copier les fichiers
  mkdir -p /tmp/fat_old /tmp/fat_new
  sudo mount -o loop $DISK_IMG /tmp/fat_old
  sudo mount -o loop disk_large.img /tmp/fat_new
  sudo cp -r /tmp/fat_old/* /tmp/fat_new/ 2>/dev/null || true
  sudo umount /tmp/fat_old /tmp/fat_new
  echo "✅ Fichiers copiés"
  
  # Remplacer
  mv $DISK_IMG disk_old.img
  mv disk_large.img $DISK_IMG
  echo "✅ disk.img remplacé (ancien: disk_old.img)"
  
  disk_info
}

# Fonction add-mistral
add_mistral() {
  if [ ! -f "$MISTRAL_PATH" ]; then
    echo "ERROR: Mistral non trouvé à $MISTRAL_PATH"
    exit 1
  fi
  
  echo "=== Adding Mistral 7B to disk ==="
  ls -lh $MISTRAL_PATH
  
  # Créer dossier models si nécessaire
  mmd -i $DISK_IMG ::/models 2>/dev/null || echo "Dossier /models/ existe déjà"
  
  # Copier (peut prendre du temps)
  echo "Copie en cours (4.1GB)..."
  mcopy -o -i $DISK_IMG $MISTRAL_PATH ::/models/mistral.gguf
  echo "✅ Mistral copié sur disk.img"
  
  disk_info
}

# Fonction add-file
add_file() {
  local SRC=$2
  local DST=$3
  
  if [ -z "$SRC" ] || [ -z "$DST" ]; then
    echo "ERROR: Usage: $0 add-file <source> <destination>"
    echo "Exemple: $0 add-file test.gguf ::/models/"
    exit 1
  fi
  
  if [ ! -f "$SRC" ]; then
    echo "ERROR: Fichier source non trouvé: $SRC"
    exit 1
  fi
  
  echo "=== Copying $SRC to $DST ==="
  mcopy -o -i $DISK_IMG $SRC $DST
  echo "✅ Fichier copié"
  
  disk_info
}

# Exécuter selon l'argument
case "$1" in
  info)        disk_info ;;
  expand)      disk_expand $2 ;;
  add-mistral) add_mistral ;;
  add-file)    add_file "$@" ;;
  *)           show_usage ;;
esac
