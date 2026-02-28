; hello.asm - Minimal static ELF64 for AetherionOS Ring 3 test
; Uses syscall to write "Hello from Ring 3!\n" to serial (syscall 1 = sys_write)
; Then exit via syscall 60 = sys_exit

BITS 64

section .text
global _start

_start:
    ; sys_write(fd=1, buf=msg, len=19)
    mov rax, 1              ; syscall number: sys_write
    mov rdi, 1              ; fd: stdout
    lea rsi, [rel msg]      ; buffer
    mov rdx, 19             ; length
    syscall

    ; sys_exit(0)
    mov rax, 60             ; syscall number: sys_exit
    xor rdi, rdi            ; exit code 0
    syscall

    ; fallback halt
    hlt
    jmp $

section .rodata
msg: db "Hello from Ring 3!", 10
