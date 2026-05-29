ENTRY(_start)

BASE_ADDRESS = 0x80200000;

/* PHDRS: explicitly define program segments so .bss NOBITS doesn't inflate the file. */
PHDRS
{
    code    PT_LOAD FLAGS(5);   /* R+X: .text, .rodata, .ph_init, .percpu */
    data    PT_LOAD FLAGS(6);   /* R+W: .data */
    bss     PT_LOAD FLAGS(6);   /* R+W: .bss (NOLOAD but needs PT_LOAD for runtime) */
    note    PT_NOTE;
}

/* 64-bit 字节序必须匹配 */
SECTIONS
{
    . = BASE_ADDRESS;

    .text.entry : { *(.text.entry) } :code
    .text : { *(.text .text.*) } :code
    .rodata : { *(.rodata .rodata.*) *(.srodata .srodata.*) } :code
    .ph_init : { *(.ph_init) } :code
    .percpu : { *(.percpu) } :code

    /* .bss NOLOAD: immediately after .rodata to keep _sbss within ±512KB of code */
    .bss (NOLOAD) : {
        _sbss = .;
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        *(COMMON)
        _ebss = .;
    } :bss

    .data : {
        *(.data .data.*)
        *(.sdata .sdata.*)
    } :data

    .bss.bstack (NOLOAD) : {
        . = ALIGN(16);
        bstack = .;
        . += 0x80000;
        bstack_top = .;
    } :bss

    /* Metadata sections placed after .bss (not in LOAD segments) */
    .symtab : { *(.symtab) }
    .symtab_shndx : { *(.symtab_shndx) }
    .strtab : { *(.strtab) }
    .shstrtab : { *(.shstrtab) }
    .comment : { *(.comment) }
    .riscv.attributes : { *(.riscv.attributes) }

    /DISCARD/ : {
        *(.eh_frame)
        *(.debug*)
        *(.rela*)
    }

    . = ALIGN(8);
    _end = .;
}
