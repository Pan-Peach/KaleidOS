#!/usr/bin/env bash
# kcomp-link.sh —— 语言无关的 .kcomp packer（.kcomp = 链接后的 ET_REL 组件程序）。
#
#   <input .o/.a...> → partial link（--gc-sections + -u 钉住入口）
#                    → strip
#                    → 契约校验（ET_REL / 入口 DEFINED / UNDEF 白名单 / 重定位白名单）
#                    → <output>.kcomp
#
# 本脚本只做 ELF 操作，**不关心输入来自 rustc staticlib 还是 clang .o**：
# 「怎么编出输入」是语言前端的职责（见 tools/build-kcomp.sh 与 build-kcomp-c.sh），
# 「输入怎么变成合法 .kcomp」是这里的职责。两者分离后 .kcomp 就不再是
# "Rust component format"，而是语言无关的组件二进制格式。
#
# 用法：
#   tools/kcomp-link.sh <output> <input.o|input.a>...
#
# 环境变量：
#   LLD       指定 linker（默认从 rustc sysroot 取 rust-lld）
#   OBJCOPY   strip 工具（默认 llvm-objcopy）
#   READELF   符号/重定位读取（默认 llvm-readelf）
#
# 入口契约（loader 侧见 os/core/src/component/loader.rs）：
#   kcomp_init  必须 DEFINED（STT_FUNC）
#   kcomp_exit  可选；定义在**任一**输入里就一并钉住（否则会被 GC 丢掉）

set -euo pipefail

if [ "$#" -lt 2 ]; then
    echo "usage: $0 <output> <input.o|input.a>..." >&2
    exit 2
fi

output=$1
shift

objcopy=${OBJCOPY:-llvm-objcopy}
readelf=${READELF:-llvm-readelf}

# linker：优先显式 LLD，否则沿用 rustc sysroot 里的 rust-lld（与 Rust 前端一致）。
lld=${LLD:-}
if [ -z "$lld" ]; then
    rustc_bin=${RUSTC:-rustc}
    sysroot=$("$rustc_bin" --print sysroot)
    host=$("$rustc_bin" -vV | sed -n 's/^host: //p')
    lld="$sysroot/lib/rustlib/$host/bin/rust-lld"
fi
if [ ! -x "$lld" ]; then
    echo "kcomp-link: linker not found: $lld" >&2
    echo "kcomp-link: 设置 LLD，或确认 rustc sysroot 里有 rust-lld。" >&2
    exit 1
fi

# -u 钉住加载入口：--gc-sections 会丢掉「未被引用」的入口段。kcomp_exit 在
# staticlib 里是独立 archive member、在 C 里是独立函数段，不显式 -u 就会被丢；
# 只有确实定义时才钉住——未定义它的组件保持 UNDEF 白名单干净（loader 按可选解析）。
force=(-u kcomp_init)
for input in "$@"; do
    if "$readelf" -s "$input" 2>/dev/null \
        | awk '$4=="FUNC" && $8=="kcomp_exit" && $7!="UND" {found=1} END{exit !found}'; then
        force+=(-u kcomp_exit)
    fi
done

# partial link：只抽可达成员、GC 未引用段。--no-relax 避免 R_RISCV_ALIGN
# （loader 不支持；relax 在本步骤没有收益）。
tmp=$(mktemp "${TMPDIR:-/tmp}/kcomp.XXXXXX.o")
trap 'rm -f "$tmp"' EXIT
"$lld" -flavor gnu -r --gc-sections --no-relax "${force[@]}" -o "$tmp" "$@"

# strip：去调试段 + Rust LTO bitcode。对 C 输入而言 .llvmbc/.llvmcmd 不存在，
# objcopy 会静默跳过（不是错误），所以这一步对两种语言都安全。
mkdir -p "$(dirname "$output")"
"$objcopy" --strip-debug --remove-section=.llvmbc --remove-section=.llvmcmd "$tmp" "$output"

# ---- 契约校验：load 前失败，别把坏镜像带进 kpkg ----

if ! "$readelf" -h "$output" | grep -q 'REL (Relocatable file)'; then
    echo "kcomp-link: $output is not ET_REL" >&2
    exit 1
fi

if ! "$readelf" -s "$output" | awk '$4=="FUNC" && $8=="kcomp_init" && $7!="UND" {found=1} END{exit !found}'; then
    echo "kcomp-link: kcomp_init (STT_FUNC) not defined in $output" >&2
    exit 1
fi

# UNDEF 只允许 kcore_*（与 Core component/export.rs 白名单精确对齐；注意这是
# **前缀**粗筛，真正精确到名字的校验在 loader 侧）。
undef=$("$readelf" -s "$output" | awk '$7=="UND" && $8!="" {print $8}' | sort -u)
bad=$(printf '%s\n' "$undef" | grep -v '^kcore_' || true)
if [ -n "$bad" ]; then
    echo "kcomp-link: non-kcore undefined symbols in $output:" >&2
    printf '  %s\n' $bad >&2
    echo "kcomp-link: freestanding 组件必须自带实现（memcpy/memset/__udivdi3 之类）" >&2
    echo "kcomp-link: 或在源码里避免产生这些引用；不要链接 C runtime / libc。" >&2
    exit 1
fi

# 只允许 loader 已支持的 RISC-V 重定位类型；特别禁止 R_RISCV_ALIGN。
#   Allow: 0 NONE, 1/2 32/64, 18/19 CALL/CALL_PLT, 20 32_PCREL,
#          23/24/25 PCREL_HI20/LO12_I/LO12_S, 26/27/28 HI20/LO12_I/LO12_S, 51 RELAX.
if "$readelf" -r "$output" | grep -q 'R_RISCV_ALIGN'; then
    echo "kcomp-link: R_RISCV_ALIGN present (loader 不支持) in $output" >&2
    exit 1
fi

unknown=$(
    "$readelf" -r "$output" | awk '/R_RISCV/{print $3}' | sort -u \
        | grep -vE '^R_RISCV_(NONE|32|64|CALL|CALL_PLT|32_PCREL|PCREL_HI20|PCREL_LO12_I|PCREL_LO12_S|HI20|LO12_I|LO12_S|RELAX)$' || true
)
if [ -n "$unknown" ]; then
    echo "kcomp-link: unsupported relocation type(s) in $output:" >&2
    printf '  %s\n' $unknown >&2
    exit 1
fi

echo "kcomp: $output (${undef:-no undefined symbols})"
