#!/usr/bin/env bash
# build-kcomp.sh —— Rust/Cargo 前端：把一个 Rust 组件构建成 .kcomp。
#
#   Cargo.toml(+src) → staticlib（私有携带 SDK / third-party 依赖）
#                    → tools/kcomp-link.sh（partial link + strip + 契约校验）
#                    → .kcomp
#
# 语言无关的那半段在 tools/kcomp-link.sh（它只管 ELF，不认识 Cargo）；
# 本脚本只负责「Rust 怎么编出输入」。C 前端见 tools/build-kcomp-c.sh——
# 两者 CLI 同形，.kcomp 因此是语言无关的组件二进制格式，而不是 Rust 格式。
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

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

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

# 组件 = staticlib：SDK / third-party 依赖随镜像私有携带（不建 shared runtime）。
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

# 交给语言无关 packer：partial link（-u kcomp_instance_create/destroy + kcomp_abi）
# → strip → 契约校验。
exec "$script_dir/kcomp-link.sh" "$output" "$lib"
