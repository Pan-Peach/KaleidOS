# 配置层（kconfig.md）

KaleidOS 用 Linux Kconfig 风格组织构建配置。**`.config` 是唯一配置真相**：目标架构、特权级、VM 模型、功能开关都只从它出发。profile 只是它的一个具名快照（`configs/*_defconfig`），构建参数（target triple / linker / QEMU / 内存）由它单向推导，不在别处重复声明。

## 1. 数据流

```text
Kconfig ──(menuconfig / defconfig / olddefconfig)──▶ .config          ← 唯一配置真相
   │                                                   │
   │  scripts/kconfig/configure.py                     │
   │  （创建 / 归一化 + 校验，不可满足即报错）              │
   ▼                                                   ▼
configs/*_defconfig ─────────────────────────▶ scripts/kconfig/genmk.py
                                                       │
                                                       ▼
                                       $(KCONFIG_CONFIG).mk（build/… 下）
                                                       │
                                                       ▼
                       Makefile: override KCFG_*（只消费，不推导）
                                                       │
                                                       ▼
        cargo --no-default-features --features $(KCFG_BOOT_FEATURES) --target $(KCFG_TARGET)
```

- **创建 / 归一化**（`scripts/kconfig/configure.py`）：从 `configs/*_defconfig`、fragment、`--set` 解析出一份完整、归一化的 `.config`。显式请求的值如果在 Kconfig 解析后活不下来（`depends on` 不满足、symbol 不可见、名字拼错），脚本直接报错退出，**不写一份会撒谎的 config**。
- **消费**（`scripts/kconfig/genmk.py`）：把 `.config` 翻译成 Make 片段 `$(KCONFIG_CONFIG).mk`。这是**唯一**存放「config → build 映射」的地方。

`Makefile` 只 `include` 生成的片段，消费里面的 `KCFG_*`，从不自己推导。

Kconfig 前端复用 `third_party/Kconfiglib`（git submodule，pin 到具体 commit），**不需要 `pip install`**：`make menuconfig` / `olddefconfig` / `savedefconfig` 直接调用 submodule 里的脚本，`scripts/kconfig/*.py` 也从它 `import kconfiglib`（本地已安装 kconfiglib 时作为兜底）。克隆后按惯例先 `git submodule update --init --recursive`。

### KCFG_* 变量

| 变量 | 含义 |
|---|---|
| `KCFG_ARCH` | 架构短名（`rv32` / `rv64`），用于产物名与 runner |
| `KCFG_TARGET` | Rust target triple（`riscv32imac-unknown-none-elf` / `riscv64gc-unknown-none-elf`） |
| `KCFG_LINKER` | 链接脚本（`linker32.ld` / `linker.ld`） |
| `KCFG_QEMU` | QEMU 可执行文件（`qemu-system-riscv32` / `qemu-system-riscv64`） |
| `KCFG_QEMU_MEM` | QEMU 内存（`1G` / `4G`） |
| `KCFG_BOOT_FEATURES` | 传给 boot crate 的 Cargo features（如 `supervisor,vm-mmu`） |
| `KCFG_SELFTEST` | `y` / `n`，决定产物名与 selftest 入口 |
| `CONFIG_<symbol>` | 每个 bool symbol 都按原值镜像一份 |

所有变量都以 `override` 写出：命令行上误加的 `make KCFG_...=...` 无法制造第二个真相，解析后的 config 永远获胜。构建命令形如：

```sh
cargo build --no-default-features --features $(KCFG_BOOT_FEATURES) --target $(KCFG_TARGET)
```

`--no-default-features` 是硬性约定：managed kernel build 永远显式声明 profile（见 §6）。

## 2. 命令

| 命令 | 作用 |
|---|---|
| `make help` | 列出配置与构建入口 |
| `make menuconfig` | `third_party/Kconfiglib` 的 curses 前端交互编辑；`KCONFIG_CONFIG` 环境变量选择读写哪个文件 |
| `make defconfig` | 写入默认 profile（`qemu_rv64`） |
| `make qemu_rv64_defconfig` / `qemu_rv32_defconfig` / `qemu_rv32_nommu_defconfig` | pattern rule `%_defconfig` → `configs/<board>_defconfig` |
| `make olddefconfig` | 用 Kconfig 新默认值刷新 `.config` |
| `make syncconfig` | 手工改过 `.config` 后重新生成 `$(KCONFIG_CONFIG).mk` |
| `make savedefconfig` | 导出一份最小 defconfig；文件名由 `OUT_DEFCONFIG=` 指定，默认 `defconfig.out` |

两条工作流：

```sh
make qemu_rv64_defconfig && make qemu   # 主工作流：先选 profile，再构建运行
make menuconfig && make qemu            # 第二条：交互调参后直接构建运行
```

`menuconfig` / `olddefconfig` / `savedefconfig` / `syncconfig` / `defconfig` / `%_defconfig` 是 **config-only goals**：不能和 build goals 写进同一条 `make` 命令（Makefile 会直接报错），它们本身也不解析已有 config。`clean` / `distclean` / `help` / `fmt` / `test-host` / `bench` 是 config-free goals，`make clean` 在没有 `.config` 的全新 checkout 上也能跑。

## 3. 符号（phase 1）

| Symbol | 类型 | 默认 | 说明 |
|---|---|---|---|
| `ARCH_RISCV32` / `ARCH_RISCV64` | choice | RV64 | 目标架构 |
| `PRIVILEGE_SUPERVISOR` / `PRIVILEGE_MACHINE` | choice | supervisor | 特权级 |
| `VM_MMU` / `VM_NOMMU` | choice | MMU | 虚拟内存模型；`VM_NOMMU depends on ARCH_RISCV32` |
| `PREEMPT` | bool | n | 抢占式调度；当前基线是协作式 RR |
| `SELFTEST` | bool | n | 构建 ArchTest 镜像；`depends on VM_MMU`，位于 "Debugging / Testing" |

`os/components/Kconfig` 目前是空的扩展点：组件选择留到 loader + manifest 里程碑，Phase 1 不迁移（组件列表仍在 Makefile 的 `KCOMP_SRCS`）。

## 4. 设计决策

### 4.1 没有 `VM_SV32` / `VM_SV39`

Sv32 / Sv39 由 `arch` 从 target triple 推导（RV32→Sv32、RV64→Sv39），没有任何东西消费这两个符号。定义它们只会让同一个事实多出第二个真相来源。

### 4.2 `PRIVILEGE_MACHINE` 只编译验证

M-mode 能编译，但**没有 boot-verified 的启动路径**（没有 M-mode boot harness / OpenSBI hand-off），也**没有任何 defconfig 选择它**。Kconfig 的 `comment` 明确写了这一点。除非你在做 M-mode bring-up，否则用 Supervisor。

### 4.3 约束映射

Kconfig 本身让不可能的组合不可表达：

- **RV64 + NoMMU 不可表达**：`VM_NOMMU depends on ARCH_RISCV32`。若绕过它，boot 会硬报错（`RV64 boot layout requires vm-mmu; NoMMU boot is RV32-only`）。
- **`SELFTEST` 要求 MMU**：selftest 直接操作页表，NoMMU 下无法构建。

### 4.4 只有 `make` 拥有 config → build 映射

映射只存在于 `genmk.py` 的一张注释表里（`ARCH_MAP` + 特权 / VM feature 表 + `KCFG_BOOT_FEATURES` 组装）。Makefile、Cargo、`.cargo/config.toml` 都不各自决定 profile。判断标准：**一个事实只有一处真相**。

### 4.5 fragment 路径是按 config 派生的

生成的 `$(KCONFIG_CONFIG).mk` 紧挨它自己的 `.config`。用 `KCONFIG_CONFIG` 切换 profile 时，每个 profile 都有自己的片段，**不可能复用陈旧变量**。

`.config` / `.config.mk` / `.config.old`（以及整个 `build/`）都在 `.gitignore` 里；只有 `configs/*_defconfig` 提交进仓库。`make distclean` 连配置一起清掉，`make clean` 不动 `.config`（它是用户数据）。

## 5. 已弃用的兼容入口 `ARCH=` / `VM=`

命令行的 `ARCH=rv32` / `VM=nommu` 仍然能用，但会打印弃用警告。它们**不携带 build truth**：会被翻译成 `build/configs/legacy/` 下一份私有 resolved config，再由 `configure.py` 校验。

- 只在**命令行**给出时生效；环境变量 `ARCH` / `VM` 被忽略。
- 不可满足的请求（如 `make ARCH=rv64 VM=nommu qemu`）会**显式失败**，而不是悄悄构建成别的东西。

新代码一律用 `make <board>_defconfig`。

## 6. 不要在这里配置

以下层**不得**决定系统 profile，否则同一事实会出现第二个真相：

- 组件自己的 `Cargo.toml`；
- `os/boot/riscv/.cargo/config.toml`（它的 `[build] target` 已被有意移除，target 永远由 Makefile 显式传入）；
- Rust `#[cfg]` / `compile_error!`。它们是**不变式与防御**（例如「build 没选 profile 就报错」），不是配置来源；
- Cargo `default = [...]` features。`arch` / `core` 上的 default 只作为**独立 host-test 基线**保留；`bootstrap` 没有 default，且 managed kernel build 永远传 `--no-default-features`。

Phase 1 **不迁移**：组件 / 驱动选择、调度器、PMP/MPU、平台发现、其余调试开关。

## 7. 如何新增一个 config symbol

1. 加到正确的 `Kconfig`：架构 / 特权级 / VM 级别的东西放 `os/arch/Kconfig`，Core 构建开关放 `os/core/Kconfig`，组件选择（将来）放 `os/components/Kconfig`。优先用 `choice` / `depends on` / `default ... if` 表达约束，而不是事后用 Rust `compile_error!` 兜底；避免 `select`。
2. 如果这个 symbol 要改变构建，把它加进 `scripts/kconfig/genmk.py` 的映射表（**唯一**存放 config → build 的地方），例如往 `KCFG_BOOT_FEATURES` 追加一个 feature。
3. 如果它改变**构建契约**（target triple、linker、QEMU 二进制、内存），扩展 `genmk.py` 里的 `ARCH_MAP`。
4. 如果某个 profile 默认要打开它，加到对应的 `configs/*_defconfig`。
5. 如果某个 Rust crate 编译期需要它，让它成为一个 Cargo feature，由 Makefile 通过生成的 `KCFG_*` 传入；**不要让 crate 自己决定**。
6. 命名禁止版本后缀（`V1`、`_v2`）：契约变了就原地替换，不做兼容别名。

## 8. 验证（验收标准）

- `make <board>_defconfig && make qemu` 对三个 profile 都能跑通：`qemu_rv64`、`qemu_rv32`、`qemu_rv32_nommu`；
- MMU / NoMMU 与 supervisor / machine 由构造互斥（choice + `depends on`），不可能同时选中；
- `make test-qemu`、`make test-arch`、`make test-driver-prober`、`make check`、`make test-host`、`make test-build` 在两个架构上都通过；
- `make clean` 在没有 `.config` 的全新 checkout 上也能工作。
