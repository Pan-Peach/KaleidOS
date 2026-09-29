//! Kconfig → Rust 的窄运输契约：`MAX_CPUS` 的解析与校验。
//!
//! 值由 Makefile 从生成的片段里的 `CONFIG_MAX_CPUS` 传入；映射只在
//! `arch/build.rs` 一处发生（与 `os/core/build.rs` 的 `TRACE_CAPACITY` 同一条
//! 契约）。host 构建（`cargo test`）用显式默认，不发明裸机默认。

/// `MAX_CPUS` 的 host 构建显式默认（与 Kconfig `default` 一致）。
pub const MAX_CPUS_DEFAULT: usize = 8;
/// 合法下界。
pub const MAX_CPUS_MIN: usize = 1;
/// 合法上界。
pub const MAX_CPUS_MAX: usize = 64;

/// `MAX_CPUS` 解析 / 校验失败原因。
#[derive(Debug, PartialEq, Eq)]
pub enum MaxCpusError {
    /// 裸机构建缺 `CONFIG_MAX_CPUS`（不发明默认）。
    Missing,
    /// 不是合法的十进制整数。
    NotAnInteger,
    /// 越界。
    OutOfRange(usize),
}

/// 解析并校验一个 resolved `CONFIG_MAX_CPUS` 值。
///
/// `raw == None`：host 构建（`bare_metal == false`）用 [`MAX_CPUS_DEFAULT`]；
/// 裸机构建报 [`MaxCpusError::Missing`]。
pub fn parse_max_cpus(raw: Option<&str>, bare_metal: bool) -> Result<usize, MaxCpusError> {
    let Some(raw) = raw else {
        return if bare_metal {
            Err(MaxCpusError::Missing)
        } else {
            Ok(MAX_CPUS_DEFAULT)
        };
    };
    let value: usize = raw.trim().parse().map_err(|_| MaxCpusError::NotAnInteger)?;
    if !(MAX_CPUS_MIN..=MAX_CPUS_MAX).contains(&value) {
        return Err(MaxCpusError::OutOfRange(value));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_defaults_when_missing() {
        assert_eq!(parse_max_cpus(None, false), Ok(MAX_CPUS_DEFAULT));
    }

    #[test]
    fn bare_metal_missing_is_error() {
        assert_eq!(parse_max_cpus(None, true), Err(MaxCpusError::Missing));
    }

    #[test]
    fn accepts_in_range_with_whitespace() {
        assert_eq!(parse_max_cpus(Some(" 8 "), true), Ok(8));
        assert_eq!(parse_max_cpus(Some("1"), true), Ok(MAX_CPUS_MIN));
        assert_eq!(parse_max_cpus(Some("64"), true), Ok(MAX_CPUS_MAX));
    }

    #[test]
    fn rejects_out_of_range_and_garbage() {
        assert_eq!(
            parse_max_cpus(Some("0"), true),
            Err(MaxCpusError::OutOfRange(0))
        );
        assert_eq!(
            parse_max_cpus(Some("65"), true),
            Err(MaxCpusError::OutOfRange(65))
        );
        assert_eq!(
            parse_max_cpus(Some("abc"), true),
            Err(MaxCpusError::NotAnInteger)
        );
        assert_eq!(
            parse_max_cpus(Some("-1"), true),
            Err(MaxCpusError::NotAnInteger)
        );
    }
}
