/* AetherionOS Kernel Linker Script for Limine Boot Protocol
 * Kernel is loaded in the upper 2GiB of the virtual address space.
 * Limine maps the kernel at this address and jumps to the entry point.
 *
 * NOTE: This linker script is only used when building with the Limine
 * boot protocol (--features limine). The default bootloader_api 0.11
 * build uses the bootloader crate's own linker script.
 */

OUTPUT_FORMAT(elf64-x86-64)
ENTRY(kmain)

PHDRS
{
    text    PT_LOAD;
    rodata  PT_LOAD;
    data    PT_LOAD;
}

SECTIONS
{
    /* Limine spec mandates kernel in the upper 2GiB */
    . = 0xffffffff80000000;

    /* Kernel start marker for page table verification */
    __kernel_start = .;

    .text : {
        *(.text .text.*)
    } :text

    . = ALIGN(CONSTANT(MAXPAGESIZE));

    .rodata : {
        *(.rodata .rodata.*)
    } :rodata

    . = ALIGN(CONSTANT(MAXPAGESIZE));

    .data : {
        *(.data .data.*)

        /* Limine request sections */
        KEEP(*(.requests_start_marker))
        KEEP(*(.requests))
        KEEP(*(.requests_end_marker))
    } :data

    /* .bss must be last mapped to :data to avoid unnecessary zeros in binary */
    .bss : {
        *(.bss .bss.*)
        *(COMMON)
    } :data

    /* Kernel end marker for page table verification */
    __kernel_end = .;

    /DISCARD/ : {
        *(.eh_frame*)
        *(.note .note.*)
    }
}
