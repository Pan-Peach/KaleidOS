//! 组件 ABI 漂移哨兵：冻结面 + 绝对数值 pin。
//!
//! ABI 的声明本体（`kcore_*` 导出 / 生命周期入口 / 稳定结构 / 组件间 function
//! table / 常量 / 枚举）现在全部由 `abi/*.toml` 单源生成（`tools/kabi/kabi_gen.py`
//! → C / SDK-Rust / Core-Rust，见 `make abi-gen`）。因此本测试**不再做文本交叉
//! 解析比对**，只保留两类独立守卫：
//!
//! 1. **生成物新鲜度**：`make abi-check` 重生成到临时目录并与提交物逐文件 diff
//!    （内容漂移 / 生成文件缺失 / 计划外文件都失败）——"schema ↔ 生成物"的唯一守卫；
//! 2. **布局**：生成物自带编译器断言——C `_Static_assert`（结构大小 / 对齐 /
//!    字段偏移 / 函数表 `N * sizeof(void *)`）与 Rust `const _: () = assert!(...)`
//!    （`size_of` / `align_of` / `offset_of` / `N * size_of::<usize>()`）。字段
//!    个数 / 顺序 / 类型漂移 = 编译错误。
//!
//! 本文件因此**删除**了旧的 C ↔ Rust function table 布局文本比对与常量跨源比对
//! （连同其手写解析器与解析器自测）：那些检查已被上面两条覆盖，且比文本解析更强
//! （编译器背书）。
//!
//! 保留的检查：
//!
//! - **生命周期入口面**：必需导出（`kcomp_instance_create` /
//!   `kcomp_instance_destroy` / `kcomp_abi`）必须在 C 头文件与 Rust 镜像中存在且
//!   形状正确；旧入口 `kcomp_init` / `kcomp_exit` 不得残留（协调替换，无 legacy
//!   fallback；注释里的历史说明会被剥离，代码引用必须删除）；
//! - **绝对数值 pin**：指纹 / 名字 / sector / open-read flag / `InterfaceKind`
//!   编码直接对**编译进来的真实生成常量**断言（`include!`，不是文本解析）——
//!   与生成流程解耦，生成器整体失灵时这些稳定契约值也必须顶着。
//!
//! 作者面文本与生成物都用 `include_str!` / `include!` 在编译期嵌入：文件缺失 =
//! 编译失败；解析不出符号 = 本文件的 sanity / 自测失败。

use std::collections::BTreeMap;

/// C 侧作者面（`AGENTS.md`：C 是根）。路径相对本文件：`os/core/tests/`。
const HEADER_SRC: &str = include_str!("../../components/kcomp-sdk/include/kcomp.h");
/// C 侧生成物（`abi/*.toml` → `kcore_*` / 生命周期 / 稳定结构 / 组件间契约）。
const GENERATED_HEADER_SRC: &str =
    include_str!("../../components/kcomp-sdk/include/generated/kcomp_abi.h");
/// SDK 的 Rust 镜像（手写 facade）。
const SDK_ABI_SRC: &str = include_str!("../../components/kcomp-sdk/src/abi.rs");
/// SDK 的 Rust 生成物（结构 / 常量 / 入口别名 / extern 块）。
const SDK_GENERATED_ABI_SRC: &str = include_str!("../../components/kcomp-sdk/src/generated/abi.rs");
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
// 文本解析（手写、够用即止；只服务于生命周期入口面检查）
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
        "size_t" | "uintptr_t" | "intptr_t" | "ptrdiff_t" | "KcompTaskEntry" | "IrqHandler" => {
            Ok(Width::Ptr)
        }
        "uint64_t" | "int64_t" => Ok(Width::W64),
        "uint32_t" | "int32_t" => Ok(Width::W32),
        "uint16_t" | "int16_t" => Ok(Width::W16),
        "uint8_t" | "int8_t" | "char" | "unsigned char" | "signed char" => Ok(Width::W8),
        "void" => Ok(Width::Unit),
        other => Err(format!("C 类型 `{other}` 未分类；请在分类表补充")),
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

// ---------------------------------------------------------------------------
// 生命周期入口面（冻结契约）
// ---------------------------------------------------------------------------
//
// 名字 / 签名 / 布局的声明本体由 abi/*.toml 单源生成 —— C `_Static_assert`
// （generated/kcomp_abi.h）、Rust `const _: ()` 布局断言（generated/abi.rs）、
// 以及 `component/generated/exports.rs` 的 typed 导出注册表（缺实现 / 签名变了 =
// 编译错误）在编译期覆盖。本测试只钉住"生命周期入口面存在且形状正确"。

#[test]
fn lifecycle_entry_surface_is_frozen() {
    // Phase 2 起声明本体在生成物里（umbrella / facade 只 include / re-export），
    // 因此把两份文本拼起来做"作者面"检查。
    let header = strip_comments(&[HEADER_SRC, GENERATED_HEADER_SRC].concat());
    let sdk = strip_comments(&[SDK_ABI_SRC, SDK_GENERATED_ABI_SRC].concat());
    let core = strip_comments(CORE_EXPORT_SRC);

    // C 侧必需声明与形状（docs/architecture/component-lifecycle.md §4/§6）。
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

/// IRQ 导出面冻结（Phase 4d 的协调 ABI 变更）：四个 `kcore_irq_*` 的 **C 声明**
/// 必须带 `resource_index`（紧跟 `device_id`）——二维锚点
/// `(DeviceId, resource_index)` 是 IRQ 契约的一部分，生成器 / schema 漂回单
/// IRQ 形状时必须响亮失败。
///
/// 这是"签名形状"的指纹：`abi/core.toml` 是唯一来源，生成物与这里逐宽度对齐。
#[test]
fn irq_export_signatures_carry_the_resource_index() {
    let header = strip_comments(&[HEADER_SRC, GENERATED_HEADER_SRC].concat());
    let c = extract_c_decls(&header);

    let register = c
        .get("kcore_irq_register")
        .expect("kcomp_abi.h 必须声明 kcore_irq_register");
    assert_eq!(
        register.params,
        vec![Width::W32, Width::W32, Width::Ptr, Width::Ptr],
        "kcore_irq_register(device_id, resource_index, handler, ctx) 形状漂移"
    );
    assert_eq!(register.ret, Width::W32);

    for name in ["kcore_irq_enable", "kcore_irq_disable", "kcore_irq_release"] {
        let decl = c.get(name).unwrap_or_else(|| panic!("缺少声明 `{name}`"));
        assert_eq!(
            decl.params,
            vec![Width::W32, Width::W32],
            "`{name}(device_id, resource_index)` 形状漂移"
        );
        assert_eq!(decl.ret, Width::W32);
    }
}

// ===========================================================================
// 组件间契约（block.device / filesystem）：编译器背书的绝对数值 pin
// ---------------------------------------------------------------------------
// 契约不是 Core 导出（Core 只把 api/ctx 当不透明指针存着），所以 C 侧名字是
// `kcomp_*`。布局一致性由单源生成 + 生成物里的 `_Static_assert`（C）/
// `const _`（Rust）保证（`make abi-check` 守住生成物新鲜度）；这里只把**稳定
// 数值**钉死：指纹 / 名字 / sector / open-read flag / InterfaceKind 编码。
//
// 下面 `include!` 的是**真实生成物**（与 SDK 编译的是同一份文件），不是文本
// 解析：常量值变了 = 断言失败，类型 / 布局变了 = 编译失败。
// ===========================================================================

mod generated_block {
    include!("../../components/kcomp-sdk/src/generated/block.rs");
}

mod generated_filesystem {
    include!("../../components/kcomp-sdk/src/generated/filesystem.rs");
}

#[test]
fn component_contract_literals_are_pinned() {
    use kernel::component::abi::InterfaceKind;

    // block.device：名字 + 指纹（数值可当 8 字节大端 ASCII 读出来）+ sector 单位。
    assert_eq!(
        generated_block::KCOMP_BLOCK_DEVICE_ABI,
        0x424C_4F43_4B44_4556,
        "block.device ABI 指纹值漂移"
    );
    assert_eq!(
        &generated_block::KCOMP_BLOCK_DEVICE_ABI.to_be_bytes(),
        b"BLOCKDEV",
        "block.device ABI 指纹不再是 ASCII tag"
    );
    assert_eq!(generated_block::KCOMP_BLOCK_DEVICE_NAME, b"block.device");
    assert_eq!(generated_block::KCOMP_BLOCK_DEVICE_SECTOR, 512);

    // Endpoint 调用契约（wire 常量）：contract 身份 + 方法号 + args / output 长度。
    // 消费路径（C 包装 / SDK typed 前端）与 provider 适配器共用同一份生成常量。
    assert_eq!(
        generated_block::KCOMP_BLOCK_DEVICE_CONTRACT,
        0x424C_4B43_4F4E_5452,
        "block.device contract id 漂移"
    );
    assert_eq!(
        &generated_block::KCOMP_BLOCK_DEVICE_CONTRACT.to_be_bytes(),
        b"BLKCONTR",
        "block.device contract id 不再是 ASCII tag"
    );
    assert_eq!(generated_block::KCOMP_BLOCK_METHOD_CAPACITY, 0);
    assert_eq!(generated_block::KCOMP_BLOCK_METHOD_READ, 1);
    assert_eq!(generated_block::KCOMP_BLOCK_METHOD_WRITE, 2);
    assert_eq!(generated_block::KCOMP_BLOCK_LBA_LEN, 8);
    assert_eq!(generated_block::KCOMP_BLOCK_CAPACITY_LEN, 8);

    // filesystem：名字 + 指纹 + 只读 open flag。
    assert_eq!(
        generated_filesystem::KCOMP_FILESYSTEM_ABI,
        0x4653_4E4F_4445_524F,
        "filesystem ABI 指纹值漂移"
    );
    assert_eq!(
        &generated_filesystem::KCOMP_FILESYSTEM_ABI.to_be_bytes(),
        b"FSNODERO",
        "filesystem ABI 指纹不再是 ASCII tag"
    );
    assert_eq!(generated_filesystem::KCOMP_FILESYSTEM_NAME, b"filesystem");
    assert_eq!(
        generated_filesystem::KCOMP_FILESYSTEM_OPEN_READ,
        0x0000_0001
    );

    // filesystem endpoint 调用契约（wire 常量）：contract 身份 + 方法号 + 长度。
    // 与 block 同一约定：ABI 指纹（"FSNODERO"）标识 function table / 扁平编码的
    // 逐位布局，contract 身份（"VFSCONTR"）标识 endpoint 契约——**两个不同的值**。
    assert_eq!(
        generated_filesystem::KCOMP_FILESYSTEM_CONTRACT,
        0x5646_5343_4F4E_5452,
        "filesystem contract id 漂移"
    );
    assert_eq!(
        &generated_filesystem::KCOMP_FILESYSTEM_CONTRACT.to_be_bytes(),
        b"VFSCONTR",
        "filesystem contract id 不再是 ASCII tag"
    );
    assert_eq!(generated_filesystem::KCOMP_FILESYSTEM_METHOD_MOUNT, 0);
    assert_eq!(generated_filesystem::KCOMP_FILESYSTEM_METHOD_UNMOUNT, 1);
    assert_eq!(generated_filesystem::KCOMP_FILESYSTEM_METHOD_OPEN, 2);
    assert_eq!(generated_filesystem::KCOMP_FILESYSTEM_METHOD_CLOSE, 3);
    assert_eq!(generated_filesystem::KCOMP_FILESYSTEM_METHOD_READ, 4);
    assert_eq!(generated_filesystem::KCOMP_FILESYSTEM_METHOD_ROOT, 5);
    assert_eq!(generated_filesystem::KCOMP_FILESYSTEM_METHOD_LOOKUP, 6);
    assert_eq!(generated_filesystem::KCOMP_FILESYSTEM_METHOD_NODE_INFO, 7);
    assert_eq!(generated_filesystem::KCOMP_FILESYSTEM_HANDLE_LEN, 8);
    assert_eq!(generated_filesystem::KCOMP_FILESYSTEM_FLAGS_LEN, 4);
    assert_eq!(generated_filesystem::KCOMP_FILESYSTEM_READ_HEADER_LEN, 8);
    assert_eq!(generated_filesystem::KCOMP_FILESYSTEM_PATH_MAX, 256);
    assert_eq!(generated_filesystem::KCOMP_FILESYSTEM_LOOKUP_ARGS_LEN, 12);
    assert_eq!(generated_filesystem::KCOMP_FILESYSTEM_NAME_MAX, 255);
    assert_eq!(generated_filesystem::KCOMP_FILESYSTEM_ENCODING_BYTES, 1);
    assert_eq!(generated_filesystem::KCOMP_FILESYSTEM_NODE_FILE, 1);
    assert_eq!(generated_filesystem::KCOMP_FILESYSTEM_NODE_DIRECTORY, 2);

    // InterfaceKind 编码（Core 侧真实类型，来自 component.toml 生成物）：
    // ABI 编码 0/1/2，与 docs/architecture/component-model.md §2 一致。
    assert_eq!(InterfaceKind::Device as u32, 0);
    assert_eq!(InterfaceKind::Service as u32, 1);
    assert_eq!(InterfaceKind::Policy as u32, 2);
}

// ===========================================================================
// ABI 错误码（errno）：编译器锚定的数值 pin
// ---------------------------------------------------------------------------
// errno 数值的单一来源是 abi/errno.toml（tools/kabi/kabi_gen.py 生成 Core / SDK /
// C 三份哑 ABI；`make abi-check` 保证三份同源）。这里不再跨源文本比对，只做
// **编译器级**抽查：直接对枚举判别式取 `as i32`，与生成流程解耦——生成器即使
// 整体失灵，这些稳定契约值也必须顶着。
// ===========================================================================

#[test]
fn errno_literals_are_pinned_to_stable_numbers() {
    use kernel::errno::Errno;

    assert_eq!(Errno::ENOENT as i32, 2);
    assert_eq!(Errno::EIO as i32, 5);
    assert_eq!(Errno::EBUSY as i32, 16);
    assert_eq!(Errno::ENODEV as i32, 19);
    assert_eq!(Errno::EINVAL as i32, 22);
    assert_eq!(Errno::EKEYREVOKED as i32, 128);
    assert_eq!(Errno::EINVAL.code(), -22);
}
