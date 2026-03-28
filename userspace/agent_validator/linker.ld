/* AetherionOS Userspace Linker Script
   Ensures .text comes first at the image base so the first page is executable.
   This prevents NX-related page faults when kernel code shares PML4[16]. */

ENTRY(_start)

SECTIONS
{
    . = 0x8000000000;

    /* Code first — must be executable at page 0 of user address space */
    .text ALIGN(0x1000) : {
        *(.text .text.*)
    }

    /* Read-only data on a new page */
    .rodata ALIGN(0x1000) : {
        *(.rodata .rodata.*)
        *(.eh_frame_hdr)
        *(.eh_frame)
    }

    /* Read-write data on a new page */
    .data ALIGN(0x1000) : {
        *(.data .data.*)
        *(.data.rel.ro .data.rel.ro.*)
    }

    .bss ALIGN(0x1000) : {
        *(.bss .bss.*)
        *(COMMON)
    }

    /DISCARD/ : {
        *(.comment)
        *(.note.*)
    }
}
