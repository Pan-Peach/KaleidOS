/*
 * aarch64 boot entry (QEMU `virt`, `-kernel <elf>`).
 *
 * QEMU contract: PC = ELF entry, x0 = DTB physical address, MMU off, no stack.
 * The image is linked identity (VMA == LMA == PA at 0x40000000), so linker
 * symbols can be dereferenced directly and no page tables are needed.
 */

.section .text.entry
.global _start
.type _start, %function

_start:
    /* All exceptions masked while the vector table / stack come up. */
    msr daifset, #0xf

    /* Initial stack from the linker-defined .bss.stack region. */
    adrp x1, __boot_stack_top
    add  x1, x1, :lo12:__boot_stack_top
    mov  sp, x1

    /* Zero .bss so Rust statics start defined.  x1 = cursor, x2 = end. */
    adrp x1, __bss_start
    add  x1, x1, :lo12:__bss_start
    adrp x2, __bss_end
    add  x2, x2, :lo12:__bss_end
1:
    cmp  x1, x2
    b.hs 2f
    str  xzr, [x1], #8
    b    1b
2:

    /* x0 still holds the DTB pointer: hand off to Rust, never returns. */
    b    bootstrap_main

.size _start, . - _start
