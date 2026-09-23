//! Flat call frame 的**借用视图**与 `unsafe` 原始 frame 适配器。
//!
//! ABI 类型 [`KcompCallFrame`] 不带生命周期；SDK **不**提供
//! `fn decode<'a>(raw: *const KcompCallFrame) -> Call<'a>` 这种"凭空发明任意
//! 生命周期"的安全 API。唯一入口是 [`with_call`]：`unsafe` + **scoped closure**，
//! 借用窗口就是闭包作用域，生命周期由调用点（Core 的 service-call 边界）保证。
//!
//! # 为什么不是安全 fn
//!
//! `KcompCallFrame` 里的三个 `(ptr, len)` 来自组件内存，SDK 无法自证其有效性；
//! 把它们变成切片这件事本身就是 `unsafe`。把 `unsafe` 关在**一个**窄适配器里、
//! 由宏展开代码在 Core 边界内调用，比让每个 handler 自己解 frame 安全得多。

use core::mem::size_of;

use crate::abi::KcompCallFrame;
use crate::errno::Errno;

/// 一次调用的三个借用区：`args` / `input` 只读，`output` 可写。
///
/// 只在服务 dispatcher 的调用窗口内有效（由 [`with_call`] 的闭包作用域限定，
/// 不能逃逸）。
pub struct Call<'call> {
    /// 标量参数区（SDK 编解码；Core 视为不透明字节）。
    pub args: &'call [u8],
    /// 输入负载（只读）；无负载时为空切片。
    pub input: &'call [u8],
    /// 输出负载（可写）；provider 写入量不得超过 `output.len()`。
    pub output: &'call mut [u8],
}

/// 在 frame 的借用窗口内调用 `f`；frame 结构非法 → `Err(EINVAL)`，`f` 不被调用。
///
/// 这是宏展开代码（[`crate::kcomp_services!`]）与 SDK 内部使用的**原始适配器**，
/// 不是业务 API（`#[doc(hidden)]` + `unsafe`）。
///
/// # 校验规则（每条都是 UB 防线）
///
/// - 长度 0 的区**允许空指针**，归一化为空切片（**绝不** `from_raw_parts(NULL, 0)`）；
/// - 长度非 0 时指针必须非空，且长度必须放得进 `isize::MAX`（切片总大小限制）；
/// - `output` 不得与 `args` / `input` / frame 自身重叠（`&mut` 必须独占）；
/// - `args` 与 `input` 之间可以重叠（两者都是只读借用）。
///
/// # Safety
///
/// 调用方保证 `frame` 指向一个有效、已初始化的 [`KcompCallFrame`]，且它描述的
/// 内存区在闭包执行期间按 `(ptr, len)` 规则有效（Core 的 service-call 边界正是
/// 这样保证的）。闭包不得把 `Call` 里的借用逃逸出去。
#[doc(hidden)]
pub unsafe fn with_call<R>(
    frame: *const KcompCallFrame,
    f: impl FnOnce(Call<'_>) -> R,
) -> Result<R, Errno> {
    if frame.is_null() {
        return Err(Errno::EINVAL);
    }
    // SAFETY: 调用方保证 frame 有效（见函数 Safety）。
    let frame = unsafe { &*frame };

    // 先做**纯地址算术**校验，再构造任何切片——重叠的 `&mut` 一旦构造出来就已经
    // 违反别名规则，不能"构造后再检查"。
    let args_range = checked_range(frame.args as usize, frame.args_len).ok_or(Errno::EINVAL)?;
    let input_range = checked_range(frame.input as usize, frame.input_len).ok_or(Errno::EINVAL)?;
    let output_range =
        checked_range(frame.output as usize, frame.output_len).ok_or(Errno::EINVAL)?;
    let frame_start = frame as *const KcompCallFrame as usize;
    let frame_range = (frame_start, frame_start + size_of::<KcompCallFrame>());
    if overlaps(output_range, args_range)
        || overlaps(output_range, input_range)
        || overlaps(output_range, frame_range)
    {
        return Err(Errno::EINVAL);
    }

    // SAFETY: 上面已保证：非零长度 → 指针非空且总大小 ≤ isize::MAX；零长度 →
    // 归一化为空切片（不触碰指针，null 也安全）。调用方保证区内内存有效。
    let args: &[u8] = if frame.args_len == 0 {
        &[]
    } else {
        unsafe { core::slice::from_raw_parts(frame.args, frame.args_len) }
    };
    let input: &[u8] = if frame.input_len == 0 {
        &[]
    } else {
        unsafe { core::slice::from_raw_parts(frame.input, frame.input_len) }
    };
    // SAFETY: 同上；`output` 与 args / input / frame 不重叠，独占有效。
    let output: &mut [u8] = if frame.output_len == 0 {
        &mut []
    } else {
        unsafe { core::slice::from_raw_parts_mut(frame.output, frame.output_len) }
    };
    Ok(f(Call {
        args,
        input,
        output,
    }))
}

/// `(start, end)` 半开区间；零长度归一化为空区间（指针值不参与）。
/// `None` = 长度非 0 但指针为空，或长度 > `isize::MAX`，或地址回绕。
fn checked_range(ptr: usize, len: usize) -> Option<(usize, usize)> {
    if len == 0 {
        return Some((0, 0));
    }
    if ptr == 0 || len > isize::MAX as usize {
        return None;
    }
    Some((ptr, ptr.checked_add(len)?))
}

/// 半开区间重叠（空区间永不重叠）。
fn overlaps(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_with(
        args: *const u8,
        args_len: usize,
        input: *const u8,
        input_len: usize,
        output: *mut u8,
        output_len: usize,
    ) -> KcompCallFrame {
        KcompCallFrame {
            args,
            args_len,
            input,
            input_len,
            output,
            output_len,
        }
    }

    /// 合法 frame：三个区各自独立，闭包看到正确字节、可写 output。
    #[test]
    fn valid_frame_yields_borrowed_regions() {
        let args = [1u8, 2, 3];
        let input = [4u8, 5];
        let mut output = [0u8; 2];
        let frame = frame_with(
            args.as_ptr(),
            args.len(),
            input.as_ptr(),
            input.len(),
            output.as_mut_ptr(),
            output.len(),
        );

        // SAFETY: frame 与本帧内三个数组一致，闭包不逃逸借用。
        let seen = unsafe {
            with_call(&frame, |call| {
                assert_eq!(call.args, [1, 2, 3]);
                assert_eq!(call.input, [4, 5]);
                call.output.copy_from_slice(&[9, 8]);
            })
        };
        assert!(seen.is_ok());
        assert_eq!(output, [9, 8]);
    }

    /// 零长度区允许空指针 → 空切片（**不** `from_raw_parts(NULL, 0)`）。
    #[test]
    fn zero_length_regions_normalize_to_empty_slices() {
        let frame = frame_with(
            core::ptr::null(),
            0,
            core::ptr::null(),
            0,
            core::ptr::null_mut(),
            0,
        );
        // SAFETY: 所有区长度 0，适配器不触碰任何指针。
        let result = unsafe {
            with_call(&frame, |call| {
                assert!(call.args.is_empty());
                assert!(call.input.is_empty());
                assert!(call.output.is_empty());
                0i32
            })
        };
        assert_eq!(result, Ok(0));
    }

    /// 非零长度 + 空指针 / 长度超过 `isize::MAX` → `EINVAL`，闭包不被调用。
    #[test]
    fn malformed_regions_are_rejected_without_calling_the_closure() {
        let mut never = false;
        let mut buf = [0u8; 1];

        let null_args = frame_with(
            core::ptr::null(),
            1,
            core::ptr::null(),
            0,
            buf.as_mut_ptr(),
            1,
        );
        // SAFETY: 适配器必须在解引用前拒绝该 frame。
        let r = unsafe { with_call(&null_args, |_| never = true) };
        assert_eq!(r, Err(Errno::EINVAL));
        assert!(!never);

        let null_output = frame_with(
            buf.as_ptr(),
            1,
            core::ptr::null(),
            0,
            core::ptr::null_mut(),
            1,
        );
        // SAFETY: 同上。
        let r = unsafe { with_call(&null_output, |_| never = true) };
        assert_eq!(r, Err(Errno::EINVAL));
        assert!(!never);

        let overflow = frame_with(
            buf.as_ptr(),
            usize::MAX,
            core::ptr::null(),
            0,
            buf.as_mut_ptr(),
            1,
        );
        // SAFETY: 同上。
        let r = unsafe { with_call(&overflow, |_| never = true) };
        assert_eq!(r, Err(Errno::EINVAL));
        assert!(!never);
    }

    /// `output` 与 `args` / `input` / frame 自身重叠 → `EINVAL`（`&mut` 独占）。
    #[test]
    fn output_overlapping_readable_regions_is_rejected() {
        let mut buf = [0u8; 8];
        let ptr = buf.as_mut_ptr();

        // output 与 args 重叠。
        let alias_args = frame_with(ptr, 4, core::ptr::null(), 0, ptr, 4);
        // SAFETY: 适配器必须在构造切片前拒绝重叠。
        assert_eq!(
            unsafe { with_call(&alias_args, |_| ()) },
            Err(Errno::EINVAL)
        );

        // output 与 input 重叠。
        let alias_input = frame_with(core::ptr::null(), 0, ptr, 4, ptr, 4);
        // SAFETY: 同上。
        assert_eq!(
            unsafe { with_call(&alias_input, |_| ()) },
            Err(Errno::EINVAL)
        );

        // output 覆盖 frame 结构自身。
        let mut frame = frame_with(
            core::ptr::null(),
            0,
            core::ptr::null(),
            0,
            core::ptr::null_mut(),
            0,
        );
        let frame_ptr = &mut frame as *mut KcompCallFrame as *mut u8;
        frame.output = frame_ptr;
        frame.output_len = 4;
        // SAFETY: 同上。
        assert_eq!(unsafe { with_call(&frame, |_| ()) }, Err(Errno::EINVAL));
    }

    /// 空 frame 指针 → `EINVAL`（不 UB）。
    #[test]
    fn null_frame_pointer_is_rejected() {
        // SAFETY: 适配器必须拒绝空 frame。
        assert_eq!(
            unsafe { with_call(core::ptr::null(), |_| ()) },
            Err(Errno::EINVAL)
        );
    }
}
