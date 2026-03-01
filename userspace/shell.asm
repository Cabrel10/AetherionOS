; shell.asm - AetherionOS Interactive Ring 3 Shell
; Runs entirely in user mode (Ring 3)
; Uses SYSCALL interface for I/O
;
; Syscall ABI (Linux x86_64):
;   RAX = syscall number
;   RDI = arg1, RSI = arg2, RDX = arg3
;   SYSCALL instruction
;   Return value in RAX
;
; Supported commands:
;   help     - Show help text
;   ps       - List processes (syscall 200)
;   version  - Show OS version
;   echo <x> - Echo text
;   exec <p> - Execute /bin/<p>
;   exit     - Exit shell
;   (empty)  - Re-display prompt

BITS 64

section .text
global _start

_start:
    ; Print banner
    mov rax, 1              ; sys_write
    mov rdi, 1              ; fd = stdout
    lea rsi, [rel banner]
    mov rdx, banner_len
    syscall

.main_loop:
    ; Print prompt "ACHA> "
    mov rax, 1
    mov rdi, 1
    lea rsi, [rel prompt]
    mov rdx, prompt_len
    syscall

    ; Read input from stdin (fd=0)
    mov rax, 0              ; sys_read
    mov rdi, 0              ; fd = stdin
    lea rsi, [rel input_buf]
    mov rdx, 255            ; max length
    syscall

    ; RAX = bytes read. If 0, try again a few times then exit
    test rax, rax
    jnz .got_input

    ; No input - increment retry counter
    inc dword [rel retry_count]
    cmp dword [rel retry_count], 100
    jge .do_exit_eof

    ; Brief pause then retry
    pause
    pause
    pause
    jmp .main_loop

.got_input:
    ; Reset retry counter
    mov dword [rel retry_count], 0

    ; Store length
    mov [rel input_len], rax

    ; Null-terminate the input (replace newline with 0)
    lea rdi, [rel input_buf]
    add rdi, rax
    dec rdi                 ; point to last char (should be \n)
    mov byte [rdi], 0       ; null terminate
    dec qword [rel input_len] ; adjust length

    ; Check if input is empty
    cmp qword [rel input_len], 0
    je .main_loop

    ; === Command dispatch ===

    ; Check "help"
    lea rdi, [rel input_buf]
    lea rsi, [rel cmd_help]
    call .strcmp
    test rax, rax
    jz .do_help

    ; Check "ps"
    lea rdi, [rel input_buf]
    lea rsi, [rel cmd_ps]
    call .strcmp
    test rax, rax
    jz .do_ps

    ; Check "version"
    lea rdi, [rel input_buf]
    lea rsi, [rel cmd_version]
    call .strcmp
    test rax, rax
    jz .do_version

    ; Check "exit"
    lea rdi, [rel input_buf]
    lea rsi, [rel cmd_exit]
    call .strcmp
    test rax, rax
    jz .do_exit

    ; Check "echo " prefix (5 chars)
    lea rdi, [rel input_buf]
    lea rsi, [rel cmd_echo]
    mov rcx, 5
    call .strncmp
    test rax, rax
    jz .do_echo

    ; Check "exec " prefix (5 chars)
    lea rdi, [rel input_buf]
    lea rsi, [rel cmd_exec]
    mov rcx, 5
    call .strncmp
    test rax, rax
    jz .do_exec

    ; Unknown command
    mov rax, 1
    mov rdi, 1
    lea rsi, [rel msg_unknown]
    mov rdx, msg_unknown_len
    syscall
    jmp .main_loop

; === Command Handlers ===

.do_help:
    mov rax, 1
    mov rdi, 1
    lea rsi, [rel help_text]
    mov rdx, help_text_len
    syscall
    jmp .main_loop

.do_ps:
    ; syscall 200 = sys_ps (custom)
    mov rax, 200
    syscall
    jmp .main_loop

.do_version:
    mov rax, 1
    mov rdi, 1
    lea rsi, [rel version_text]
    mov rdx, version_text_len
    syscall
    jmp .main_loop

.do_exit:
    mov rax, 1
    mov rdi, 1
    lea rsi, [rel msg_bye]
    mov rdx, msg_bye_len
    syscall
    ; sys_exit(0)
    mov rax, 60
    xor rdi, rdi
    syscall
    hlt

.do_exit_eof:
    ; No more input available, exit gracefully
    mov rax, 1
    mov rdi, 1
    lea rsi, [rel msg_eof]
    mov rdx, msg_eof_len
    syscall
    ; sys_exit(0)
    mov rax, 60
    xor rdi, rdi
    syscall
    hlt

.do_echo:
    ; Echo the text after "echo "
    lea rsi, [rel input_buf]
    add rsi, 5              ; skip "echo "
    ; Calculate remaining length
    mov rdx, [rel input_len]
    sub rdx, 5
    jle .main_loop          ; nothing to echo
    mov rax, 1
    mov rdi, 1
    syscall
    ; Print newline
    mov rax, 1
    mov rdi, 1
    lea rsi, [rel newline]
    mov rdx, 1
    syscall
    jmp .main_loop

.do_exec:
    ; Execute program: path is after "exec "
    lea rdi, [rel input_buf]
    add rdi, 5              ; skip "exec "
    ; sys_exec(path)
    mov rax, 59
    syscall
    ; If exec returns, it failed
    push rax                ; save error
    mov rax, 1
    mov rdi, 1
    lea rsi, [rel msg_exec_fail]
    mov rdx, msg_exec_fail_len
    syscall
    pop rax
    jmp .main_loop

; === String comparison routines ===

; Compare null-terminated strings at RDI and RSI
; Returns 0 in RAX if equal, nonzero if not
.strcmp:
    push rcx
.strcmp_loop:
    mov cl, [rdi]
    mov ch, [rsi]
    cmp cl, ch
    jne .strcmp_neq
    test cl, cl             ; both are 0?
    jz .strcmp_eq
    inc rdi
    inc rsi
    jmp .strcmp_loop
.strcmp_eq:
    xor rax, rax            ; equal
    pop rcx
    ret
.strcmp_neq:
    mov rax, 1              ; not equal
    pop rcx
    ret

; Compare first RCX bytes of strings at RDI and RSI
; Returns 0 in RAX if equal prefix, nonzero if not
.strncmp:
    push rbx
    xor rbx, rbx
.strncmp_loop:
    cmp rbx, rcx
    je .strncmp_eq
    mov al, [rdi + rbx]
    cmp al, [rsi + rbx]
    jne .strncmp_neq
    inc rbx
    jmp .strncmp_loop
.strncmp_eq:
    xor rax, rax
    pop rbx
    ret
.strncmp_neq:
    mov rax, 1
    pop rbx
    ret

; === Data Section ===

section .rodata

banner:
    db 10
    db "========================================", 10
    db "  AetherionOS v1.3.0 - Couche 13 Shell", 10
    db "  Running in Ring 3 (User Mode)", 10
    db "  Type 'help' for commands", 10
    db "========================================", 10, 10
banner_len equ $ - banner

prompt:
    db "ACHA> "
prompt_len equ $ - prompt

help_text:
    db 10
    db "AetherionOS Shell Commands:", 10
    db "  help       - Show this help text", 10
    db "  ps         - List running processes", 10
    db "  version    - Show OS version", 10
    db "  echo <txt> - Echo text to console", 10
    db "  exec <bin> - Execute /bin/<binary>", 10
    db "  exit       - Exit shell", 10, 10
help_text_len equ $ - help_text

version_text:
    db "AetherionOS v1.3.0-couche13-multi-process", 10
    db "Architecture: x86_64, Ring 3 User Space", 10
    db "Kernel: Matriarchal Hierarchy + Priority Scheduler", 10
    db "Security: SYSCALL/SYSRET, W^X, Page Isolation", 10
version_text_len equ $ - version_text

msg_unknown:
    db "Unknown command. Type 'help' for available commands.", 10
msg_unknown_len equ $ - msg_unknown

msg_bye:
    db "Goodbye from ACHA Shell!", 10
msg_bye_len equ $ - msg_bye

msg_exec_fail:
    db "[ERROR] exec failed: file not found", 10
msg_exec_fail_len equ $ - msg_exec_fail

msg_eof:
    db 10, "[SHELL] End of input - exiting.", 10
msg_eof_len equ $ - msg_eof

newline:
    db 10

cmd_help:    db "help", 0
cmd_ps:      db "ps", 0
cmd_version: db "version", 0
cmd_exit:    db "exit", 0
cmd_echo:    db "echo ", 0
cmd_exec:    db "exec ", 0

section .bss
input_buf:   resb 256
input_len:   resq 1
retry_count: resd 1
