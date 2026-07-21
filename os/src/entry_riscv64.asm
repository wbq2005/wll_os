
    .option norvc
    .section .data.boot_page_table, "aw", @progbits
    .globl boot_page_table
    .p2align 12
boot_page_table:
    # Each root entry is a 1 GiB SV39 leaf.  A superpage PTE stores
    # physical PPN[2] in bits [53:28], so identity entry N is N << 28;
    # it is *not* the 1 GiB byte stride.  Keeping these mappings supervisor
    # only lets user page tables inherit the kernel direct map safely.
    .set PTE_PPN2_SHIFT, 28
    .set PTE_FLAGS, 0x2F
    .set IDX, 0
    .rept 512
    .quad (IDX << PTE_PPN2_SHIFT) | PTE_FLAGS
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
    # ---- Clear BSS ----
    la t0, _sbss
    la t1, _ebss
    bgeu t0, t1, bss_done
bss_loop:
    sw zero, 0(t0)
    addi t0, t0, 4
    bltu t0, t1, bss_loop
bss_done:
    # ---- Set kernel stack (used as sscratch value) ----
    la t0, _boot_trampoline_stack_top
    # Save it in a callee-saved reg so it survives the MMU switch
    mv s0, t0

    # ---- Enable MMU: satp = (8<<60) | (boot_pt_phys>>12) ----
    la t0, boot_page_table
    srli t0, t0, 12       # PPN of boot_page_table
    # RV64 LUI sign-extends bit 31, so `lui t1, 0x80000` produces
    # 0xffff_ffff_8000_0000 and requests a reserved SATP mode.  QEMU applies
    # the WARL rule by leaving SATP bare.  Build the Sv39 mode field with a
    # full-width shift instead.
    li t1, 8
    slli t1, t1, 60        # SATP.MODE = Sv39
    or t0, t0, t1
    csrw satp, t0
    sfence.vma

    # ---- Establish the kernel-side trap ABI ----
    # kernelvec distinguishes a supervisor trap from a user trap by swapping
    # sp with sscratch: sscratch must be zero while executing in the kernel.
    # user_restore installs the current TrapFrame address immediately before
    # sret, and uservec clears it again after a user-to-kernel transition.
    csrw sscratch, zero

    # ---- Set sp to bstack_top (Rust kernel boot stack) ----
    la sp, bstack_top

    # ---- Jump to rust_main ----
    la t1, rust_main
    jalr x0, t1

    # rust_main is -> !, should not return
hang:
    j hang
