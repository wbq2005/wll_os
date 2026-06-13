macro_rules! includes_trap_macros {
    () => {
        r#"
        .ifndef REGS_TRAP_MACROS_FLAG
        .equ REGS_TRAP_MACROS_FLAG, 1

        .macro LDR  reg, offset
            ld  \reg, \offset*8(sp)
        .endm

        .macro STR  reg, offset
            sd  \reg, \offset*8(sp)
        .endm

        .macro LOAD reg, offset
            ld  \reg, \offset*8(sp)
        .endm

        .macro SAVE reg, offset
            sd  \reg, \offset*8(sp)
        .endm

        .macro LOAD_N n
            ld  x\n, \n*8(sp)
        .endm

        .macro SAVE_N n
            sd  x\n, \n*8(sp)
        .endm

        .macro SAVE_GENERAL_REGS
            SAVE    x1, 1
            csrr    x1, sscratch
            SAVE    x1, 2
            .set    n, 3
            .rept   29 
                SAVE_N  %n
            .set    n, n + 1
            .endr

            csrr    t0, sstatus
            csrr    t1, sepc
            SAVE    t0, 32
            SAVE    t1, 33
        .endm

        .macro SAVE_FP_REGS
            .option push
            .option arch, +d
            fsd     f0,  36*8(sp)
            fsd     f1,  37*8(sp)
            fsd     f2,  38*8(sp)
            fsd     f3,  39*8(sp)
            fsd     f4,  40*8(sp)
            fsd     f5,  41*8(sp)
            fsd     f6,  42*8(sp)
            fsd     f7,  43*8(sp)
            fsd     f8,  44*8(sp)
            fsd     f9,  45*8(sp)
            fsd     f10, 46*8(sp)
            fsd     f11, 47*8(sp)
            fsd     f12, 48*8(sp)
            fsd     f13, 49*8(sp)
            fsd     f14, 50*8(sp)
            fsd     f15, 51*8(sp)
            fsd     f16, 52*8(sp)
            fsd     f17, 53*8(sp)
            fsd     f18, 54*8(sp)
            fsd     f19, 55*8(sp)
            fsd     f20, 56*8(sp)
            fsd     f21, 57*8(sp)
            fsd     f22, 58*8(sp)
            fsd     f23, 59*8(sp)
            fsd     f24, 60*8(sp)
            fsd     f25, 61*8(sp)
            fsd     f26, 62*8(sp)
            fsd     f27, 63*8(sp)
            fsd     f28, 64*8(sp)
            fsd     f29, 65*8(sp)
            fsd     f30, 66*8(sp)
            fsd     f31, 67*8(sp)
            frcsr   t0
            sd      t0, 68*8(sp)
            .option pop
        .endm

        .macro LOAD_GENERAL_REGS
            LOAD    t0, 32
            LOAD    t1, 33
            csrw    sstatus, t0
            csrw    sepc, t1

            LOAD    x1, 1
            .set    n, 3
            .rept   29
                LOAD_N  %n
            .set    n, n + 1
            .endr
            LOAD    x2, 2
        .endm

        .macro LOAD_FP_REGS
            .option push
            .option arch, +d
            fld     f0,  36*8(sp)
            fld     f1,  37*8(sp)
            fld     f2,  38*8(sp)
            fld     f3,  39*8(sp)
            fld     f4,  40*8(sp)
            fld     f5,  41*8(sp)
            fld     f6,  42*8(sp)
            fld     f7,  43*8(sp)
            fld     f8,  44*8(sp)
            fld     f9,  45*8(sp)
            fld     f10, 46*8(sp)
            fld     f11, 47*8(sp)
            fld     f12, 48*8(sp)
            fld     f13, 49*8(sp)
            fld     f14, 50*8(sp)
            fld     f15, 51*8(sp)
            fld     f16, 52*8(sp)
            fld     f17, 53*8(sp)
            fld     f18, 54*8(sp)
            fld     f19, 55*8(sp)
            fld     f20, 56*8(sp)
            fld     f21, 57*8(sp)
            fld     f22, 58*8(sp)
            fld     f23, 59*8(sp)
            fld     f24, 60*8(sp)
            fld     f25, 61*8(sp)
            fld     f26, 62*8(sp)
            fld     f27, 63*8(sp)
            fld     f28, 64*8(sp)
            fld     f29, 65*8(sp)
            fld     f30, 66*8(sp)
            fld     f31, 67*8(sp)
            ld      t0, 68*8(sp)
            fscsr   t0
            .option pop
        .endm

        .macro LOAD_PERCPU dst, sym
            lui  \dst, %hi(__PERCPU_\sym)
            add  \dst, \dst, gp
            ld   \dst, %lo(__PERCPU_\sym)(\dst)
        .endm

        .macro SAVE_PERCPU sym, temp, src
            lui  \temp, %hi(__PERCPU_\sym)
            add  \temp, \temp, gp
            sd   \src,  %lo(__PERCPU_\sym)(\temp)
        .endm

        .endif
        "#
    };
}
