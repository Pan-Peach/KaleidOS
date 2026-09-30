/* x86_64 trap entry stubs and the common save/dispatch/restore path.
 *
 * Included by `trap/mod.rs` via `global_asm!`.  Layout is ABI with
 * `TrapFrame` in that module: field order == push order, and the offset
 * assertions there fail the build if the two drift.
 *
 * One stub per IDT vector is the boring, standard shape: the vector number is
 * pushed by the stub, the CPU pushes the error code for the ten vectors that
 * have one (8/10/11/12/13/14/17/21/29/30) and the stub pushes a dummy 0 for
 * every other vector, so the frame is uniform.
 */


.section .text

/* ISR_STUB num — push (error_code, vector) and join the common path. */
.macro ISR_STUB num
    .global x86_isr_stub_\num
x86_isr_stub_\num:
    .if \num == 8 || \num == 10 || \num == 11 || \num == 12 || \num == 13 || \num == 14 || \num == 17 || \num == 21 || \num == 29 || \num == 30
        pushq $\num
    .else
        pushq $0
        pushq $\num
    .endif
    jmp x86_trap_common
.endm

/* 256 stubs in eight .irp batches (GAS x86 has no %-expansion in .rept). */
.irp n, 0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31
    ISR_STUB \n
.endr
.irp n, 32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,59,60,61,62,63
    ISR_STUB \n
.endr
.irp n, 64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80,81,82,83,84,85,86,87,88,89,90,91,92,93,94,95
    ISR_STUB \n
.endr
.irp n, 96,97,98,99,100,101,102,103,104,105,106,107,108,109,110,111,112,113,114,115,116,117,118,119,120,121,122,123,124,125,126,127
    ISR_STUB \n
.endr
.irp n, 128,129,130,131,132,133,134,135,136,137,138,139,140,141,142,143,144,145,146,147,148,149,150,151,152,153,154,155,156,157,158,159
    ISR_STUB \n
.endr
.irp n, 160,161,162,163,164,165,166,167,168,169,170,171,172,173,174,175,176,177,178,179,180,181,182,183,184,185,186,187,188,189,190,191
    ISR_STUB \n
.endr
.irp n, 192,193,194,195,196,197,198,199,200,201,202,203,204,205,206,207,208,209,210,211,212,213,214,215,216,217,218,219,220,221,222,223
    ISR_STUB \n
.endr
.irp n, 224,225,226,227,228,229,230,231,232,233,234,235,236,237,238,239,240,241,242,243,244,245,246,247,248,249,250,251,252,253,254,255
    ISR_STUB \n
.endr

/* The common path: save the caller-saved set (plus rbx for scratch), align a
 * call frame, dispatch in Rust, then restore and `iretq`.  Only the timer
 * path returns; fault paths halt inside Rust.
 *
 * Stack at entry: [vector][error_code] on top of the CPU frame.
 * Stack at the Rust call: [rax..rbx] then vector/error/rip/cs/rflags/rsp/ss,
 * which is exactly `TrapFrame` (see trap/mod.rs).
 */
x86_trap_common:
    pushq %rbx
    pushq %r11
    pushq %r10
    pushq %r9
    pushq %r8
    pushq %rdi
    pushq %rsi
    pushq %rdx
    pushq %rcx
    pushq %rax
    movq %rsp, %rbx          /* frame pointer (== &TrapFrame) */
    andq $-16, %rsp          /* System V: 16-byte align before the call */
    movq %rbx, %rdi
    call x86_trap_entry
    movq %rbx, %rsp
    popq %rax
    popq %rcx
    popq %rdx
    popq %rsi
    popq %rdi
    popq %r8
    popq %r9
    popq %r10
    popq %r11
    popq %rbx
    addq $16, %rsp           /* vector + error code */
    iretq

/* Stub address table: IDT entry i points at x86_isr_stub_table[i]. */
.section .rodata
.align 8
.global x86_isr_stub_table
x86_isr_stub_table:
.irp n, 0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31
    .quad x86_isr_stub_\n
.endr
.irp n, 32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,59,60,61,62,63
    .quad x86_isr_stub_\n
.endr
.irp n, 64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80,81,82,83,84,85,86,87,88,89,90,91,92,93,94,95
    .quad x86_isr_stub_\n
.endr
.irp n, 96,97,98,99,100,101,102,103,104,105,106,107,108,109,110,111,112,113,114,115,116,117,118,119,120,121,122,123,124,125,126,127
    .quad x86_isr_stub_\n
.endr
.irp n, 128,129,130,131,132,133,134,135,136,137,138,139,140,141,142,143,144,145,146,147,148,149,150,151,152,153,154,155,156,157,158,159
    .quad x86_isr_stub_\n
.endr
.irp n, 160,161,162,163,164,165,166,167,168,169,170,171,172,173,174,175,176,177,178,179,180,181,182,183,184,185,186,187,188,189,190,191
    .quad x86_isr_stub_\n
.endr
.irp n, 192,193,194,195,196,197,198,199,200,201,202,203,204,205,206,207,208,209,210,211,212,213,214,215,216,217,218,219,220,221,222,223
    .quad x86_isr_stub_\n
.endr
.irp n, 224,225,226,227,228,229,230,231,232,233,234,235,236,237,238,239,240,241,242,243,244,245,246,247,248,249,250,251,252,253,254,255
    .quad x86_isr_stub_\n
.endr
