.global __switch
__switch:
    # 保存当前寄存器上下文到 from
    sd ra, 0(a0)
    sd sp, 8(a0)
    sd s0, 16(a0)
    sd s1, 24(a0)
    sd s2, 32(a0)
    sd s3, 40(a0)
    sd s4, 48(a0)
    sd s5, 56(a0)
    sd s6, 64(a0)
    sd s7, 72(a0)
    sd s8, 80(a0)
    sd s9, 88(a0)       
    sd s10, 96(a0)
    sd s11, 104(a0)
    # tp = 被透明保存 / 恢复的架构执行状态（本镜像无 TLS）；与 RiscvContext 布局
    # 一致（ra@0 / sp@8 / s0-s11@16-104 / tp@112）。psABI 把 tp 标为 unallocatable，
    # 编译器从不写它；trap 帧另行保存 tp（TrapFrame.x[4]）。
    sd tp, 112(a0)
    # 恢复新的寄存器上下文 from
    ld ra, 0(a1)
    ld sp, 8(a1)        
    ld s0, 16(a1)
    ld s1, 24(a1)
    ld s2, 32(a1)
    ld s3, 40(a1)
    ld s4, 48(a1)
    ld s5, 56(a1)
    ld s6, 64(a1)
    ld s7, 72(a1)
    ld s8, 80(a1)
    ld s9, 88(a1)
    ld s10, 96(a1)
    ld s11, 104(a1)
    ld tp, 112(a1)
    ret