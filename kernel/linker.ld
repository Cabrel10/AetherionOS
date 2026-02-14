/* Aetherion OS - Linker Script */
/* Couche 1 HAL - Boot configuration */

ENTRY(_start)

SECTIONS {
    /* Kernel starts at 1MB (standard for bootloaders) */
    . = 1M;

    .text : {
        *(.text .text.*)
    }

    .rodata : {
        *(.rodata .rodata.*)
    }

    .data : {
        *(.data .data.*)
    }

    .bss : {
        *(COMMON)
        *(.bss .bss.*)
    }

    /* Discard debug sections to reduce size */
    /DISCARD/ : {
        *(.comment)
        *(.note*)
    }
}
