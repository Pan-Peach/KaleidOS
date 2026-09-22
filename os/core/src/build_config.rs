//! Kconfig → Rust 的窄运输契约：`TRACE_CAPACITY` 的解析与校验。
//!
//! `build.rs` 与 kernel lib 的 host test 共用本文件（build script 侧用
//! `#[path]` 引入）：build.rs 只做"读环境变量 → 校验 → 写 OUT_DIR 常量"，
//! **不读 `.config`**，也不重新实现 Kconfig 的默认 / `range` 语义（这里的
//! 范围只是防御性再校验，Kconfig 仍是唯一真相，见 docs/architecture/kconfig.md）。
//!
//! 值由 Makefile 从生成的片段里的 `CONFIG_TRACE_CAPACITY` 传入；映射只在
//! genmk.py 一处。

/// `TRACE_CAPACITY` 的 host 构建显式默认（与 Kconfig `default` 一致）。
pub const TRACE_CAPACITY_DEFAULT: usize = 1024;
/// Kconfig `range 64 8192` 的下界。
pub const TRACE_CAPACITY_MIN: usize = 64;
/// Kconfig `range 64 8192` 的上界。
pub const TRACE_CAPACITY_MAX: usize = 8192;

/// 拒绝一个值时说明原因；**绝不静默退回默认值**（那会让错误配置伪装成成功）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceCapacityError {
    /// 裸机构建必须显式给出：Kconfig 是唯一真相，build.rs 不发明默认。
    Missing,
    /// 不是十进制整数。
    NotANumber,
    /// 超出 Kconfig 的 range。
    OutOfRange,
}

/// 解析并校验一个 resolved `CONFIG_TRACE_CAPACITY` 值。
///
/// `required = true`（裸机构建）：缺失即错误；`false`（host test / clippy）：
/// 缺失用 [`TRACE_CAPACITY_DEFAULT`]。
pub fn parse_trace_capacity(
    raw: Option<&str>,
    required: bool,
) -> Result<usize, TraceCapacityError> {
    let Some(raw) = raw else {
        return if required {
            Err(TraceCapacityError::Missing)
        } else {
            Ok(TRACE_CAPACITY_DEFAULT)
        };
    };
    let value: usize = raw.parse().map_err(|_| TraceCapacityError::NotANumber)?;
    if !(TRACE_CAPACITY_MIN..=TRACE_CAPACITY_MAX).contains(&value) {
        return Err(TraceCapacityError::OutOfRange);
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 边界值包含在 range 内（`range` 两端都合法）。
    #[test]
    fn explicit_values_at_the_range_bounds_are_accepted() {
        assert_eq!(parse_trace_capacity(Some("64"), true), Ok(64));
        assert_eq!(parse_trace_capacity(Some("1024"), true), Ok(1024));
        assert_eq!(parse_trace_capacity(Some("8192"), true), Ok(8192));
    }

    /// 缺失值：host 构建用显式默认，裸机构建报错（不允许发明默认）。
    #[test]
    fn missing_value_uses_the_host_default_only_when_not_required() {
        assert_eq!(
            parse_trace_capacity(None, false),
            Ok(TRACE_CAPACITY_DEFAULT)
        );
        assert_eq!(
            parse_trace_capacity(None, true),
            Err(TraceCapacityError::Missing)
        );
    }

    /// 越界（含 0）一律拒绝。
    #[test]
    fn out_of_range_is_rejected() {
        assert_eq!(
            parse_trace_capacity(Some("0"), true),
            Err(TraceCapacityError::OutOfRange)
        );
        assert_eq!(
            parse_trace_capacity(Some("63"), true),
            Err(TraceCapacityError::OutOfRange)
        );
        assert_eq!(
            parse_trace_capacity(Some("8193"), true),
            Err(TraceCapacityError::OutOfRange)
        );
    }

    /// 非十进制 / 负数 / 空串不是"默认值"，是错误。
    #[test]
    fn non_decimal_is_rejected() {
        assert_eq!(
            parse_trace_capacity(Some(""), true),
            Err(TraceCapacityError::NotANumber)
        );
        assert_eq!(
            parse_trace_capacity(Some("1k"), true),
            Err(TraceCapacityError::NotANumber)
        );
        assert_eq!(
            parse_trace_capacity(Some("-1"), true),
            Err(TraceCapacityError::NotANumber)
        );
    }
}
