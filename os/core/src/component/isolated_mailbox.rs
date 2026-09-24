//! Isolated 跨域 service 的**扁平调用帧邮箱**（increment 6；host-testable 纯逻辑）。
//!
//! 跨 AS 的调用帧**从不共享**：caller 的 `KcompCallFrame` 只描述 caller 域内的
//! 三个 `(ptr, len)`；Core 把它们**拷贝**进 provider 实例自己的邮箱页（Core
//! 拥有的 backing，只映射在该实例的私有 AS 里），再把邮箱内的**实例域 VA** 组成
//! 一个新的 `KcompCallFrame` 交给 provider。provider 因此看不到 caller 的帧、
//! caller 的缓冲或任何 Core 内存；返回时 Core 只把 output 区的前 `output_len`
//! 字节拷回 caller 的 `output` 缓冲。
//!
//! ```text
//! caller 域（Core AS）                     provider 域（私有 AS）
//!   frame.args / input / output  ──拷贝──►  邮箱 args / input / output 区
//!   frame 描述符（caller 栈）     ──构造──►  邮箱描述符（实例内 VA）
//!   output 缓冲                  ◄──拷回──  provider 写入的 output 区
//! ```
//!
//! # 容量与拒绝（绝不截断）
//!
//! 每个负载区容量固定（[`ARGS_MAX`] / [`INPUT_MAX`] / [`OUTPUT_MAX`]）；任一
//! `len` 超过容量 → [`MailboxError::FrameTooLarge`]（ABI 上是 `-EMSGSIZE`），
//! provider **从未执行**，绝不静默截断。
//!
//! # 布局（Core 内部契约；provider 只见 Core 交付的实例内 VA）
//!
//! ```text
//! +0     KcompCallFrame 描述符（6 个指针宽字段）
//! +64    args 拷贝区   （ARGS_MAX）
//! +1088  input 拷贝区  （INPUT_MAX）
//! +2112  output 区     （OUTPUT_MAX）
//! ```
//!
//! 本模块只定义**页内布局**与拷贝方向；邮箱在 provider 域内的 VA 基址与页
//! backing 由 `component/isolated_lifecycle.rs` 决定（`ISOLATED_MAILBOX_BASE`）。

use crate::generated::abi::KcompCallFrame;

/// 描述符在邮箱页内的偏移（页对齐的 backing ⇒ 指针宽对齐）。
pub const FRAME_OFF: usize = 0;
/// args 拷贝区偏移。
pub const ARGS_OFF: usize = 64;
/// args 拷贝区容量（字节）。
pub const ARGS_MAX: usize = 1024;
/// input 拷贝区偏移。
pub const INPUT_OFF: usize = ARGS_OFF + ARGS_MAX;
/// input 拷贝区容量（字节）。
pub const INPUT_MAX: usize = 1024;
/// output 区偏移。
pub const OUTPUT_OFF: usize = INPUT_OFF + INPUT_MAX;
/// output 区容量（字节）。
pub const OUTPUT_MAX: usize = 1024;

/// 邮箱页内实际用到的字节数（布局断言用；页大小由 `isolated_lifecycle` 决定）。
pub const MAILBOX_BYTES: usize = OUTPUT_OFF + OUTPUT_MAX;

/// 邮箱写入 / 校验的拒绝原因（ABI 翻译在 `call.rs`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxError {
    /// 长度非零但指针为空：结构性非法（`endpoint_call` 已在 ABI 边界拒绝；
    /// 这里是纵深防御，绝不把非法 `(ptr, len)` 交给 provider）。
    InvalidFrame,
    /// 任一负载长度超过邮箱容量：显式拒绝（`-EMSGSIZE`），绝不截断。
    FrameTooLarge,
}

/// 一次已写入邮箱的调用帧：provider 侧的实例内 VA。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailboxFrame {
    /// 邮箱里描述符的实例内 VA（`kcomp_service_dispatch` 的 `frame` 参数）。
    pub frame: usize,
    pub args: usize,
    pub args_len: usize,
    pub input: usize,
    pub input_len: usize,
    pub output: usize,
    pub output_len: usize,
}

/// 校验一个 caller frame 放得进邮箱（**任何拷贝之前**）。
pub fn check_frame(frame: &KcompCallFrame) -> Result<(), MailboxError> {
    if (frame.args.is_null() && frame.args_len != 0)
        || (frame.input.is_null() && frame.input_len != 0)
        || (frame.output.is_null() && frame.output_len != 0)
    {
        return Err(MailboxError::InvalidFrame);
    }
    if frame.args_len > ARGS_MAX || frame.input_len > INPUT_MAX || frame.output_len > OUTPUT_MAX {
        return Err(MailboxError::FrameTooLarge);
    }
    Ok(())
}

/// 把 caller frame 的三个负载**拷贝**进邮箱并写入描述符。
///
/// `backing` = 邮箱 backing 的 Core 视图（Core 独占）；`va_base` = 该邮箱在
/// provider 域内的 VA 基址。返回的 [`MailboxFrame`] 是 provider 侧看到的地址，
/// 只用于 Core 侧断言 / 诊断——provider 拿到的是描述符里的同一批 VA。
///
/// # Safety
///
/// `backing` 必须指向该实例邮箱 backing 的起点（Core 独占、至少
/// [`MAILBOX_BYTES`] 字节）；`va_base` 必须与该 backing 在 provider 域内的映射
/// 基址一致。caller 的三个 `(ptr, len)` 必须有效（`endpoint_call` 的 ABI 边界
/// 已校验）。
pub unsafe fn write_frame(
    backing: usize,
    va_base: usize,
    frame: &KcompCallFrame,
) -> Result<MailboxFrame, MailboxError> {
    check_frame(frame)?;
    // SAFETY: 调用方保证 backing 指向邮箱 backing 起点；`check_frame` 已拒绝
    // 超长负载，三个区（+ARGS_OFF / +INPUT_OFF / +OUTPUT_OFF）互不重叠且都在
    // `MAILBOX_BYTES` 内；caller 指针由 ABI 边界保证可读。
    unsafe {
        let base = backing as *mut u8;
        if frame.args_len > 0 {
            core::ptr::copy_nonoverlapping(frame.args, base.add(ARGS_OFF), frame.args_len);
        }
        if frame.input_len > 0 {
            core::ptr::copy_nonoverlapping(frame.input, base.add(INPUT_OFF), frame.input_len);
        }
        // output 区清零：provider 不写时 caller 不会读到上一次调用的残留。
        if frame.output_len > 0 {
            core::ptr::write_bytes(base.add(OUTPUT_OFF), 0, frame.output_len);
        }
        let descriptor = KcompCallFrame {
            args: (va_base + ARGS_OFF) as *const u8,
            args_len: frame.args_len,
            input: (va_base + INPUT_OFF) as *const u8,
            input_len: frame.input_len,
            output: (va_base + OUTPUT_OFF) as *mut u8,
            output_len: frame.output_len,
        };
        core::ptr::write(base.add(FRAME_OFF).cast::<KcompCallFrame>(), descriptor);
    }
    Ok(MailboxFrame {
        frame: va_base + FRAME_OFF,
        args: va_base + ARGS_OFF,
        args_len: frame.args_len,
        input: va_base + INPUT_OFF,
        input_len: frame.input_len,
        output: va_base + OUTPUT_OFF,
        output_len: frame.output_len,
    })
}

/// 把邮箱 output 区的前 `len` 字节拷回 caller 的缓冲。
///
/// # Safety
///
/// 同 [`write_frame`]（`backing` 是邮箱 backing 起点）；`dst` 必须可写 `len`
/// 字节，`len` 必须等于本次 [`write_frame`] 写入的 `output_len`（因此
/// ≤ [`OUTPUT_MAX`]，拷贝不越出 output 区）。
pub unsafe fn read_output(backing: usize, dst: *mut u8, len: usize) {
    if len == 0 {
        return;
    }
    // SAFETY: 见本函数 Safety 段：len ≤ OUTPUT_MAX，源在 output 区内。
    unsafe {
        core::ptr::copy_nonoverlapping((backing + OUTPUT_OFF) as *const u8, dst, len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// 页对齐的测试 backing（Vec<usize> 保证指针宽对齐）。
    fn backing() -> alloc::vec::Vec<usize> {
        vec![0usize; MAILBOX_BYTES.div_ceil(core::mem::size_of::<usize>())]
    }

    const VA_BASE: usize = 0x2200_1000;

    fn frame<'a>(args: &'a [u8], input: &'a [u8], output: &'a mut [u8]) -> KcompCallFrame {
        KcompCallFrame {
            args: args.as_ptr(),
            args_len: args.len(),
            input: input.as_ptr(),
            input_len: input.len(),
            output: output.as_mut_ptr(),
            output_len: output.len(),
        }
    }

    /// 布局：字段按序、不重叠、都在邮箱页内（对 RV32 / RV64 是同一份）。
    #[test]
    fn layout_fields_are_ordered_and_inside_the_mailbox() {
        let fields = [
            (FRAME_OFF, core::mem::size_of::<KcompCallFrame>()),
            (ARGS_OFF, ARGS_MAX),
            (INPUT_OFF, INPUT_MAX),
            (OUTPUT_OFF, OUTPUT_MAX),
        ];
        for pair in fields.windows(2) {
            assert!(
                pair[0].0 + pair[0].1 <= pair[1].0,
                "mailbox fields must not overlap"
            );
        }
        assert_eq!(fields[0].0, 0, "描述符必须在邮箱起点");
        assert_eq!(MAILBOX_BYTES, OUTPUT_OFF + OUTPUT_MAX);
        const { assert!(MAILBOX_BYTES <= 4096, "邮箱布局必须放得进一页") };
    }

    /// 拷贝方向：caller 的 args / input 进邮箱；描述符里的指针全部是**实例域 VA**
    /// （不是 caller 的地址）；output 区被清零等待 provider 写入。
    #[test]
    fn write_frame_copies_payloads_and_publishes_instance_local_pointers() {
        let mut backing = backing();
        let args = [1u8, 2, 3, 4];
        let input = [5u8, 6, 7];
        let mut caller_output = [0xFFu8; 2];
        let caller = frame(&args, &input, &mut caller_output);
        let caller_args_ptr = caller.args as usize;

        // SAFETY: backing 容量 = MAILBOX_BYTES，VA_BASE 与本模块测试约定一致。
        let published =
            unsafe { write_frame(backing.as_mut_ptr() as usize, VA_BASE, &caller) }.unwrap();

        assert_eq!(published.frame, VA_BASE + FRAME_OFF);
        assert_eq!(published.args, VA_BASE + ARGS_OFF);
        assert_eq!(published.input, VA_BASE + INPUT_OFF);
        assert_eq!(published.output, VA_BASE + OUTPUT_OFF);
        assert_ne!(published.args, caller_args_ptr, "绝不共享 caller 地址");
        assert_eq!(published.args_len, args.len());
        assert_eq!(published.input_len, input.len());
        assert_eq!(published.output_len, caller_output.len());

        let bytes = backing.as_ptr() as *const u8;
        // SAFETY: 邮箱布局内；测试单线程。
        unsafe {
            let descriptor =
                &*((backing.as_ptr() as *const u8).add(FRAME_OFF) as *const KcompCallFrame);
            assert_eq!(descriptor.args as usize, VA_BASE + ARGS_OFF);
            assert_eq!(descriptor.input as usize, VA_BASE + INPUT_OFF);
            assert_eq!(descriptor.output as usize, VA_BASE + OUTPUT_OFF);
            let copied_args = core::slice::from_raw_parts(bytes.add(ARGS_OFF), args.len());
            assert_eq!(copied_args, args);
            let copied_input = core::slice::from_raw_parts(bytes.add(INPUT_OFF), input.len());
            assert_eq!(copied_input, input);
            let zeroed = core::slice::from_raw_parts(bytes.add(OUTPUT_OFF), caller_output.len());
            assert_eq!(zeroed, [0u8; 2], "output 区必须先清零");
        }
        assert_eq!(caller_output, [0xFFu8; 2], "caller 缓冲在调用前不被改写");
    }

    /// 拷回：provider 写进邮箱 output 区 → Core 拷回 caller 的缓冲，长度恰好
    /// 是 caller 声明的 `output_len`。
    #[test]
    fn read_output_copies_exactly_the_declared_length() {
        let mut backing = backing();
        let args = [0u8; 0];
        let input = [0u8; 0];
        let mut caller_output = [0xEEu8; 4];
        let caller = frame(&args, &input, &mut caller_output);
        // SAFETY: 同上。
        unsafe { write_frame(backing.as_mut_ptr() as usize, VA_BASE, &caller) }.unwrap();

        // provider 侧写入：模拟 dispatcher 往 output 区写 3 字节。
        // SAFETY: output 区在邮箱内；测试单线程。
        unsafe {
            let out = (backing.as_mut_ptr() as *mut u8).add(OUTPUT_OFF);
            out.write(0xA1);
            out.add(1).write(0xA2);
            out.add(2).write(0xA3);
            out.add(3).write(0xA4); // 第 4 字节：caller 只声明了 4 字节，应被拷回
        }
        // SAFETY: len = caller.output_len ≤ OUTPUT_MAX。
        unsafe { read_output(backing.as_ptr() as usize, caller_output.as_mut_ptr(), 4) };
        assert_eq!(caller_output, [0xA1, 0xA2, 0xA3, 0xA4]);
    }

    /// 超容量：显式拒绝（不截断、不拷贝）。
    #[test]
    fn oversized_payloads_are_rejected_not_truncated() {
        let mut backing = backing();
        let big = vec![0x5Au8; ARGS_MAX + 1];
        let small = [0u8; 1];
        let mut output = [0u8; 1];
        let caller = KcompCallFrame {
            args: big.as_ptr(),
            args_len: big.len(),
            input: small.as_ptr(),
            input_len: small.len(),
            output: output.as_mut_ptr(),
            output_len: output.len(),
        };
        // SAFETY: backing 容量足够；校验在拷贝之前。
        assert_eq!(
            unsafe { write_frame(backing.as_mut_ptr() as usize, VA_BASE, &caller) },
            Err(MailboxError::FrameTooLarge)
        );
        assert_eq!(check_frame(&caller), Err(MailboxError::FrameTooLarge));

        // input / output 超容量同样拒绝。
        let caller = KcompCallFrame {
            args: small.as_ptr(),
            args_len: 1,
            input: big.as_ptr(),
            input_len: big.len(),
            output: output.as_mut_ptr(),
            output_len: 1,
        };
        assert_eq!(check_frame(&caller), Err(MailboxError::FrameTooLarge));
        let caller = KcompCallFrame {
            args: small.as_ptr(),
            args_len: 1,
            input: small.as_ptr(),
            input_len: 1,
            output: big.as_ptr() as *mut u8,
            output_len: big.len(),
        };
        assert_eq!(check_frame(&caller), Err(MailboxError::FrameTooLarge));

        // 邮箱未被触碰（拒绝发生在任何拷贝之前）。
        let bytes = backing.as_ptr() as *const u8;
        // SAFETY: 邮箱布局内。
        unsafe {
            assert_eq!(*bytes.add(ARGS_OFF), 0, "拒绝的调用绝不写邮箱");
            assert_eq!(*bytes.add(OUTPUT_OFF), 0);
        }
    }

    /// 结构性非法：长度非零配空指针 → `InvalidFrame`（纵深防御）。
    #[test]
    fn null_pointer_with_nonzero_length_is_structurally_invalid() {
        let mut output = [0u8; 1];
        let caller = KcompCallFrame {
            args: core::ptr::null(),
            args_len: 1,
            input: core::ptr::null(),
            input_len: 0,
            output: output.as_mut_ptr(),
            output_len: 1,
        };
        assert_eq!(check_frame(&caller), Err(MailboxError::InvalidFrame));
        let caller = KcompCallFrame {
            args: core::ptr::null(),
            args_len: 0,
            input: core::ptr::null(),
            input_len: 0,
            output: core::ptr::null_mut(),
            output_len: 3,
        };
        assert_eq!(check_frame(&caller), Err(MailboxError::InvalidFrame));
    }

    /// 空 frame（三个长度全 0）合法：描述符仍然成立，provider 拿到三个空指针
    /// + 实例域 VA（地址本身由 Core 交付，provider 不应解引用空负载）。
    #[test]
    fn empty_frame_is_accepted() {
        let mut backing = backing();
        let caller = KcompCallFrame {
            args: core::ptr::null(),
            args_len: 0,
            input: core::ptr::null(),
            input_len: 0,
            output: core::ptr::null_mut(),
            output_len: 0,
        };
        // SAFETY: 空负载 + 容量足够的 backing。
        let published =
            unsafe { write_frame(backing.as_mut_ptr() as usize, VA_BASE, &caller) }.unwrap();
        assert_eq!(published.args_len, 0);
        assert_eq!(published.output_len, 0);
    }
}
