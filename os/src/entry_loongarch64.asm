    # LoongArch64 启动入口（QEMU virt）。
    # 本文件为当前唯一生效的 _start；polyhal-boot 虽在 Cargo 依赖中
    # 但未被引用，不会参与链接。两者不可同时启用。
    .section .text.entry
    .globl _start
_start:
    # 建立 DMW 窗口：0x8000... 为 UC，0x9000... 为 CA
    ori         $t0, $zero, 0x1
    lu52i.d     $t0, $t0, -2048
    csrwr       $t0, 0x180
    ori         $t0, $zero, 0x11
    lu52i.d     $t0, $t0, -1792
    csrwr       $t0, 0x181

    # 打开分页，初始化最小 CPU 状态
    li.w        $t0, 0xb0
    csrwr       $t0, 0x0
    li.w        $t0, 0x00
    csrwr       $t0, 0x1
    li.w        $t0, 0x00
    csrwr       $t0, 0x2

    la.global   $sp, boot_stack_top
    csrrd       $a0, 0x20           # cpuid
    li.w        $a1, 0x100000       # QEMU DTB 地址
    la.global   $t0, rust_main
    jirl        $zero, $t0, 0

    .section .bss.stack
    .align 12
boot_stack:
    .space 131072  # 128KB
boot_stack_top:
