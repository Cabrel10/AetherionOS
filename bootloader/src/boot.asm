; Aetherion OS Bootloader
; BIOS Boot Sector (512 bytes)
; Loads kernel from disk and transfers control

[org 0x7c00]          ; BIOS loads bootloader here
[bits 16]             ; Start in 16-bit real mode

start:
    ; Setup segments
    cli                   ; Disable interrupts
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7c00        ; Stack grows downward from bootloader
    sti                   ; Enable interrupts
    
    ; Display boot message
    mov si, msg_boot
    call print_string
    
    ; Load kernel from disk
    ; For now, we'll assume kernel is at sector 2 (sector 1 is bootloader)
    ; In production, use file system
    
    mov si, msg_loading
    call print_string
    
    ; Reset disk
    xor ax, ax
    int 0x13
    jc disk_error
    
    ; Load kernel sectors (simplified - load 64 sectors = 32KB)
    mov ah, 0x02          ; BIOS read sector function
    mov al, 64            ; Number of sectors to read
    mov ch, 0             ; Cylinder 0
    mov cl, 2             ; Start at sector 2 (sector 1 is bootloader)
    mov dh, 0             ; Head 0
    mov dl, 0x80          ; First hard disk (or 0x00 for floppy)
    mov bx, 0x8000        ; Load kernel at 0x8000
    int 0x13
    jc disk_error
    
    mov si, msg_loaded
    call print_string
    
    ; Enter protected mode
    call enable_a20       ; Enable A20 line (access >1MB memory)
    cli                   ; Disable interrupts for mode switch
    lgdt [gdt_descriptor] ; Load GDT
    
    ; Switch to protected mode
    mov eax, cr0
    or eax, 1             ; Set PE (Protection Enable) bit
    mov cr0, eax
    
    ; Far jump to 32-bit code (flushes pipeline)
    jmp CODE_SEG:protected_mode_start

disk_error:
    mov si, msg_disk_error
    call print_string
    jmp hang

; Print string (16-bit real mode)
; SI = pointer to null-terminated string
print_string:
    push ax
    push bx
    mov ah, 0x0e          ; BIOS teletype output
.loop:
    lodsb                 ; Load byte from [SI] into AL, increment SI
    test al, al           ; Check for null terminator
    jz .done
    int 0x10              ; BIOS video interrupt
    jmp .loop
.done:
    pop bx
    pop ax
    ret

; Enable A20 line (allows access to memory above 1MB)
enable_a20:
    in al, 0x92
    or al, 2
    out 0x92, al
    ret

; Boot messages
msg_boot:        db 'Aetherion Bootloader v0.0.1', 0x0d, 0x0a, 0
msg_loading:     db 'Loading kernel...', 0x0d, 0x0a, 0
msg_loaded:      db 'Kernel loaded. Entering protected mode...', 0x0d, 0x0a, 0
msg_disk_error:  db 'DISK ERROR!', 0x0d, 0x0a, 0

; GDT (Global Descriptor Table)
gdt_start:

gdt_null:  ; Mandatory null descriptor
    dq 0

gdt_code:  ; Code segment descriptor
    dw 0xffff      ; Limit (bits 0-15)
    dw 0           ; Base (bits 0-15)
    db 0           ; Base (bits 16-23)
    db 0x9a        ; Access byte (present, ring 0, code, executable, readable)
    db 0xcf        ; Flags (4KB granularity, 32-bit) + Limit (bits 16-19)
    db 0           ; Base (bits 24-31)

gdt_data:  ; Data segment descriptor
    dw 0xffff      ; Limit (bits 0-15)
    dw 0           ; Base (bits 0-15)
    db 0           ; Base (bits 16-23)
    db 0x92        ; Access byte (present, ring 0, data, writable)
    db 0xcf        ; Flags (4KB granularity, 32-bit) + Limit (bits 16-19)
    db 0           ; Base (bits 24-31)

gdt_end:

gdt_descriptor:
    dw gdt_end - gdt_start - 1  ; Size
    dd gdt_start                 ; Offset

CODE_SEG equ gdt_code - gdt_start
DATA_SEG equ gdt_data - gdt_start

[bits 32]
protected_mode_start:
    ; Setup segments
    mov ax, DATA_SEG
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax
    mov esp, 0x90000      ; Set up stack
    
    ; Setup paging for long mode (64-bit)
    ; For simplicity, identity map first 2MB
    
    ; Clear page tables area (0x1000 - 0x5000)
    mov edi, 0x1000
    mov cr3, edi          ; Set CR3 to PML4 address
    xor eax, eax
    mov ecx, 4096
    rep stosd             ; Clear 16KB (4 page tables)
    mov edi, cr3
    
    ; Setup page tables (simplified identity mapping)
    ; PML4[0] -> PDPT
    mov dword [edi], 0x2003      ; PML4[0] = 0x2000 | present | writable
    add edi, 0x1000
    
    ; PDPT[0] -> PDT
    mov dword [edi], 0x3003      ; PDPT[0] = 0x3000 | present | writable
    add edi, 0x1000
    
    ; PDT[0] -> PT
    mov dword [edi], 0x4003      ; PDT[0] = 0x4000 | present | writable
    add edi, 0x1000
    
    ; PT entries (identity map first 2MB)
    mov ebx, 0x00000003          ; Present | writable
    mov ecx, 512                 ; 512 entries * 4KB = 2MB
.set_entry:
    mov dword [edi], ebx
    add ebx, 0x1000              ; Next 4KB page
    add edi, 8
    loop .set_entry
    
    ; Enable PAE (Physical Address Extension)
    mov eax, cr4
    or eax, 1 << 5               ; Set PAE bit
    mov cr4, eax
    
    ; Set long mode bit in EFER MSR
    mov ecx, 0xC0000080          ; EFER MSR
    rdmsr
    or eax, 1 << 8               ; Set LM bit
    wrmsr
    
    ; Enable paging (enters long mode)
    mov eax, cr0
    or eax, 1 << 31              ; Set PG bit
    mov cr0, eax
    
    ; Load 64-bit GDT
    lgdt [gdt64_descriptor]
    
    ; Far jump to 64-bit code
    jmp CODE64_SEG:long_mode_start

; 64-bit GDT
gdt64_start:
    dq 0                         ; Null descriptor

gdt64_code:
    dq 0x00209A0000000000        ; 64-bit code segment

gdt64_data:
    dq 0x0000920000000000        ; 64-bit data segment

gdt64_end:

gdt64_descriptor:
    dw gdt64_end - gdt64_start - 1
    dd gdt64_start

CODE64_SEG equ gdt64_code - gdt64_start
DATA64_SEG equ gdt64_data - gdt64_start

[bits 64]
long_mode_start:
    ; Setup segments
    mov ax, DATA64_SEG
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax
    
    ; Clear screen (VGA text mode)
    mov rdi, 0xb8000
    mov rax, 0x0f200f20      ; White space on black background
    mov rcx, 500             ; 80*25 / 4 (we write 4 chars at once)
    rep stosq
    
    ; Jump to kernel
    ; Kernel is loaded at 0x8000, jump to it
    mov rax, 0x8000
    jmp rax

hang:
    cli
    hlt
    jmp hang

; Padding and boot signature
times 510-($-$$) db 0        ; Pad to 510 bytes
dw 0xaa55                    ; Boot signature
