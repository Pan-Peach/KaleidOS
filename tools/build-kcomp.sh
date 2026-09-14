#!/usr/bin/env bash
# build-kcomp.sh —— 把一个组件构建成**链接后的** .kcomp（ET_REL 组件程序）。
#
# 单一构建管线：Makefile 与 os/core/build.rs 都调用本脚本，避免两条路径漂移。
#
#   component wrapper + third-party/SDK deps
#     → staticlib（私有携带所有依赖）
#     → rust-lld -r --gc-sections -u kcomp_init（+ 定义了 kcomp_exit 时一并 -u）
#       （partial link + 段 GC + 保留入口）
#     → llvm-objcopy --strip-debug（去调试/元数据）
#     → .kcomp（ET_REL；UNDEF 只允许白名单 kcore_*；kcomp_init DEFINED，
#               kcomp_exit 可选 DEFINED）
#
# 用法：
#   tools/build-kcomp.sh <component-dir> <target> <output> [<target-dir>]
#
#   component-dir  含 Cargo.toml 的组件目录（manifest 需声明 crate-type=["staticlib"]）
#   target         rust target triple（riscv64gc-... / riscv32imac-...）
#   output         输出的 .kcomp 路径
#   target-dir     cargo 产物目录（默认 <component-dir>/target）
#
# 包名从 component-dir/Cargo.toml 读取（目录名可与包名不同，如 drivers/virtio_blk
# 的包名是 kcomp_virtio_blk）——避免构建脚本与 cargo 包名强耦合。

set -euo pipefail

if [ "$#" -lt 3 ] || [ "$#" -gt 4 ]; then
    echo "usage: $0 <component-dir> <target> <output> [<target-dir>]" >&2
    exit 2
fi

component_dir=$1
target=$2
output=$3
target_dir=${4:-"$component_dir/target"}

# 包名（Cargo.toml [package] 的第一个 name）= staticlib 文件名 lib<pkg>.a。
pkg=$(sed -n 's/^[[:space:]]*name[[:space:]]*=[[:space:]]*"\([^"]*\)"/\1/p' "$component_dir/Cargo.toml" | head -1)
if [ -z "$pkg" ]; then
    echo "build-kcomp: cannot read package name from $component_dir/Cargo.toml" >&2
    exit 1
fi
lib_name=${pkg//-/_}

cargo_bin=${CARGO:-cargo}
rustc_bin=${RUSTC:-rustc}
objcopy=${OBJCOPY:-llvm-objcopy}
readelf=${READELF:-llvm-readelf}

# 1) 组件 = staticlib：SDK / third-party 依赖随镜像私有携带（不建 shared runtime）。
"$cargo_bin" build --release \
    --manifest-path "$component_dir/Cargo.toml" \
    --target "$target" \
    --target-dir "$target_dir"

lib="$target_dir/$target/release/lib$lib_name.a"
if [ ! -f "$lib" ]; then
    echo "build-kcomp: staticlib not found: $lib" >&2
    echo "build-kcomp: 组件 manifest 是否声明了 [lib] crate-type = [\"staticlib\"]？" >&2
    exit 1
fi

# 2) partial link：只抽可达成员、GC 未引用段、-u 钉住加载入口。
#    --no-relax 避免 R_RISCV_ALIGN（loader 不支持；relax 在本步骤无收益）。
#
#    组件可以**可选**导出退出入口 kcomp_exit（Linux module_exit 类比）。它在
#    staticlib 里是独立 archive member，若不显式 -u 就会被归档抽取/GC 丢掉；
#    只有定义时才钉住——未定义该符号的组件保持 UNDEF 白名单干净（loader 按
#    可选解析，见 os/core/src/component/loader.rs）。
sysroot=$("$rustc_bin" --print sysroot)
host=$("$rustc_bin" -vV | sed -n 's/^host: //p')
lld="$sysroot/lib/rustlib/$host/bin/rust-lld"
if [ ! -x "$lld" ]; then
    echo "build-kcomp: rust-lld not found: $lld" >&2
    exit 1
fi

tmp=$(mktemp "${TMPDIR:-/tmp}/kcomp.XXXXXX.o")
trap 'rm -f "$tmp"' EXIT

force=(-u kcomp_init)
if "$readelf" -s "$lib" | awk '$4=="FUNC" && $8=="kcomp_exit" && $7!="UND" {found=1} END{exit !found}'; then
    force+=(-u kcomp_exit)
fi

"$lld" -flavor gnu -r --gc-sections --no-relax "${force[@]}" -o "$tmp" "$lib"

# 3) strip：去调试段 + LTO bitcode（loader 不需要，且体积巨大）。
mkdir -p "$(dirname "$output")"
"$objcopy" --strip-debug --remove-section=.llvmbc --remove-section=.llvmcmd "$tmp" "$output"

# 4) 契约校验（load 前就失败，别把坏镜像带进 kpkg）。
if ! "$readelf" -h "$output" | grep -q 'REL (Relocatable file)'; then
    echo "build-kcomp: $output is not ET_REL" >&2
    exit 1
fi

if ! "$readelf" -s "$output" | awk '$4=="FUNC" && $8=="kcomp_init" && $7!="UND" {found=1} END{exit !found}'; then
    echo "build-kcomp: kcomp_init (STT_FUNC) not defined in $output" >&2
    exit 1
fi

# UNDEF 只允许 kcore_*（与 Core export.rs 白名单精确对齐）。
undef=$("$readelf" -s "$output" | awk '$7=="UND" && $8!="" {print $8}' | sort -u)
bad=$(printf '%s\n' "$undef" | grep -v '^kcore_' || true)
if [ -n "$bad" ]; then
    echo "build-kcomp: non-kcore undefined symbols in $output:" >&2
    printf '  %s\n' $bad >&2
    exit 1
fi

# 只允许 loader 已支持的 RISC-V 重定位类型；特别禁止 R_RISCV_ALIGN。
#" Allow R_RISCV: 0 NONE, 1/2 32/64, 18/19 CALL/CALL_PLT, 20 32_PCREL,
#  23/24/25 PCREL_HI20/LO12_I/LO12_S, 26/27/28 HI20/LO12_I/LO12_S, 51 RELAX.
if "$readelf" -r "$output" | grep -q 'R_RISCV_ALIGN'; then
    echo "build-kcomp: R_RISCV_ALIGN present (loader 不支持) in $output" >&2
    exit 1
fi
unknown=$( \
    "$readelf" -r "$output" | awk '/R_RISCV/{print $3}' | sort -u | \
    grep -vE '^R_RISCV_(NONE|32|64|CALL|CALL_PLT|32_PCREL|PCREL_HI20|PCREL_LO12_I|PCREL_LO12_S|HI20|LO12_I|LO12_S|RELAX)$' || true)
if [ -n "$unknown" ]; then
    echo "build-kcomp: unsupported relocation type(s) in $output:" >&2
    printf '  %s\n' $unknown >&2
    exit 1
fi

echo "kcomp: $output (${undef:-no undefined symbols})"
