OUTPUT_ARCH(loongarch)
ENTRY(_start)

BASE_ADDRESS = 0x80200000;

/*
 * LoongArch64 Linker Script for QEMU -kernel loading
 *
 * PHDRS forces _start (.text.entry) into the FIRST PT_LOAD segment.
 * The first segment has RWE flags so .text can be execute+write.
 * BSS is NOLOAD to avoid file size bloat.
 *
 * QEMU's -kernel loader reads the ELF entry point and loads PT_LOAD
 * segments. By ensuring _start is in the first (and smallest) segment,
 * and that segment contains all code+rodata, we guarantee the CPU
 * starts executing _start after the ELF is loaded.
 */
PHDRS
{
    code    PT_LOAD FLAGS(7);   /* RWE: code + rodata + data */
    bss     PT_LOAD FLAGS(6);   /* RW:  .bss */
    note    PT_NOTE;
}

SECTIONS
{
    . = BASE_ADDRESS;

    _skernel = .;

    /* All executable content: .text.entry (_start), .text, .rodata, .data */
    .code : {
        *(.text.entry)
        *(.text .text.*)
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
        *(.data .data.*)
        *(.sdata .sdata.*)
        *(.got .got.*)
        *(.eh_frame)
        *(.ph_init)
        *(.percpu)
    } :code

    .bss (NOLOAD) : {
        _sbss = .;
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        *(COMMON)
        _ebss = .;
    } :bss

    .bss.stack (NOLOAD) : {
        . = ALIGN(16);
        boot_stack = .;
        . += 0x20000;
        boot_stack_top = .;
    } :bss

    /DISCARD/ : { *(.debug*) *(.rela*) }

    _end = .;
}
