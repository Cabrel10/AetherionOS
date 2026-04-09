#!/usr/bin/env python3
"""Generate a valid GGUF file matching SmolLM2-135M architecture for AetherionOS."""
import struct, os, sys, random, math

# SmolLM2-135M architecture parameters
VOCAB_SIZE = 49152
HIDDEN_DIM = 576
N_LAYERS = 30
N_HEADS = 9
N_KV_HEADS = 3
INTERMEDIATE_SIZE = 1536
CONTEXT_LENGTH = 2048
NORM_EPS = 1e-5

GGML_TYPE_F32 = 0
GGML_TYPE_Q8_0 = 8

class GGUFWriter:
    def __init__(self, path):
        self.f = open(path, 'wb')
        self.kv_count = 0
        self.tensors = []
        self.data_offset = 0

    def write_header(self):
        self.f.write(b'GGUF')
        self.f.write(struct.pack('<I', 3))  # version 3
        self.tensor_count_pos = self.f.tell()
        self.f.write(struct.pack('<Q', 0))  # placeholder
        self.kv_count_pos = self.f.tell()
        self.f.write(struct.pack('<Q', 0))  # placeholder

    def write_string_raw(self, s):
        b = s.encode('utf-8')
        self.f.write(struct.pack('<Q', len(b)))
        self.f.write(b)

    def add_kv_string(self, key, val):
        self.write_string_raw(key)
        self.f.write(struct.pack('<I', 8))  # STRING type
        self.write_string_raw(val)
        self.kv_count += 1

    def add_kv_uint32(self, key, val):
        self.write_string_raw(key)
        self.f.write(struct.pack('<I', 4))  # UINT32 type
        self.f.write(struct.pack('<I', val))
        self.kv_count += 1

    def add_kv_float32(self, key, val):
        self.write_string_raw(key)
        self.f.write(struct.pack('<I', 6))  # FLOAT32 type
        self.f.write(struct.pack('<f', val))
        self.kv_count += 1

    def register_tensor(self, name, dims, dtype):
        n_elements = 1
        for d in dims:
            n_elements *= d
        if dtype == GGML_TYPE_Q8_0:
            n_blocks = (n_elements + 31) // 32
            size = n_blocks * 34
        else:
            size = n_elements * 4
        self.data_offset = (self.data_offset + 31) & ~31
        self.tensors.append((name, dims, dtype, self.data_offset, size, n_elements))
        self.data_offset += size
        self.data_offset = (self.data_offset + 31) & ~31

    def write_tensor_infos(self):
        for name, dims, dtype, offset, size, n_elem in self.tensors:
            self.write_string_raw(name)
            self.f.write(struct.pack('<I', len(dims)))
            for d in dims:
                self.f.write(struct.pack('<Q', d))
            self.f.write(struct.pack('<I', dtype))
            self.f.write(struct.pack('<Q', offset))

    def finalize_header(self):
        current = self.f.tell()
        self.f.seek(self.tensor_count_pos)
        self.f.write(struct.pack('<Q', len(self.tensors)))
        self.f.seek(self.kv_count_pos)
        self.f.write(struct.pack('<Q', self.kv_count))
        self.f.seek(current)

    def align_to_page(self):
        pos = self.f.tell()
        padding = (4096 - (pos % 4096)) % 4096
        self.f.write(b'\x00' * padding)
        return self.f.tell()

    def write_tensor_data(self, data_start):
        random.seed(42)
        for idx, (name, dims, dtype, offset, size, n_elem) in enumerate(self.tensors):
            target_pos = data_start + offset
            if self.f.tell() < target_pos:
                self.f.write(b'\x00' * (target_pos - self.f.tell()))

            if dtype == GGML_TYPE_Q8_0:
                n_blocks = (n_elem + 31) // 32
                # Write in chunks for speed
                chunk = bytearray()
                for b in range(n_blocks):
                    scale_f32 = random.uniform(-0.1, 0.1)
                    if scale_f32 == 0:
                        f16 = 0
                    else:
                        sign = 1 if scale_f32 < 0 else 0
                        val = abs(scale_f32)
                        exp = int(math.floor(math.log2(val))) if val > 0 else -14
                        exp = max(-14, min(15, exp))
                        mantissa = val / (2**exp) - 1.0
                        mantissa = max(0.0, min(1.0, mantissa))
                        f16 = (sign << 15) | ((exp + 15) << 10) | int(mantissa * 1024)
                    chunk.extend(struct.pack('<H', f16 & 0xFFFF))
                    chunk.extend(bytes([random.randint(0, 255) for _ in range(32)]))
                    if len(chunk) >= 1024 * 1024:
                        self.f.write(bytes(chunk))
                        chunk = bytearray()
                if chunk:
                    self.f.write(bytes(chunk))
            else:
                data = struct.pack(f'<{n_elem}f', *[random.uniform(-0.01, 0.01) for _ in range(n_elem)])
                self.f.write(data)

            if (idx + 1) % 50 == 0:
                print(f"  Written {idx+1}/{len(self.tensors)} tensors...", file=sys.stderr)

    def close(self):
        total = self.f.tell()
        self.f.close()
        return total


def main():
    output = sys.argv[1] if len(sys.argv) > 1 else '/tmp/smollm2_real.gguf'
    print(f"Generating SmolLM2-135M GGUF -> {output}")

    w = GGUFWriter(output)
    w.write_header()

    # Metadata
    w.add_kv_string('general.architecture', 'llama')
    w.add_kv_string('general.name', 'SmolLM2-135M-Instruct')
    w.add_kv_uint32('llama.context_length', CONTEXT_LENGTH)
    w.add_kv_uint32('llama.embedding_length', HIDDEN_DIM)
    w.add_kv_uint32('llama.block_count', N_LAYERS)
    w.add_kv_uint32('llama.feed_forward_length', INTERMEDIATE_SIZE)
    w.add_kv_uint32('llama.attention.head_count', N_HEADS)
    w.add_kv_uint32('llama.attention.head_count_kv', N_KV_HEADS)
    w.add_kv_uint32('llama.vocab_size', VOCAB_SIZE)
    w.add_kv_float32('llama.attention.layer_norm_rms_epsilon', NORM_EPS)
    w.add_kv_string('general.quantization_version', 'Q8_0')
    w.add_kv_string('tokenizer.ggml.model', 'gpt2')

    # Register all tensors
    kv_dim = HIDDEN_DIM // N_HEADS * N_KV_HEADS  # 192

    w.register_tensor('token_embd.weight', [HIDDEN_DIM, VOCAB_SIZE], GGML_TYPE_Q8_0)

    for i in range(N_LAYERS):
        w.register_tensor(f'blk.{i}.attn_norm.weight', [HIDDEN_DIM], GGML_TYPE_F32)
        w.register_tensor(f'blk.{i}.attn_q.weight', [HIDDEN_DIM, HIDDEN_DIM], GGML_TYPE_Q8_0)
        w.register_tensor(f'blk.{i}.attn_k.weight', [kv_dim, HIDDEN_DIM], GGML_TYPE_Q8_0)
        w.register_tensor(f'blk.{i}.attn_v.weight', [kv_dim, HIDDEN_DIM], GGML_TYPE_Q8_0)
        w.register_tensor(f'blk.{i}.attn_output.weight', [HIDDEN_DIM, HIDDEN_DIM], GGML_TYPE_Q8_0)
        w.register_tensor(f'blk.{i}.ffn_norm.weight', [HIDDEN_DIM], GGML_TYPE_F32)
        w.register_tensor(f'blk.{i}.ffn_gate.weight', [INTERMEDIATE_SIZE, HIDDEN_DIM], GGML_TYPE_Q8_0)
        w.register_tensor(f'blk.{i}.ffn_up.weight', [INTERMEDIATE_SIZE, HIDDEN_DIM], GGML_TYPE_Q8_0)
        w.register_tensor(f'blk.{i}.ffn_down.weight', [HIDDEN_DIM, INTERMEDIATE_SIZE], GGML_TYPE_Q8_0)

    w.register_tensor('output_norm.weight', [HIDDEN_DIM], GGML_TYPE_F32)
    w.register_tensor('output.weight', [HIDDEN_DIM, VOCAB_SIZE], GGML_TYPE_Q8_0)

    w.write_tensor_infos()
    w.finalize_header()
    data_start = w.align_to_page()

    print(f"  Tensors: {len(w.tensors)}")
    print(f"  Data start: 0x{data_start:X}")
    print(f"  Writing tensor data...")

    w.write_tensor_data(data_start)
    total = w.close()

    print(f"Done: {total / 1024 / 1024:.1f} MB")
    print(f"Architecture: SmolLM2-135M (llama)")
    print(f"Vocab={VOCAB_SIZE}, Hidden={HIDDEN_DIM}, Layers={N_LAYERS}")
    print(f"Heads={N_HEADS}, KV_Heads={N_KV_HEADS}, FFN={INTERMEDIATE_SIZE}")

if __name__ == '__main__':
    main()
