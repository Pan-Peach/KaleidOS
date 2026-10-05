# Cleanup 调研与实施清单

> 2026-10-05 仓库审计与官方项目对照。本文是改进建议，不替代现行架构契约。
> §3 保留清理前审计证据；实施记录见 §6。构建与测试入口以 Makefile 为准。

## 1. 结论

优先整理构建、测试与文档的归属。让 CoreTest 统一组件/系统集成场景，
让不同层次的测试通过同一入口运行、筛选和汇总。Host 继续验证纯逻辑，
ArchTest 继续证明实际 CPU/页表/中断行为，串口测试继续验证真实启动与用户操作。

配置收敛的目标是每种事实有一个来源：系统构建选项在 Kconfig，
本次构建消费一份 resolved `.config`；组件库存与运行图分开；
测试选择与 QEMU 硬件场景由测试编排负责；第三方库内部选项由对应组件负责。
现有的 Kconfig → genmk → Make → Cargo 链应当保留。

## 2. 成熟项目怎么做

以下是上游事实；最后一列是对 KaleidOS 的建议，不能直接当作已有实现。

| 项目/来源 | 官方做法 | KaleidOS 可采用的部分 |
|---|---|---|
| [Linux 测试概览](https://docs.kernel.org/dev-tools/testing-overview.html) | KUnit 验证可独立测试的内部逻辑，kselftest 验证公开接口与完整功能 | 按证明对象分层；CoreTest 对应公开组件接口的集成验证，host 覆盖纯逻辑 |
| [Linux Kconfig](https://docs.kernel.org/kbuild/kconfig.html) / [Kbuild](https://docs.kernel.org/kbuild/kbuild.html) | `.config` 记录解析后的选择，KCONFIG_CONFIG 选择配置文件，O= 指定独立输出目录 | 保留配置链，让配置与完整产物使用同一个构建目录 |
| [Zephyr 配置](https://docs.zephyrproject.org/latest/build/kconfig/setting.html) | board/application fragment 合并为构建目录中的最终 `.config` | fragment 是输入，resolved config 才是消费对象；无需把各层默认值都当成用户入口 |
| [Zephyr Twister](https://docs.zephyrproject.org/latest/develop/twister/index.html) | runner 管理平台、场景、不同 harness 与结果；支持筛选和只构建，明确区分运行状态 | 统一测试编排与报告，保留不同执行后端；先用小的静态场景表实现 |
| [seL4test](https://docs.sel4.systems/projects/sel4test/) | 测试项目与内核分开组合构建，配置影响用例适用性，可按正则选择用例 | CoreTest 保持独立普通组件，显式声明平台条件、依赖与选择范围 |
| [Linux KTAP](https://docs.kernel.org/dev-tools/ktap.html) | 机器可解析的计划、用例结果与诊断；支持 SKIP、TIMEOUT 等结果 | 用例缺失与跳过不能算 PASS；guest 结果与 runner 汇总可采用 KTAP |

[Linux README](https://github.com/torvalds/linux/blob/master/README) 按读者任务导向文档，
[Zephyr README](https://github.com/zephyrproject-rtos/zephyr/blob/main/README.rst) 导向上手与贡献，
[seL4 README](https://github.com/seL4/seL4/blob/master/README.md) 提供项目与文档入口。
据此建议根 README 只回答项目是什么、如何启动、如何验证、接下来读哪里。

[Zephyr AGENTS.md](https://github.com/zephyrproject-rtos/zephyr/blob/main/AGENTS.md)
把自己定位为贡献文档与常见错误的摘要，并要求读相关正式文档、以文档为准。
KaleidOS 可以采用这种职责划分，保留自己“实现由人类手写”的约定；
无需引入上游的 DCO、提交 trailer 或大型团队流程。

## 3. 清理前的仓库审计

| 优先级 | 清理前证据 | 影响与处理方向 |
|---|---|---|
| P0 | [Makefile](../../Makefile) 的 KCOMP_SRCS / KCOMP_C_SRCS 共 26 个组件，15 个来自 tests/；kernel 无条件依赖 init.kpkg | 普通系统与测试库存混装。拆开生产包、测试包、host fixture 清单，测试开关经 Kconfig 解析 |
| P0 | [os/core/build.rs](../../os/core/build.rs) 普通运行时无条件构建 11 个 fixture，选择 RV32 或默认 RV64；host 与裸机构建都会触发 | Core 编译隐含组件工具链与构建成本。把 fixture 准备移出生产 Core 的 build.rs，host loader 测试仍消费真实工件 |
| P0 | Make 的 build/kpkg、build/kpkg-build、tools/qemu/init.kpkg 被多个配置共用；make core 覆盖共享包为空包 | 配置文件独立，产物尚未独立。先隔离构建目录，再开放并行；已有 .NOTPARALLEL 只是部分串行措施 |
| P1 | genmk.ARCH_MAP、[runner.py](../../tests/qemu/runner.py).ARCH_CONF、[arch_runner.py](../../tests/qemu/arch_runner.py).ARCH_CONF 重复 QEMU 可执行名与默认内存 | 默认平台参数有三份副本。runner 接收解析后参数，保留 OOM 内存与设备拓扑等明确的场景差异 |
| P1 | [CoreTest runtime](../../os/components/tests/core_test/src/runtime.rs) 与 runner 的 CORE_TEST_*_CASES 都维护用例范围 | 新增/删掉用例容易漏同步。guest 报告实际计划与结果，runner 校验所选计划是否完整、是否运行成功 |
| P1 | [report.rs](../../os/components/tests/core_test/src/runtime/report.rs) 手工分配 u64 失败位号，最高已到 59，并将高 32 位折回 i32 | 继续加用例会碰到固定宽度上限。使用通过/失败计数与稳定用例名，create 保留 0/非0 返回约定 |
| P1 | arch_runner 有 43 个基础 case + 3 个 SMP case；其中含 lifecycle、restart、stale endpoint、堆后端与服务部署矩阵 | ArchTest 同时承载硬件证据与系统语义。逐例按断言归属拆分，公开 ABI 可表达的场景迁入 CoreTest |
| P1 | make test = test-host + test-qemu + test-arch；[CI](../../.github/workflows/ci.yml) 另跑 test-arch-smp-rv64 | 本地“完整测试”比默认内核 CI 少一项；--smp 当前还会重复全部基础 ArchTest。统一默认矩阵并提供精确筛选 |
| P1 | Make 的 OUR_CRATES、fmt、clippy、check、test-host 分别列 crate；ram_blk_rw 出现在 fmt/clippy，但未进入 check 的对应列表 | 工具覆盖随着组件增长漂移。开发检查复用一份 crate 清单，明确每项适用的 host/target 验证 |
| P1 | [Kconfig 胶水测试](../../tests/kconfig/test_glue.py) 在 check 的内部目标运行；compat runner 测试只在独立 workflow 运行 | make test-host 未涵盖全部宿主工具测试。集中工具测试入口，并区分内核默认回归与宿主兼容性参考 |
| P2 | .cargo/config.toml 与 Make BOOT_RUSTFLAGS 都携带 linker flags；KALEIDOS_CORE_ONLY 同时跳过工件生成与部分 host 测试 | 将工具链参数、系统配置、测试准备捷径写清职责。保留必要的防御/工具 flags，减少隐含改变测试覆盖的入口 |
| P2 | 修改前 README 同时写“.kcomp 未来独立”和“加载链已完成”；AGENTS 约 10.6 KB，含当前不做/进行中等状态；配置文档符号表仍以早期 RISC-V 为主 | 状态副本漂移。README 负责导航，AGENTS 负责禁错，STATUS 负责进度；配置表和帮助文字需再逐项对照源码 |

上述计数来自清理前静态审计，不代表运行结果。`CONFIG_PREEMPT` 可选，但
[sched::on_timer_tick](../../os/core/src/sched.rs) 仍有 todo；其他 ISA 的 Kconfig help
也存在与源码不同步的“全是 todo”描述，应审计后明确支持范围。

## 4. 配置归属

| 事实 | 唯一归属 | 消费方式 |
|---|---|---|
| 架构、特权级、VM、Core 功能/容量、测试镜像选择 | Kconfig + 本次构建的 resolved `.config` | genmk 单向生成，Make/Cargo 消费 |
| board 默认值 | configs/*_defconfig | 输入快照，不复制一份完整配置 |
| monitor/selftest 等用途差异 | 小的 fragment | 合并、校验后消费最终配置 |
| 编译器/链接器路径与内部 flags | 工具链适配脚本、Cargo 配置 | 不另选系统 profile |
| 镜像包含哪些组件 | 构建层的一份组件清单，按已解析配置选择 | 生成 cpio manifest；不当成运行图 |
| 组件启动、服务绑定与运行图 | init 等组件的配置/编排 | Core 验证，不替组件决定策略 |
| 测试 suite、场景、平台适用性、超时 | 测试编排层的一份静态表 | 选择既有 defconfig/fragment，准备硬件拓扑并运行 |
| FatFs ffconf.h / littlefs 宏等库内部细节 | 对应组件 adapter | 只在组件内部消费；不把每个库宏提升成全局 Kconfig |

`.config.mk`、build.rs 生成常量、Cargo feature 转发与 `cfg` 防御是必要的消费层。
判断是否要删应看它是否独立决定同一事实，而非文件名里是否有 config。
不新增一套与 Kconfig 平行的系统 TOML/YAML 配置。

## 5. 测试归属与迁移

| 现有测试 | 建议归属 | 验收条件 |
|---|---|---|
| Core 状态机、owner/stale/rollback、parser、页表编码、property | host，邻近被测源码 | 测生产逻辑；fake 不被当作硬件生效证据 |
| loader 对真实 .kcomp 的解析与重定位 | host integration，显式准备 fixture | fixture 仍由真实链接管线生成；普通 Core build 不触发准备 |
| task/scheduler、block/FS/prober、exec、组件 SMP | CoreTest | 全程公开 ABI、真实组件；错误路径与新实例语义有断言 |
| isolated heap、服务部署矩阵、生命周期/重启/过期访问 | 优先迁入 CoreTest | 只保留可由公开 ABI 观察的断言；私有 AS/页表细节留在 ArchTest |
| satp/AS 切换、RO/NX/unmapped fault、TLB、寄存器、IRQ/timer、AP/IPI | ArchTest | 保留真实硬件触发与故障 cause，破坏性用例独立 guest |
| boot → init → FAT → ksh、输入回显、失败后会话存活、shutdown、OOM | 统一 runner 下的串口 workflow | 仍走真实默认启动与用户输入，独立盘/guest；不把正常 init 改成测试 harness |
| libc-test 的 Linux/Windows 宿主参考 | 独立 reference suite，复用报告原则 | 清楚标记参考平台；不能据此宣称 KaleidOS 兼容性通过 |
| Kconfig、ABI generator、Python runner 自身 | host tool tests | 参数错误、非零退出、超时、缺失/截断报告确实使门禁失败 |

迁移 isolated 用例应先把每条断言分成公开语义和硬件生效两部分。
已有 `kcore_component_load(..., domain)` 可以选择部署域，但某些构造失败条件、
检查页表或故障恢复的白盒用例仍需私有 harness。不能为迁移测试新增调试后门 ABI。
CoreTest 也无法代替“默认启动者确实启动了 init”和串口输入流程的外部观察。

建议保留现有 `make test-host` / `test-qemu` / `test-arch` 名字作为选择入口。
`make test` 汇总默认内核回归，`make check` 承担格式、lint、ABI/配置工具检查、host 与构建。
两者复用同一份默认平台/场景表；CI 选择相同子集，不复制一套矩阵。
可选新 ISA 与宿主参考 suite 明确标为 opt-in，分别报告 build-only、skip 或真实运行结果。

报告可采用 KTAP；通过本次选择的计划数量与逐例结果验证完整性。
缺结果、重复结果、超时、异常退出都应失败；skip 必须有理由，
默认回归要求的用例不能被任意 skip 掩盖。host 的 Cargo 输出由宿主编排汇总，
无需给每个普通 Rust 单测增加一份手工注册数据。

## 6. 实施记录与完成条件

1. **文档入口。** 精简 README/AGENTS，构建步骤移入 building，docs 索引保留权威归属；
   测试指南写清当前实际命令覆盖。本次已完成这部分。
2. **构建隔离。** 为一次构建确定一个输出目录，将 config、包、ELF、fixture、磁盘和日志
   的归属写清；生产包去掉 test-only 库存。先用 Make 小文件/脚本拆职责，不换构建系统。
   验收：交替构建两个 profile 与 Core-only 不覆盖彼此包；普通包没有测试组件。
3. **fixture 与工具检查。** 将测试工件准备移出生产 build.rs，复用组件构建管线；
   fmt/clippy/check 用一份源码清单；宿主工具测试进入统一入口。
   验收：普通 Core build 不生成 fixture；host parser/loader 测试仍覆盖真实工件。
4. **统一运行与报告。** runner 共用 QEMU 进程、串口、日志、超时处理；平台默认参数从
   genmk 的输出传入。加入最小静态 suite/场景表、单例筛选、逐例完整性校验与结果汇总。
   验收：能只跑一个 ArchTest；默认 make test 包含 SMP 硬件门禁；本地与 CI 选择一致。
5. **迁移集成场景。** 按 §5 将公开语义迁入 CoreTest，移除被替代的重复断言，
   保留 fault/寄存器/页表的 ArchTest 证据与真实串口流程。
   验收：每个集成契约能定位到 CoreTest 用例，每个硬件契约能定位到 ArchTest 用例。

用户已授权执行这轮 cleanup。改动限定于构建、现有 boot 的工件路径、测试与文档；
没有扩大 runtime graph、IPC、驱动框架或 capability 实现范围。

| 项目 | 本轮落点 |
|---|---|
| 文档 | README 恢复内核特色与实际链路；AGENTS 保留稳定规则；building/testing 负责命令与测试边界 |
| 构建隔离 | 根 Makefile 收敛为 `mk/*.mk`；`O=` 选择配置及包/镜像/缓存/磁盘/日志归属；Core-only 使用独立子目录 |
| 组件库存 | `mk/components.mk` 一处维护生产、host fixture 与 guest test；Kconfig 的 TEST_COMPONENTS 控制测试包 |
| fixture | Core build.rs 仅转发构建值；`make test-host` 显式准备真实工件并启用 test-fixtures；普通 parser 测试保持默认可跑 |
| runner | `tests/qemu/common.py` 共享进程、串口、临时目录与日志；QEMU 参数来自 genmk；CoreTest 报告 KTAP，不再用失败位图或 runner 名称副本 |
| 默认门禁 | test-host 包含宿主工具；test-arch 包含 RV64 三项 SMP；SMP 不重复基础测试；ArchTest 支持单例与列表，CI 复用入口 |
| 集成归属 | RV64 `core_test/runtime/deployment.rs` 通过公开 ABI 覆盖堆部署、服务矩阵、嵌套调用、重入、失败/新实例/过期绑定 |

完整的 isolated 白盒 suite 继续保留。公开带配置 create 当前固定 KernelNative，
Isolated import 不包含组件创建/枚举接口，也没有公开 stop；CoreTest 借普通 fixture 的
block 服务入口触发 I-mode 客户端操作，不新增 Core 调试 API。
需要指定私有域 config、观察地址空间/寄存器或销毁现场的用例仍归 ArchTest。

### 本轮验收（2026-10-05）

| 检查 | 结果 |
|---|---|
| `make check` | PASS：格式、clippy、ABI/Kconfig、host 与 RV64 build / RV32 check；750 项 Rust host 测试通过，9 项原有 ignored 保留 |
| `make test` | PASS：host + 全部默认 QEMU / ArchTest 子入口，实际包含 SMP |
| CoreTest | RV64 两种 topology 各 70 项；RV32 两种 topology 各 49 项，全部 PASS |
| init 串口流程 | RV64 FAT / OOM / 无盘 / 坏盘、RV32 FAT / 无盘 / 坏盘，共 7 个场景 PASS |
| ArchTest | RV64 43/43、RV32 43/43、RV64 SMP 3/3 PASS；故障 cause 与异常退出均由 runner 判定 |
| 最新 host tool tests | Kconfig 12/12、ABI selftest/check、compat runner 11/11、输出归属 4/4、QEMU harness 7/7 PASS |
| 普通 Core 逻辑 | 清除 fixture 环境变量后 `cargo test -p kernel --lib`：502 PASS、5 ignored；未触发工件准备 |
| 产物归属 | cpio 独立读取生产包：11 个生产组件，无 test-only；Core-only 空包未改写生产包 hash；生产 Core build 目录没有 `.kcomp` |
| 文档与工作区 | 本轮入口 Markdown 链接与 `git diff --check` PASS |

本机 PATH 中 Nix 的 cc 会生成要求更新 glibc 的宿主 proc-macro 动态库，
与系统 glibc 2.31 不匹配。验证时仅指定系统宿主 linker：

```sh
CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=/usr/bin/gcc make check
CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=/usr/bin/gcc make test
```

这是当前开发环境的工具链选择，未写入系统 Kconfig 或固定到项目构建脚本。
CI 使用 Ubuntu 的 Clang/LLVM/GCC。其他 ISA opt-in 与真机不包含在本轮默认矩阵。
