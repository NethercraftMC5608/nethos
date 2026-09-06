/* QEMU's `virt` machine puts RAM at 0x4000_0000 and loads an arm64 -kernel
 * image at 0x4008_0000, the same address the Linux arm64 boot protocol uses.
 * Linking there means the ELF's physical addresses match where QEMU actually
 * places it, so no relocation and no position-independent code is needed. */
ENTRY(_start)

SECTIONS
{
    . = 0x40080000;

    .text : {
        KEEP(*(.text.boot))     /* _start must be the first byte of the image */
        *(.text .text.*)
    }

    /* The vector table's base must be 2KB-aligned or VBAR_EL1 silently
     * ignores the low bits and every exception lands somewhere arbitrary.
     * Its own output section, so the alignment is stated here rather than
     * depending on an input section's .balign surviving section merging. */
    .vectors ALIGN(2048) : { KEEP(*(.text.vectors)) }

    .rodata ALIGN(8)  : { *(.rodata .rodata.*) }
    .data   ALIGN(8)  : { *(.data .data.*) }

    /* 4096, not 16: the page tables in paging.rs are 4KB-aligned statics and
     * TTBR0_EL1 silently ignores the low bits of an address that is not, so a
     * misaligned table is not a fault -- it is a translation regime pointing
     * somewhere arbitrary. lld places the symbols correctly regardless by
     * padding inside the section, but it warns that the section's own
     * alignment contradicts its contents, and it is right to. */
    .bss ALIGN(4096) (NOLOAD) : {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    }

    /* The boot stack lives in the image rather than being placed at some
     * assumed top-of-RAM: the device tree has not been parsed yet at the
     * point sp is first set, so how much RAM exists is not yet known. */
    . = ALIGN(16);
    . += 0x10000;
    __stack_top = .;

    __image_end = .;

    /DISCARD/ : { *(.comment) *(.note*) *(.eh_frame*) *(.ARM.exidx*) }
}
