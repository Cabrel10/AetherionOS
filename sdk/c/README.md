# AetherionOS C-SDK

Static C library (`libaetherion.a`) for building Ring 3 userspace programs.

## Quick Start

```bash
# Build the SDK
make -C sdk/c/

# Compile your program
gcc -c -nostdlib -fno-builtin -ffreestanding -fPIC -mcmodel=large \
    -mno-sse -mno-sse2 -mno-mmx -mno-80387 -mno-red-zone \
    -O2 -Isdk/c/ myapp.c -o myapp.o

# Link against libaetherion.a
ld -T sdk/c/aetherion.ld -static -o myapp.elf myapp.o -Lsdk/c/ -laetherion
```

## Contents

| File | Purpose |
|------|---------|
| `aetherion.h` | Public API header (syscall wrappers, string utils, network) |
| `aetherion.c` | Implementation (compiled into libaetherion.a) |
| `aetherion.ld` | Linker script (text at `0x8000000000`, Ring 3 isolation) |
| `Makefile` | Builds `libaetherion.a` |

## API Categories

- **POSIX**: `read`, `write`, `exit`, `fork`, `execve`, `waitpid`, `pipe`, `open`, `close`, `mmap`
- **Threads**: `sys_clone`, `sys_wait`, `sys_yield`, `thread_create`
- **Network**: `tcp_connect`, `tcp_send`, `tcp_read`, `tcp_shutdown`, `gethostbyname`, `net_ping`
- **AetherionOS**: `bus_publish`, `vga_write`
- **String**: `strlen`, `strcmp`, `memset`, `memcpy`, `itoa`, `puts`, `print_int`, `print_hex`
