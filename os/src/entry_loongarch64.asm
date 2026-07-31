    # LoongArch64 entry for QEMU virt.
    .section .text.entry
    .globl _start
_start:
    # DMW0: uncached high direct map, useful for MMIO.
    ori         $t0, $zero, 0x1
    lu52i.d     $t0, $t0, -2048
    csrwr       $t0, 0x180

    # DMW1: cached high direct map for RAM.
    ori         $t0, $zero, 0x11
    lu52i.d     $t0, $t0, -1792
    csrwr       $t0, 0x181

    # DMW2: low identity map while this kernel is linked at 0x8020_0000.
    # Without it, enabling PG invalidates the next instruction fetch.
    ori         $t0, $zero, 0x11
    csrwr       $t0, 0x182

    # Enable paging/direct-mapped windows and keep interrupts/FPU off.
    li.w        $t0, 0xb0
    csrwr       $t0, 0x0
    li.w        $t0, 0x00
    csrwr       $t0, 0x1
    li.w        $t0, 0x00
    csrwr       $t0, 0x2

    la.local    $sp, boot_stack_top
    csrrd       $a0, 0x20
    addi.d      $r21, $a0, 1
    li.w        $a1, 0x100000
    la.local    $t0, rust_main
    jirl        $zero, $t0, 0

    .section .bss.stack
    .align 12
boot_stack:
    .space 131072
boot_stack_top:

    .section .bss.smp_stacks
    .align 4
    .globl _smp_boot_stacks
_smp_boot_stacks:
    .space 1048576
    .globl _smp_boot_stacks_end
_smp_boot_stacks_end:

    .section .text.entry
    .globl _secondary_start
_secondary_start:
    ori         $t0, $zero, 0x1
    lu52i.d     $t0, $t0, -2048
    csrwr       $t0, 0x180
    ori         $t0, $zero, 0x11
    lu52i.d     $t0, $t0, -1792
    csrwr       $t0, 0x181
    ori         $t0, $zero, 0x11
    csrwr       $t0, 0x182

    li.w        $t0, 0xb0
    csrwr       $t0, 0x0
    li.w        $t0, 0x00
    csrwr       $t0, 0x1
    li.w        $t0, 0x00
    csrwr       $t0, 0x2

    li.w        $t0, 0x1028
    iocsrrd.d   $sp, $t0
    csrrd       $a0, 0x20
    addi.d      $r21, $a0, 1
    la.local    $t0, rust_secondary_main
    jirl        $zero, $t0, 0
