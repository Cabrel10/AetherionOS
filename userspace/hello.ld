/* userspace/hello.ld - Linker script for Ring 3 user process
 * Places ALL segments in PML4[1] (0x0000_0080_0000_0000 = 512 GiB)
 * to ensure complete isolation from kernel in PML4[0]
 */

ENTRY(_start)

SECTIONS
{
    /* Base address in PML4[1] */
    . = 0x0000008000000000;

    .text : ALIGN(4096)
    {
        *(.text*)
    }

    .rodata : ALIGN(4096)
    {
        *(.rodata*)
    }

    .data : ALIGN(4096)
    {
        *(.data*)
    }

    .bss : ALIGN(4096)
    {
        *(.bss*)
    }

    /DISCARD/ :
    {
        *(.comment)
        *(.note*)
        *(.eh_frame*)
    }
}
