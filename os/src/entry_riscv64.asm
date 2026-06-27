
    .option norvc
    .section .data.boot_page_table, "aw", @progbits
    .globl boot_page_table
    .p2align 12
boot_page_table:
    .set STRIDE, 0x40000000
    .set PTE_FLAGS, 0x3F
    .set IDX, 0
    .rept 512
    .quad (IDX * STRIDE) | PTE_FLAGS
    .set IDX, IDX + 1
    .endr

    /* Boot trampoline stack: used only during _start before Rust takes over.
     * Placed in .bss so it's zeroed by the bootloader.
     * After MMU is on, VA = PA = 0x8024_0000 (within entry 2: 0x8000_0000-0xBFFF_FFFF). */
    .section .bss, "aw", @nobits
    .globl _boot_trampoline_stack
    .balign 16
_boot_trampoline_stack:
    .space 4096
    .globl _boot_trampoline_stack_top
_boot_trampoline_stack_top:

    .section .text.entry, "ax", @progbits
    .globl _start
    .type _start, @function
    .p2align 4
_start:
    .equ UART, 0x10000000
    li t6, UART
    li t5, 0x41
    sb t5, 0(t6)          # 'A'

    # ---- Clear BSS ----
    la t0, _sbss
    la t1, _ebss
    bgeu t0, t1, bss_done
bss_loop:
    sw zero, 0(t0)
    addi t0, t0, 4
    bltu t0, t1, bss_loop
bss_done:

    li t5, 0x42
    sb t5, 0(t6)          # 'B'

    # ---- Set kernel stack (used as sscratch value) ----
    la t0, _boot_trampoline_stack_top
    # Save it in a callee-saved reg so it survives the MMU switch
    mv s0, t0

    # ---- Enable MMU: satp = (8<<60) | (boot_pt_phys>>12) ----
    la t0, boot_page_table
    srli t0, t0, 12       # PPN of boot_page_table
    lui t1, 0x80000        # 8 << 60
    or t0, t0, t1
    csrw satp, t0
    sfence.vma

    li t5, 0x43
    sb t5, 0(t6)          # 'C'

    # ---- Set sscratch = kernel stack top (for kernelvec) ----
    # Now that MMU is on, VA = PA for the kernel region.
    # kernelvec uses sscratch to hold kernel sp on user traps.
    csrw sscratch, s0

    li t5, 0x44
    sb t5, 0(t6)          # 'D'

    # ---- Set sp to bstack_top (Rust kernel boot stack) ----
    la sp, bstack_top

    li t5, 0x45
    sb t5, 0(t6)          # 'E'

    # ---- Jump to rust_main ----
    la t1, rust_main
    jalr x0, t1

    # rust_main is -> !, should not return
    li t5, 0x52
    sb t5, 0(t6)          # 'R'
hang:
    j hang
