.section .text.entry
.globl _start

# OpenSBI:
# a0: hartid
# a1: dtb addr

_start:
    # auipc observes the actual address where the loader entered the image;
    # this is the runtime kernel PA, independent of the link-time LMA.
    auipc s1, 0

    # 非 boot hart（hartid != 0）直接永久 park：
    # 不进 Rust、不碰栈（早期每个 hart 共用 boot_stack_top，若都进 Rust
    # 会踩同一个栈 = UB）。
    beq a0, zero, .Lboot_hart

.Lpark:
    wfi
    j .Lpark

.Lboot_hart:
    la sp, boot_stack_top

    # 清零正式内核 .bss 的物理 LMA（boot hart 专属；副 hart 已 park）。
    # linker 将 .bss 的 VMA 放到高半区，因此这里必须使用 LMA symbols。
    la t0, __bss_load_start
    la t1, __bss_load_end
    bgeu t0, t1, 2f
1:
    sd zero, 0(t0)
    addi t0, t0, 8
    bltu t0, t1, 1b
2:

    # 清零临时页表（它们位于低地址 bootstrap 区，不属于正式 .bss）。
    la t0, early_boot_root
    la t1, early_tables_end
4:
    bgeu t0, t1, .Lclear_tables_done
    sd zero, 0(t0)
    addi t0, t0, 8
    j 4b

.Lclear_tables_done:

    # Temporary broad bootstrap map: 0..8 GiB identity plus the same
    # physical windows at HIGH_OFFSET. Rust will replace it with the FDT-
    # driven root before Core starts.
    la t0, early_boot_root
    li t1, 0xcf                  # V|R|W|X|A|D
    li a2, 0
    li a3, 8
5:
    slli t2, a2, 30              # physical 1 GiB window base
    srli t3, t2, 2               # level-2 leaf PPN encoding
    or t3, t3, t1

    slli t4, a2, 3
    add t5, t0, t4
    sd t3, 0(t5)                  # identity root[a2]

    addi t4, a2, 256
    slli t4, t4, 3
    add t5, t0, t4
    sd t3, 0(t5)                  # high-half root[256 + a2]

    addi a2, a2, 1
    bltu a2, a3, 5b

    # Replace the 1 GiB high alias containing KERNEL_VMA with a small
    # 4 KiB mapping from the fixed high VMA to the runtime kernel PA.
    # This must happen before jumping to the high-linked Rust entry.
    la t0, kernel_vma_ptr
    ld t0, 0(t0)                  # fixed KERNEL_VMA
    la t1, kernel_image_size_ptr
    ld t1, 0(t1)                  # linked image size

    # root[vpn(KERNEL_VMA, 2)] = early_kernel_l1
    srli t2, t0, 30
    andi t2, t2, 0x1ff
    slli t2, t2, 3
    la t3, early_boot_root
    add t2, t3, t2
    la t4, early_kernel_l1
    srli t4, t4, 12
    slli t4, t4, 10
    ori t4, t4, 1
    sd t4, 0(t2)

    # early_kernel_l1[vpn(KERNEL_VMA, 1)] = early_kernel_l0
    srli t2, t0, 21
    andi t2, t2, 0x1ff
    slli t2, t2, 3
    la t3, early_kernel_l1
    add t2, t3, t2
    la t4, early_kernel_l0
    srli t4, t4, 12
    slli t4, t4, 10
    ori t4, t4, 1
    sd t4, 0(t2)

    # early_kernel_l0[i] = kernel_pa + i * PAGE_SIZE
    la t2, early_kernel_l0
    srli t4, t0, 12
    andi t4, t4, 0x1ff
    li t6, 4095
    add t1, t1, t6
    srli t1, t1, 12
    mv t3, s1
7:
    srli t5, t3, 12
    slli t5, t5, 10
    ori t5, t5, 0xcf             # V|R|W|X|A|D
    slli t6, t4, 3
    add t6, t2, t6
    sd t5, 0(t6)
    li t6, 4096
    add t3, t3, t6
    addi t4, t4, 1
    addi t1, t1, -1
    bnez t1, 7b

.Lkernel_map_done:

    # Activate the temporary root using its physical LMA.
    la t0, early_boot_root
    srli t0, t0, 12
    li t1, 0x8000000000000000     # Sv39 MODE=8
    or t0, t0, t1
    sfence.vma
    csrw satp, t0
    sfence.vma

    # bootstrap_main is high-VMA. Keep its absolute pointer in low rodata
    # so this low trampoline does not need a PC-relative call across the
    # low/high address gap.
    la t0, bootstrap_main_ptr
    ld t0, 0(t0)

    # 传入 kernel pa
    mv a2, s1
    jr t0

3:
    wfi
    j 3b

.section .bss.stack
.align 12

boot_stack:
    .space 4096*4

boot_stack_top:

# Low temporary root, used before Rust can build the FDT-driven root.
.section .bss.early_root
.align 12
.global early_boot_root
early_boot_root:
    .space 4096

.align 12
early_kernel_l1:
    .space 4096

.align 12
early_kernel_l0:
    .space 4096

early_tables_end:

# Fresh high-half stack for the Rust hand-off.
.section .bss.high_stack
.align 12
high_boot_stack:
    .space 4096*4

.global high_boot_stack_top
high_boot_stack_top:

.section .rodata.entry
.align 3
.global bootstrap_main_ptr
bootstrap_main_ptr:
    .dword bootstrap_main

kernel_vma_ptr:
    .dword __bootstrap_start

kernel_image_size_ptr:
    .dword __bootstrap_end - __bootstrap_start
