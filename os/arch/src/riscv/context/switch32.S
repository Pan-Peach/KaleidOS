.global __switch
__switch:
    # 保存当前寄存器上下文到 from（RV32：14 个 word）
    sw ra, 0(a0)
    sw sp, 4(a0)
    sw s0, 8(a0)
    sw s1, 12(a0)
    sw s2, 16(a0)
    sw s3, 20(a0)
    sw s4, 24(a0)
    sw s5, 28(a0)
    sw s6, 32(a0)
    sw s7, 36(a0)
    sw s8, 40(a0)
    sw s9, 44(a0)
    sw s10, 48(a0)
    sw s11, 52(a0)
    # tp = 被透明保存 / 恢复的架构执行状态（本镜像无 TLS）；与 RiscvContext 布局
    # 一致（ra@0 / sp@4 / s0-s11@8-52 / tp@56）。psABI 把 tp 标为 unallocatable，
    # 编译器从不写它；trap 帧另行保存 tp（TrapFrame.x[4]）。
    sw tp, 56(a0)

    # 恢复新的寄存器上下文到 to
    lw ra, 0(a1)
    lw sp, 4(a1)
    lw s0, 8(a1)
    lw s1, 12(a1)
    lw s2, 16(a1)
    lw s3, 20(a1)
    lw s4, 24(a1)
    lw s5, 28(a1)
    lw s6, 32(a1)
    lw s7, 36(a1)
    lw s8, 40(a1)
    lw s9, 44(a1)
    lw s10, 48(a1)
    lw s11, 52(a1)
    lw tp, 56(a1)
    ret
