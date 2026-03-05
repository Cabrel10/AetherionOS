/* c_app.ld - Linker script for AetherionOS C userspace apps */
/* Base address in PML4[1] for kernel isolation */
ENTRY(_start)

SECTIONS
{
    . = 0x0000008000000000;

    .text : ALIGN(4096)
    {
        *(.text.start)
        *(.text*)
        *(.plt*)
        *(.got.plt*)
    }

    .rodata : ALIGN(4096)
    {
        *(.rodata*)
        *(.got*)
    }

    .data : ALIGN(4096)
    {
        *(.data*)
    }

    .bss : ALIGN(4096)
    {
        *(.bss*)
        *(COMMON)
    }

    /DISCARD/ :
    {
        *(.comment)
        *(.note*)
        *(.eh_frame*)
    }
}
