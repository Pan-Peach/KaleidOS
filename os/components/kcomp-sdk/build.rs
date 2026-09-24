//! build.rs —— 把 freestanding C 分配器（`c/kalloc.c`）编成静态库并链进 crate。
//!
//! 单一真相：分配器实现是 **C**（`docs/architecture/memory-and-heap.md` §6）——
//! C 组件无法链接 Rust，所以 Rust 侧只做 adapter（`src/heap.rs` / `src/alloc.rs`），
//! host `cargo test` 也走这份**真实 C 代码**（不是 Rust 复刻）。
//!
//! 产物：`$OUT_DIR/libkalloc.a`（`cargo:rustc-link-lib=static=kalloc`）。
//! 链接器按需抽取成员：不引用 `kcomp_heap_*` 的组件镜像不会带上它。
//!
//! 旗标与 `tools/build-kcomp-c.sh` 对齐（同一套 `.kcomp` 契约：freestanding、
//! 无 PIC、无 unwind table、`-mno-relax`、`medany`）。host 只做测试用，不加
//! `-fno-pic`（宿主可执行文件默认 PIE）。
//!
//! 无外部 crate：只用 `std::process::Command`。

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("kcomp-sdk: CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("kcomp-sdk: OUT_DIR"));
    let target = env::var("TARGET").expect("kcomp-sdk: TARGET");

    let source = manifest_dir.join("c").join("kalloc.c");
    let include_dir = manifest_dir.join("include");
    let object = out_dir.join("kalloc.o");
    let archive = out_dir.join("libkalloc.a");

    let cc = pick_compiler();
    let mut compile = Command::new(&cc);
    compile
        .arg("-c")
        .arg(&source)
        .arg("-o")
        .arg(&object)
        .arg(format!("-I{}", include_dir.display()));
    for flag in target_flags(&target) {
        compile.arg(flag);
    }
    let status = compile
        .status()
        .unwrap_or_else(|err| panic!("kcomp-sdk: failed to run C compiler {cc}: {err}"));
    if !status.success() {
        panic!(
            "kcomp-sdk: C compiler {cc} failed to compile {} for {target}",
            source.display()
        );
    }

    let ar = env::var("AR").unwrap_or_else(|_| "ar".to_string());
    let status = Command::new(&ar)
        .arg("crs")
        .arg(&archive)
        .arg(&object)
        .status()
        .unwrap_or_else(|err| panic!("kcomp-sdk: failed to run archiver {ar}: {err}"));
    if !status.success() {
        panic!(
            "kcomp-sdk: archiver {ar} failed to create {}",
            archive.display()
        );
    }

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=kalloc");
    println!("cargo:rerun-if-changed=c/kalloc.c");
    println!("cargo:rerun-if-changed=include/kcomp_kalloc.h");
    println!("cargo:rerun-if-env-changed=CC");
    println!("cargo:rerun-if-env-changed=AR");
}

/// 目标旗标：riscv64* / riscv32* 用与 `tools/build-kcomp-c.sh` 一致的交叉旗标；
/// 其余（host 测试）用 freestanding + 警告，不加 `-fno-pic`。
fn target_flags(target: &str) -> Vec<String> {
    let freestanding = [
        "-mno-relax",
        "-mcmodel=medany",
        "-fno-pic",
        "-ffreestanding",
        "-fno-builtin",
        "-fno-stack-protector",
        "-fno-asynchronous-unwind-tables",
        "-fno-unwind-tables",
        "-ffunction-sections",
        "-fdata-sections",
        "-O2",
    ];
    let cross = |clang_target: &str, march: &str, mabi: &str| {
        let mut flags = vec![
            format!("--target={clang_target}"),
            format!("-march={march}"),
            format!("-mabi={mabi}"),
        ];
        flags.extend(freestanding.iter().map(|flag| flag.to_string()));
        flags
    };

    if target.starts_with("riscv64") {
        cross("riscv64-unknown-elf", "rv64gc", "lp64d")
    } else if target.starts_with("riscv32") {
        cross("riscv32-unknown-elf", "rv32imac", "ilp32")
    } else {
        [
            "-ffreestanding",
            "-fno-builtin",
            "-fno-stack-protector",
            "-O2",
            "-Wall",
            "-Wextra",
        ]
        .iter()
        .map(|flag| flag.to_string())
        .collect()
    }
}

/// 编译器选择：`$CC` → `clang` → `cc`；都不工作就带清晰信息失败。
fn pick_compiler() -> String {
    let mut candidates = Vec::new();
    if let Ok(cc) = env::var("CC") {
        candidates.push(cc);
    }
    candidates.push("clang".to_string());
    candidates.push("cc".to_string());

    for candidate in &candidates {
        if candidate.is_empty() {
            continue;
        }
        let works = Command::new(candidate)
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        if works {
            return candidate.clone();
        }
    }
    panic!(
        "kcomp-sdk: no working C compiler (tried $CC, clang, cc); \
         install clang or set CC"
    );
}
