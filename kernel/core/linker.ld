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
        /* Linux puts a driver's probe-time code in .init.text and frees it
         * afterwards. nk does not free it, so it simply joins .text -- but it
         * has to be gathered explicitly or it is silently dropped, taking
         * every driver's init function with it. */
        *(.init.text .init.text.*)
        *(.exit.text .exit.text.*)
    }

    /* Every built-in driver's module_init.
     *
     * `module_init` on a built-in driver is not a constructor. It is an entry
     * in .initcallN.init, where N is a level, and the levels exist to order
     * registration: the virtio bus registers at 4, the drivers that attach to
     * it at 6. Gathering them out of order gives a driver looking for a bus
     * that does not exist yet, which presents as a device nobody probes.
     *
     * KEEP on every one, because nothing in the program references them and
     * --gc-sections would take the lot. */
    .initcall ALIGN(8) : {
        __initcall_start = .;
        KEEP(*(.initcallearly.init))
        KEEP(*(.initcall0.init))  KEEP(*(.initcall0s.init))
        KEEP(*(.initcall1.init))  KEEP(*(.initcall1s.init))
        KEEP(*(.initcall2.init))  KEEP(*(.initcall2s.init))
        KEEP(*(.initcall3.init))  KEEP(*(.initcall3s.init))
        KEEP(*(.initcall4.init))  KEEP(*(.initcall4s.init))
        KEEP(*(.initcall5.init))  KEEP(*(.initcall5s.init))
        KEEP(*(.initcallrootfs.init))
        KEEP(*(.initcall6.init))  KEEP(*(.initcall6s.init))
        KEEP(*(.initcall7.init))  KEEP(*(.initcall7s.init))
        __initcall_end = .;
    }

    /* The vector table's base must be 2KB-aligned or VBAR_EL1 silently
     * ignores the low bits and every exception lands somewhere arbitrary.
     * Its own output section, so the alignment is stated here rather than
     * depending on an input section's .balign surviving section merging. */
    .vectors ALIGN(2048) : { KEEP(*(.text.vectors)) }

    .rodata ALIGN(8)  : { *(.rodata .rodata.*) *(__param) *(__bug_table) }
    .data   ALIGN(8)  : { *(.data .data.*) *(.init.data .init.data.*) }

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

    /DISCARD/ : {
        *(.comment) *(.note*) *(.eh_frame*) *(.ARM.exidx*)
        /* Linux's build-time metadata, none of which nk consumes.
         * .altinstructions is the one worth naming: it is the table of
         * CPU-erratum and feature patches Linux applies to its own text at
         * boot. nk does not apply them, which is correct for QEMU and is a
         * real limitation on hardware with errata -- noted here rather than
         * silently discarded. */
        *(.modinfo) *(.discard) *(.discard.*)
        *(.altinstructions) *(.altinstr_replacement)
    }
}
