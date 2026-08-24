.section .text.entry
.globl _start

# OpenSBI:
# a0: hartid
# a1: dtb addr

_start:
    # 非 boot hart（hartid != 0）直接永久 park：
    # 不进 Rust、不碰栈（早期每个 hart 共用 boot_stack_top，若都进 Rust
    # 会踩同一个栈 = UB）。
    beq a0, zero, .Lboot_hart
.Lpark:
    wfi
    j .Lpark

.Lboot_hart:
    la sp, boot_stack_top

    # 清零 .bss（boot hart 专属；副 hart 已 park 不用它）。
    # .bss.stack 位于 __bss_start 之前（linker.ld 先放 *(.bss.stack)），
    # 栈本身不会被清零。
    la t0, __bss_start
    la t1, __bss_end
    bgeu t0, t1, 2f
1:
    sd zero, 0(t0)
    addi t0, t0, 8
    bltu t0, t1, 1b
2:

    call bootstrap_main

3:
    wfi
    j 3b

.section .bss.stack
.align 12

boot_stack:
    .space 4096*4

boot_stack_top: