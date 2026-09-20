//! 三方 ABI 漂移哨兵：`kcomp.h` ↔ SDK `abi.rs` ↔ Core `export.rs`。
//!
//! 纯文本交叉校验（无 bindgen / cbindgen / 代码生成器），任何漂移必须**响亮
//! 失败**，绝不静默跳过：
//!
//! 1. **名字集合**：三份文件声明的 `kcore_*` 集合必须逐一相等；
//! 2. **签名形状**：每个符号的参数个数 + **宽度类别**序列（以及返回类别）必须
//!    相等——`size_t` / `usize` / 裸指针统一抽象成"指针宽"（RV32/RV64 同抽象）；
//! 3. **生命周期入口**：必需导出（`kcomp_instance_create` /
//!    `kcomp_instance_destroy` / `kcomp_abi`）必须在 C 头文件与 Rust 镜像中
//!    存在且形状正确；旧入口 `kcomp_init` / `kcomp_exit` 不得残留（协调替换，
//!    无 legacy fallback；注释里的历史说明会被剥离，代码引用必须删除）。
//!
//! 三份文件用 `include_str!` 在编译期嵌入：文件缺失 = 编译失败；解析不出符号
//! = 本文件的 sanity / 自测失败。任何一方不一致 → 测试 panic 并把差异列全。

use std::collections::{BTreeMap, BTreeSet};

/// C 侧作者面（`AGENTS.md`：C 是根）。路径相对本文件：`os/core/tests/`。
const HEADER_SRC: &str = include_str!("../../components/kcomp-sdk/include/kcomp.h");
/// SDK 的 Rust 镜像。
const SDK_ABI_SRC: &str = include_str!("../../components/kcomp-sdk/src/abi.rs");
/// Core 的强制点（EXPORT_SYMBOL 白名单表 + 真实函数签名）。
const CORE_EXPORT_SRC: &str = include_str!("../src/component/export.rs");

/// 跨语言宽度类别：具体拼写不同（`size_t` vs `usize`），宽度与语义必须相同。
/// 不区分 RV32/RV64——指针宽是一个抽象类别，两个 profile 都成立。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Width {
    /// `void` / `()`（只在返回位置出现）。
    Unit,
    /// 指针宽：裸指针、`size_t` / `usize`、函数指针别名（`*Entry` / `*Handler`）。
    Ptr,
    W64,
    W32,
    W16,
    W8,
}

/// 一个导出符号的签名形状。
#[derive(Clone, Debug, PartialEq, Eq)]
struct Signature {
    params: Vec<Width>,
    ret: Width,
}

// ---------------------------------------------------------------------------
// 文本解析（手写、够用即止；不是通用 C/Rust parser）
// ---------------------------------------------------------------------------

/// 剥离 `//` 行注释与 `/* */` 块注释；双引号字符串**原样保留**（导出表里
/// `b"kcore_..."` 是名字来源）。换行保留，出错信息才能指到行。
fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '/' && chars.peek() == Some(&'/') {
            chars.next();
            for c in chars.by_ref() {
                if c == '\n' {
                    out.push('\n');
                    break;
                }
            }
        } else if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut closed = false;
            while let Some(c) = chars.next() {
                if c == '\n' {
                    out.push('\n');
                }
                if c == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    closed = true;
                    break;
                }
            }
            assert!(closed, "包含未闭合的块注释，无法解析 ABI");
            out.push(' ');
        } else if c == '"' {
            out.push(c);
            let mut escaped = false;
            for c in chars.by_ref() {
                out.push(c);
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn read_ident(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    while end < bytes.len() && is_ident_byte(bytes[end]) {
        end += 1;
    }
    end
}

/// 从 `open`（指向 `(`）找匹配的 `)`（支持嵌套括号，如函数指针参数）。
fn matching_paren(bytes: &[u8], open: usize) -> Result<usize, String> {
    assert_eq!(bytes[open], b'(');
    let mut depth = 0usize;
    for (i, &b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(i);
                }
            }
            _ => {}
        }
    }
    Err("括号不配对".to_string())
}

/// 按顶层逗号切分参数列表（括号内的逗号不算，兼容 `void (*h)(void *ctx)`）。
fn split_top_level(raw: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (i, c) in raw.char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(raw[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    let tail = raw[start..].trim();
    if !tail.is_empty() {
        parts.push(tail.to_string());
    }
    parts
}

fn classify_c_type(raw: &str) -> Result<Width, String> {
    let t = raw.trim();
    if t.contains('*') {
        return Ok(Width::Ptr);
    }
    match t {
        "size_t" | "uintptr_t" | "intptr_t" | "ptrdiff_t" | "KcompTaskEntry" => Ok(Width::Ptr),
        "uint64_t" | "int64_t" => Ok(Width::W64),
        "uint32_t" | "int32_t" => Ok(Width::W32),
        "uint16_t" | "int16_t" => Ok(Width::W16),
        "uint8_t" | "int8_t" | "char" | "unsigned char" | "signed char" => Ok(Width::W8),
        "void" => Ok(Width::Unit),
        other => Err(format!("C 类型 `{other}` 未分类；请在分类表补充")),
    }
}

fn classify_rust_type(raw: &str) -> Result<Width, String> {
    let t = raw.trim();
    if t.contains('*') {
        return Ok(Width::Ptr);
    }
    match t {
        "usize" | "isize" => Ok(Width::Ptr),
        "u64" | "i64" => Ok(Width::W64),
        "u32" | "i32" => Ok(Width::W32),
        "u16" | "i16" => Ok(Width::W16),
        "u8" | "i8" => Ok(Width::W8),
        "()" => Ok(Width::Unit),
        other => {
            // 函数指针别名按值传递 = 指针宽；Core 侧别名以此命名（ComponentEntry 等）。
            let last = other.rsplit("::").next().unwrap_or(other);
            if last.ends_with("Entry") || last.ends_with("Handler") {
                Ok(Width::Ptr)
            } else {
                Err(format!(
                    "Rust 类型 `{other}` 未分类；函数指针别名请以 `*Entry`/`*Handler` 命名，或在分类表补充"
                ))
            }
        }
    }
}

/// C 参数里剥掉尾随的参数名：`uint64_t a` → `uint64_t`；
/// `const uint8_t *ptr` → `const uint8_t *`；抽象声明（`size_t`）保持原样。
fn c_param_type(param: &str) -> &str {
    let p = param.trim();
    let bytes = p.as_bytes();
    let mut name_start = bytes.len();
    while name_start > 0 && is_ident_byte(bytes[name_start - 1]) {
        name_start -= 1;
    }
    if name_start == bytes.len() || name_start == 0 {
        return p; // 无尾随标识符，或整个参数就是标识符（抽象声明）
    }
    let mut before = name_start;
    while before > 0 && bytes[before - 1].is_ascii_whitespace() {
        before -= 1;
    }
    if before == name_start {
        return p; // 名字紧贴类型（无分隔空白），不剥
    }
    let head = p[..before].trim_end();
    if head.is_empty() { p } else { head }
}

fn parse_c_params(raw: &str) -> Result<Vec<Width>, String> {
    let parts = split_top_level(raw);
    if parts.len() == 1 && parts[0] == "void" {
        return Ok(Vec::new());
    }
    parts
        .iter()
        .map(|p| classify_c_type(c_param_type(p)))
        .collect()
}

fn parse_rust_params(raw: &str) -> Result<Vec<Width>, String> {
    split_top_level(raw)
        .iter()
        .map(|p| {
            let (_, ty) = p
                .split_once(':')
                .ok_or_else(|| format!("Rust 参数 `{p}` 缺少 `:`"))?;
            classify_rust_type(ty)
        })
        .collect()
}

/// C 声明名之前的返回类型（单行：`int32_t` / `uint8_t *` / `void`）。
fn c_return_type(before: &str) -> &str {
    let bytes = before.as_bytes();
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    let mut start = end;
    while start > 0 {
        let b = bytes[start - 1];
        if b.is_ascii_alphanumeric() || b == b'_' || b == b'*' || b == b' ' || b == b'\t' {
            start -= 1;
        } else {
            break;
        }
    }
    before[start..end].trim()
}

/// Rust 函数 `)` 之后的返回类型（无 `->` = `()`）。
fn rust_return_type(after: &str) -> &str {
    let rest = after.trim_start();
    match rest.strip_prefix("->") {
        Some(rest) => {
            let end = rest.find(['{', ';']).unwrap_or(rest.len());
            rest[..end].trim()
        }
        None => "()",
    }
}

/// 扫描 C 头文件里所有 `kcore_*` / `kcomp_*` 函数声明（注释需已剥离）。
fn extract_c_decls(stripped: &str) -> BTreeMap<String, Signature> {
    let bytes = stripped.as_bytes();
    let mut out = BTreeMap::new();
    let mut i = 0;
    while i < bytes.len() {
        let name_start = bytes[i] == b'k'
            && (i == 0 || !is_ident_byte(bytes[i - 1]))
            && (stripped[i..].starts_with("kcore_") || stripped[i..].starts_with("kcomp_"));
        if !name_start {
            i += 1;
            continue;
        }
        let name_end = read_ident(bytes, i);
        let name = stripped[i..name_end].to_string();
        let mut j = name_end;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'(' {
            // 不是函数声明（类型定义、`extern const ... kcomp_abi;`、struct tag）。
            i = name_end;
            continue;
        }
        let close = matching_paren(bytes, j).unwrap_or_else(|e| panic!("`{name}`: {e}"));
        let params = parse_c_params(&stripped[j + 1..close])
            .unwrap_or_else(|e| panic!("C `{name}` 参数: {e}"));
        let ret = classify_c_type(c_return_type(&stripped[..i]))
            .unwrap_or_else(|e| panic!("C `{name}` 返回: {e}"));
        assert!(
            out.insert(name.clone(), Signature { params, ret })
                .is_none(),
            "kcomp.h 重复声明 `{name}`"
        );
        i = close + 1;
    }
    out
}

/// 扫描 Rust 源码里所有 `fn kcore_*` 定义 / 声明（注释需已剥离）。
fn extract_rust_decls(stripped: &str) -> BTreeMap<String, Signature> {
    let bytes = stripped.as_bytes();
    let mut out = BTreeMap::new();
    let mut i = 0;
    while i + 2 <= bytes.len() {
        let fn_kw = &bytes[i..i + 2] == b"fn"
            && (i == 0 || !is_ident_byte(bytes[i - 1]))
            && bytes.get(i + 2).is_some_and(u8::is_ascii_whitespace);
        if !fn_kw {
            i += 1;
            continue;
        }
        let mut j = i + 2;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        let name_end = read_ident(bytes, j);
        if !stripped[j..].starts_with("kcore_") {
            i = name_end.max(i + 1);
            continue;
        }
        let name = stripped[j..name_end].to_string();
        let mut k = name_end;
        while k < bytes.len() && bytes[k].is_ascii_whitespace() {
            k += 1;
        }
        if k >= bytes.len() || bytes[k] != b'(' {
            i = name_end;
            continue;
        }
        let close = matching_paren(bytes, k).unwrap_or_else(|e| panic!("`{name}`: {e}"));
        let params = parse_rust_params(&stripped[k + 1..close])
            .unwrap_or_else(|e| panic!("Rust `{name}` 参数: {e}"));
        let ret = classify_rust_type(rust_return_type(&stripped[close + 1..]))
            .unwrap_or_else(|e| panic!("Rust `{name}` 返回: {e}"));
        assert!(
            out.insert(name.clone(), Signature { params, ret })
                .is_none(),
            "Rust 侧重复声明 `{name}`"
        );
        i = close + 1;
    }
    out
}

/// Core `export.rs` 的 `EXPORTS` 表名字（`name: b"kcore_..."`）。
fn extract_export_table_names(stripped: &str) -> BTreeSet<String> {
    const ANCHOR: &str = "name: b\"";
    let mut out = BTreeSet::new();
    let mut rest = stripped;
    while let Some(pos) = rest.find(ANCHOR) {
        let start = pos + ANCHOR.len();
        let end = rest[start..].find('"').expect("导出表名字字面量未闭合");
        out.insert(rest[start..start + end].to_string());
        rest = &rest[start + end + 1..];
    }
    out
}

/// 折叠空白后的子串检查（声明可以换行 / 缩进）。
fn contains_normalized(haystack: &str, needle: &str) -> bool {
    let flat = haystack.split_whitespace().collect::<Vec<_>>().join(" ");
    let needle = needle.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.contains(&needle)
}

/// 按标识符边界查找名字（避免 `kcomp_x` 命中 `xkcomp_x`）。
fn contains_ident(src: &str, ident: &str) -> bool {
    let bytes = src.as_bytes();
    src.match_indices(ident).any(|(pos, _)| {
        let before_ok = pos == 0 || !is_ident_byte(bytes[pos - 1]);
        let after = pos + ident.len();
        let after_ok = after >= bytes.len() || !is_ident_byte(bytes[after]);
        before_ok && after_ok
    })
}

fn only_kcore(map: BTreeMap<String, Signature>) -> BTreeMap<String, Signature> {
    map.into_iter()
        .filter(|(name, _)| name.starts_with("kcore_"))
        .collect()
}

// ---------------------------------------------------------------------------
// 断言
// ---------------------------------------------------------------------------

fn assert_name_sets_equal(
    label_a: &str,
    a: &BTreeSet<String>,
    label_b: &str,
    b: &BTreeSet<String>,
) {
    let only_a: Vec<_> = a.difference(b).cloned().collect();
    let only_b: Vec<_> = b.difference(a).cloned().collect();
    assert!(
        only_a.is_empty() && only_b.is_empty(),
        "kcore_* 名字集合漂移：\n  只在 {label_a}：{only_a:?}\n  只在 {label_b}：{only_b:?}"
    );
}

fn compare_signatures(
    label_a: &str,
    a: &BTreeMap<String, Signature>,
    label_b: &str,
    b: &BTreeMap<String, Signature>,
    problems: &mut Vec<String>,
) {
    for (name, sig_a) in a {
        let Some(sig_b) = b.get(name) else {
            continue; // 名字集合测试单独报告缺失
        };
        if sig_a != sig_b {
            problems.push(format!(
                "  `{name}`: {label_a} = ({:?}) -> {:?}；{label_b} = ({:?}) -> {:?}",
                sig_a.params, sig_a.ret, sig_b.params, sig_b.ret
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// 三方交叉校验
// ---------------------------------------------------------------------------

#[test]
fn header_and_sdk_mirror_declare_the_same_kcore_symbols() {
    let names_c = only_kcore(extract_c_decls(&strip_comments(HEADER_SRC)));
    let names_sdk = only_kcore(extract_rust_decls(&strip_comments(SDK_ABI_SRC)));
    assert!(
        names_c.len() >= 36,
        "kcomp.h 只解析出 {} 个 kcore_* 声明（解析器坏了或声明被删）",
        names_c.len()
    );
    assert!(
        names_sdk.len() >= 36,
        "abi.rs 只解析出 {} 个 kcore_* 声明（解析器坏了或声明被删）",
        names_sdk.len()
    );
    assert_name_sets_equal(
        "kcomp.h",
        &names_c.keys().cloned().collect(),
        "abi.rs",
        &names_sdk.keys().cloned().collect(),
    );
}

#[test]
fn header_and_core_export_table_declare_the_same_kcore_symbols() {
    let header = strip_comments(HEADER_SRC);
    let core = strip_comments(CORE_EXPORT_SRC);
    let names_c: BTreeSet<String> = only_kcore(extract_c_decls(&header)).into_keys().collect();
    let names_core = extract_export_table_names(&core);
    assert!(
        !names_core.is_empty(),
        "export.rs 的 EXPORTS 表解析出 0 个名字（表被改写成了 drift test 不认识的形式？）"
    );
    assert_name_sets_equal("kcomp.h", &names_c, "export.rs", &names_core);

    // 表里的每个名字必须有函数体；每个 `fn kcore_*` 必须登记进表。
    let core_fns = extract_rust_decls(&core);
    let table_without_body: Vec<_> = names_core
        .iter()
        .filter(|name| !core_fns.contains_key(*name))
        .collect();
    assert!(
        table_without_body.is_empty(),
        "export.rs 导出表缺少函数定义：{table_without_body:?}"
    );
    let body_not_in_table: Vec<_> = core_fns
        .keys()
        .filter(|name| !names_core.contains(*name))
        .collect();
    assert!(
        body_not_in_table.is_empty(),
        "export.rs `fn kcore_*` 未登记导出表（组件永远解析不到）：{body_not_in_table:?}"
    );
}

#[test]
fn kcore_signature_shapes_match_across_all_three_files() {
    let c = only_kcore(extract_c_decls(&strip_comments(HEADER_SRC)));
    let r = extract_rust_decls(&strip_comments(SDK_ABI_SRC));
    let e = extract_rust_decls(&strip_comments(CORE_EXPORT_SRC));
    let mut problems = Vec::new();
    compare_signatures("kcomp.h", &c, "abi.rs", &r, &mut problems);
    compare_signatures("kcomp.h", &c, "export.rs", &e, &mut problems);
    compare_signatures("abi.rs", &r, "export.rs", &e, &mut problems);
    assert!(
        problems.is_empty(),
        "kcore_* 签名漂移（参数个数 / 宽度类别 / 返回类别）：\n{}",
        problems.join("\n")
    );
}

#[test]
fn lifecycle_entry_surface_is_frozen() {
    let header = strip_comments(HEADER_SRC);
    let sdk = strip_comments(SDK_ABI_SRC);
    let core = strip_comments(CORE_EXPORT_SRC);

    // C 侧必需声明与形状（docs/component-lifecycle.md §4/§6）。
    let c = extract_c_decls(&header);
    let create = c
        .get("kcomp_instance_create")
        .expect("kcomp.h 必须声明 kcomp_instance_create");
    assert_eq!(
        create.params,
        vec![Width::Ptr, Width::Ptr],
        "kcomp_instance_create(args, out_state) 形状漂移"
    );
    assert_eq!(create.ret, Width::W32);
    let destroy = c
        .get("kcomp_instance_destroy")
        .expect("kcomp.h 必须声明 kcomp_instance_destroy");
    assert_eq!(destroy.params, vec![Width::Ptr]);
    assert_eq!(destroy.ret, Width::W32);
    for needle in [
        "extern const uint64_t kcomp_abi;",
        "struct KcompCreateArgs {",
        "typedef void (*KcompTaskEntry)(void *arg);",
    ] {
        assert!(
            contains_normalized(&header, needle),
            "kcomp.h 缺少冻结声明：`{needle}`"
        );
    }

    // Rust 镜像（宏与类型别名必须存在）。
    for needle in [
        "pub struct KcompCreateArgs",
        "pub const KCOMP_ABI: u64",
        "pub type KcompInstanceCreate",
        "pub type KcompInstanceDestroy",
        "pub type KcompTaskEntry",
    ] {
        assert!(
            contains_normalized(&sdk, needle),
            "abi.rs 缺少 Rust 镜像：`{needle}`"
        );
    }

    // 协调替换：旧入口不得在代码里残留（注释已剥离；无 legacy fallback）。
    for (label, src) in [("kcomp.h", &header), ("abi.rs", &sdk), ("export.rs", &core)] {
        for legacy in ["kcomp_init", "kcomp_exit"] {
            assert!(
                !contains_ident(src, legacy),
                "{label} 仍引用旧生命周期符号 `{legacy}`：协调替换不保留 fallback"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 解析器自测（不依赖三份真实文件的当前状态，保证测试工具本身可信）
// ---------------------------------------------------------------------------

#[test]
fn c_parser_extracts_declaration_shapes() {
    let src = strip_comments(
        "/* kcore_hidden(uint64_t x); */\nint32_t kcore_demo(uint64_t a, const void *b);\n",
    );
    let sigs = extract_c_decls(&src);
    assert_eq!(sigs.len(), 1, "注释里的伪声明必须被剥离");
    assert!(sigs.contains_key("kcore_demo"));
    assert!(!sigs.contains_key("kcore_hidden"));
    assert_eq!(sigs["kcore_demo"].params, vec![Width::W64, Width::Ptr]);
    assert_eq!(sigs["kcore_demo"].ret, Width::W32);
}

#[test]
fn c_parser_handles_void_and_function_pointers() {
    let src = strip_comments(
        "int32_t kcore_a(void);\nint32_t kcore_b(uint64_t h, void (*handler)(void *ctx), void *ctx);\n",
    );
    let sigs = extract_c_decls(&src);
    assert_eq!(sigs["kcore_a"].params, Vec::<Width>::new());
    assert_eq!(
        sigs["kcore_b"].params,
        vec![Width::W64, Width::Ptr, Width::Ptr]
    );
}

#[test]
fn rust_parser_extracts_declaration_shapes() {
    let src = strip_comments(
        "// pub fn kcore_hidden(x: u8) -> u8;\npub fn kcore_demo(a: usize, b: *mut u32) -> i32;\n",
    );
    let sigs = extract_rust_decls(&src);
    assert_eq!(sigs.len(), 1, "注释里的伪声明必须被剥离");
    assert!(sigs.contains_key("kcore_demo"));
    assert!(!sigs.contains_key("kcore_hidden"));
    assert_eq!(sigs["kcore_demo"].params, vec![Width::Ptr, Width::Ptr]);
    assert_eq!(sigs["kcore_demo"].ret, Width::W32);
}

#[test]
fn export_table_parser_reads_name_literals() {
    let src = "static E: [X; 2] = [X { name: b\"kcore_a\", address: f }, X { name: b\"kcore_b\", address: g }];";
    let names = extract_export_table_names(src);
    assert_eq!(
        names.into_iter().collect::<Vec<_>>(),
        vec!["kcore_a".to_string(), "kcore_b".to_string()]
    );
}

#[test]
fn classifiers_reject_unknown_types_and_accept_fn_aliases() {
    assert!(classify_c_type("long double").is_err());
    assert!(classify_rust_type("SomeRandomStruct").is_err());
    assert_eq!(
        classify_rust_type("component::ComponentEntry"),
        Ok(Width::Ptr)
    );
    assert_eq!(classify_rust_type("irq::IrqHandler"), Ok(Width::Ptr));
}
