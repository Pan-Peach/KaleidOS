#!/usr/bin/env bash
# build-kcomp-c.sh —— C 前端：把 freestanding C 组件构建成 .kcomp。
#
#   <component-dir>/*.c → clang -c（freestanding，不链 libc）
#                       → tools/kcomp-link.sh（partial link + strip + 契约校验）
#                       → .kcomp
#
# **C support ≠ libc support**：不链接任何 C runtime / libc。组件只能 import
# kcore_* 白名单；memcpy/memset/__udivdi3 之类要么自带实现，要么在源码里避免。
# 这正是 FatFs 适合的环境：它是给裸机 MCU 用的 ANSI C（8051/AVR/PIC/Z80...）。
#
# 用法（与 Rust 前端同形，便于 Makefile 统一分发）：
#   tools/build-kcomp-c.sh <component-dir> <target> <output> [<obj-dir>]
#
#   component-dir  组件目录；默认编译其中所有 *.c
#   target         rust target triple（riscv64gc-... / riscv32imac-...）
#   output         输出的 .kcomp 路径
#   obj-dir        中间 .o 目录（默认 <component-dir>/build，建议放仓库 build/ 下）
#
# 源文件清单（可选）：若 <component-dir>/kcomp-c-src.txt 存在，则改为编译其中
# 列出的每个路径（每行一个，# 开头为注释；相对路径按**仓库根**解析）——
# 用于把 third_party 里的库源文件（如 FatFs 的 ff.c）一起编进组件。
#
# 环境变量：CC（默认 clang）、CFLAGS（追加到固定旗标之后）
#
# 实测注意事项：
#   * 用 clang。gcc(riscv64-unknown-elf 9.3) 的产物带 R_RISCV_BRANCH /
#     R_RISCV_RVC_BRANCH / R_RISCV_RVC_JUMP，loader 不支持 → packer 会拒绝。
#   * clang 10 可直接编 riscv32imac / rv64gc，无需额外安装工具链。

set -euo pipefail

if [ "$#" -lt 3 ] || [ "$#" -gt 4 ]; then
    echo "usage: $0 <component-dir> <target> <output> [<obj-dir>]" >&2
    exit 2
fi

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd "$script_dir/.." && pwd)

component_dir=$1
target=$2
output=$3
obj_dir=${4:-"$component_dir/build"}

cc=${CC:-clang}

# rust target triple → clang target / -march / -mabi，与 Rust 组件的 target spec 对齐，
# 避免同一镜像里两种 ABI（kcore_* 只走整数/指针，但 ABI 必须一致）。
case "$target" in
    riscv64gc-unknown-none-elf)   clang_target=riscv64-unknown-elf; march=rv64gc;   mabi=lp64d  ;;
    riscv32imac-unknown-none-elf) clang_target=riscv32-unknown-elf; march=rv32imac; mabi=ilp32  ;;
    *)
        echo "build-kcomp-c: unsupported target: $target" >&2
        exit 1
        ;;
esac

# freestanding 旗标（逐条都对应一条 .kcomp 契约，别随手删）：
#   -ffreestanding -fno-builtin          不链 libc、不合成 builtin 调用
#   -fno-stack-protector                 不引 __stack_chk_fail
#   -ffunction-sections -fdata-sections  --gc-sections 才能按函数/数据丢弃
#   -fno-asynchronous-unwind-tables      \
#   -fno-unwind-tables                   不生成 .eh_frame（省体积、少重定位面）
#   -fno-pic                             不引 GOT 重定位
#   -mno-relax                           不生成 R_RISCV_ALIGN（loader 不支持）
#   -mcmodel=medany                      auipc/PCREL 相对寻址。**不要 medlow**：
#                                        RV64 的 lui 会把 32 位结果符号扩展，组件
#                                        加载地址若 bit31=1（如 0x81a00000）就变成
#                                        0xffffffff8...，一取地址即 load page fault。
#                                        Rust 组件走的就是 PCREL 路径。
cflags=(
    -target "$clang_target" -march="$march" -mabi="$mabi"
    -ffreestanding -fno-builtin -fno-stack-protector
    -ffunction-sections -fdata-sections
    -fno-asynchronous-unwind-tables -fno-unwind-tables
    -fno-pic -mno-relax -mcmodel=medany -O2
    # 组件 ABI 的 C 作者面：C 组件 `#include "kcomp.h"`（kcore_* 白名单 +
    # kcomp_* 生命周期入口 / 组件间契约；Rust 镜像在 kcomp-sdk/src/abi.rs）。
    # 同目录还有 freestanding `<string.h>` shim（ff.c 会 #include 它）。
    -I"$repo_root/os/components/kcomp-sdk/include"
)
if [ -n "${CFLAGS:-}" ]; then
    # shellcheck disable=SC2206  # 故意按空白拆分调用方传入的额外旗标
    cflags+=($CFLAGS)
fi

# 源文件：显式清单优先，否则取组件目录下所有 *.c。
list="$component_dir/kcomp-c-src.txt"
sources=()
if [ -f "$list" ]; then
    while IFS= read -r line || [ -n "$line" ]; do
        line=${line%%#*}
        read -r line <<< "$line"          # 去掉首尾空白
        [ -n "$line" ] || continue
        case "$line" in
            /*) sources+=("$line") ;;
            *)  sources+=("$repo_root/$line") ;;
        esac
    done < "$list"
else
    shopt -s nullglob
    sources=("$component_dir"/*.c)
    shopt -u nullglob
fi

if [ "${#sources[@]}" -eq 0 ]; then
    echo "build-kcomp-c: no C sources (looked for $component_dir/*.c or $list)" >&2
    exit 1
fi

# 随组件私有携带 SDK 的 C 运行时：kcomp.h 只是声明，实现（freestanding weak
# `mem*` / `strlen` / `strchr`）在 kcomp-sdk/c/。每个 C 组件都自带这一份（不建
# shared runtime）；Rust 组件的 compiler_builtins 已提供 mem*，不走这条路径。
# 同目录的 `<string.h>` shim 由 -I 暴露给组件源码。
sdk_c_dir="$repo_root/os/components/kcomp-sdk/c"
shopt -s nullglob
sdk_sources=("$sdk_c_dir"/*.c)
shopt -u nullglob
if [ "${#sdk_sources[@]}" -eq 0 ]; then
    echo "build-kcomp-c: SDK C runtime not found in $sdk_c_dir" >&2
    echo "build-kcomp-c: C 组件需要 freestanding mem*；缺失会导致链接失败" >&2
    exit 1
fi
sources+=("${sdk_sources[@]}")

mkdir -p "$obj_dir"
objects=()
for src in "${sources[@]}"; do
    if [ ! -f "$src" ]; then
        echo "build-kcomp-c: source not found: $src" >&2
        exit 1
    fi
    obj="$obj_dir/$(basename "${src%.c}").o"
    "$cc" "${cflags[@]}" -c "$src" -o "$obj"
    objects+=("$obj")
done

# 交给语言无关 packer：partial link（-u kcomp_instance_create/destroy + kcomp_abi）
# → strip → 契约校验。
exec "$script_dir/kcomp-link.sh" "$output" "${objects[@]}"
