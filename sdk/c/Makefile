# sdk/c/Makefile - AetherionOS C-SDK Build System
#
# Builds libaetherion.a -- the static library for Ring 3 C programs.
#
# Usage:
#   make              Build libaetherion.a
#   make clean        Remove build artifacts
#   make install      Copy to INSTALL_DIR (default: .)
#
# To build a user program:
#   gcc -c $(CFLAGS) -I$(SDK_DIR) myapp.c -o myapp.o
#   ld -T $(SDK_DIR)/aetherion.ld -static -o myapp.elf myapp.o -L$(SDK_DIR) -laetherion

CC       ?= gcc
AR       ?= ar
SDK_DIR  := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))

# Bare-metal x86_64 flags (no SSE, no stdlib, large code model for PIC)
CFLAGS   := -nostdlib -fno-builtin -fno-stack-protector -ffreestanding \
            -mno-sse -mno-sse2 -mno-mmx -mno-80387 -mno-red-zone \
            -fPIC -O2 -Wall -Wextra -mcmodel=large

OBJS     := aetherion.o
LIB      := libaetherion.a

.PHONY: all clean install

all: $(LIB)

$(LIB): $(OBJS)
	$(AR) rcs $@ $^
	@echo "[SDK] Built $@ ($(shell stat -c %s $@ 2>/dev/null || echo '?') bytes)"

aetherion.o: aetherion.c aetherion.h
	$(CC) -c $(CFLAGS) -o $@ $<

clean:
	rm -f $(OBJS) $(LIB)

install: $(LIB)
	@echo "[SDK] Library: $(LIB)"
	@echo "[SDK] Header:  aetherion.h"
	@echo "[SDK] Linker:  aetherion.ld"
