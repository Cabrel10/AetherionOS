#!/usr/bin/env python3
"""
Créer un micro-modèle GGUF pour tester le loader sans utiliser 4GB.
Structure minimale compatible avec le parser GGUF d'AetherionOS.
"""
import struct
import random

def create_test_gguf(output_path, vocab_size=256, embed_dim=32, num_layers=2):
    """
    Créer un fichier GGUF minimal pour tests.
    
    Args:
        vocab_size: Taille du vocabulaire (défaut: 256)
        embed_dim: Dimension des embeddings (défaut: 32)
        num_layers: Nombre de couches (défaut: 2)
    """
    buf = bytearray()
    
    # Header GGUF v3
    buf += b'GGUF'
    buf += struct.pack('<I', 3)    # version
    buf += struct.pack('<Q', 3)    # 3 tenseurs de test
    buf += struct.pack('<Q', 3)    # 3 métadonnées
    
    # Metadata 1: architecture
    key = b'general.architecture'
    buf += struct.pack('<Q', len(key))
    buf += key
    buf += struct.pack('<I', 8)    # type STRING
    val = b'llama'
    buf += struct.pack('<Q', len(val))
    buf += val
    
    # Metadata 2: context length
    key2 = b'llama.context_length'
    buf += struct.pack('<Q', len(key2))
    buf += key2
    buf += struct.pack('<I', 4)    # type UINT32
    buf += struct.pack('<I', 512)
    
    # Metadata 3: embedding dimension
    key3 = b'llama.embedding_length'
    buf += struct.pack('<Q', len(key3))
    buf += key3
    buf += struct.pack('<I', 4)    # type UINT32
    buf += struct.pack('<I', embed_dim)
    
    # Tenseur 1: token_embd.weight (vocab_size x embed_dim F32)
    name = b'token_embd.weight'
    buf += struct.pack('<Q', len(name))
    buf += name
    buf += struct.pack('<I', 2)    # 2 dims
    buf += struct.pack('<Q', vocab_size)
    buf += struct.pack('<Q', embed_dim)
    buf += struct.pack('<I', 0)    # F32
    buf += struct.pack('<Q', 0)    # offset (sera calculé après alignment)
    
    # Tenseur 2: output.weight (embed_dim x vocab_size F32)
    name2 = b'output.weight'
    buf += struct.pack('<Q', len(name2))
    buf += name2
    buf += struct.pack('<I', 2)    # 2 dims
    buf += struct.pack('<Q', embed_dim)
    buf += struct.pack('<Q', vocab_size)
    buf += struct.pack('<I', 0)    # F32
    buf += struct.pack('<Q', vocab_size * embed_dim * 4)  # offset après tenseur 1
    
    # Tenseur 3: blk.0.attn_q.weight (embed_dim x embed_dim F32)
    name3 = b'blk.0.attn_q.weight'
    buf += struct.pack('<Q', len(name3))
    buf += name3
    buf += struct.pack('<I', 2)    # 2 dims
    buf += struct.pack('<Q', embed_dim)
    buf += struct.pack('<Q', embed_dim)
    buf += struct.pack('<I', 0)    # F32
    buf += struct.pack('<Q', 2 * vocab_size * embed_dim * 4)  # offset
    
    # Alignment padding (32 bytes)
    ALIGN = 32
    pad = (ALIGN - len(buf) % ALIGN) % ALIGN
    buf += b'\x00' * pad
    
    # Poids aléatoires pour les 3 tenseurs
    random.seed(42)
    
    # Tenseur 1: token embeddings
    for i in range(vocab_size * embed_dim):
        buf += struct.pack('<f', random.gauss(0, 0.02))
    
    # Tenseur 2: output weights
    for i in range(embed_dim * vocab_size):
        buf += struct.pack('<f', random.gauss(0, 0.02))
    
    # Tenseur 3: attention weights
    for i in range(embed_dim * embed_dim):
        buf += struct.pack('<f', random.gauss(0, 0.02))
    
    # Écrire le fichier
    with open(output_path, 'wb') as f:
        f.write(buf)
    
    size_kb = len(buf) / 1024
    print(f'✅ Modèle créé: {output_path}')
    print(f'   Taille: {len(buf)} bytes ({size_kb:.1f} KB)')
    print(f'   Vocab: {vocab_size}, Embed: {embed_dim}, Layers: {num_layers}')
    print(f'   Tenseurs: 3 (token_embd, output, attn_q)')

if __name__ == '__main__':
    import sys
    
    if len(sys.argv) < 2:
        print("Usage: python3 create_test_model.py <output.gguf> [vocab_size] [embed_dim]")
        print("Exemple: python3 create_test_model.py test_llm.gguf 256 32")
        sys.exit(1)
    
    output = sys.argv[1]
    vocab = int(sys.argv[2]) if len(sys.argv) > 2 else 256
    embed = int(sys.argv[3]) if len(sys.argv) > 3 else 32
    
    create_test_gguf(output, vocab, embed)
