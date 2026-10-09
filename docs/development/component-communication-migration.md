# 组件通信与混合 VFS 增量迁移

> 分阶段迁移计划，非现行契约。用户已授权 Agent 在本任务直接实现。决策见 [IPC ADR](ipc-request-reply-adr.md)、[VFS ADR](hybrid-vfs-adr.md)；基线及不可缩减回归见 [审计](component-communication-audit.md)。每阶段独立验证、可回退，不因本文列出文件就视为完成。

## 1. 依赖与阶段边界

```text
Phase 0 审计/研究/基线
  ├─ Phase 1 host truth → K Server Task Echo → SMP/domain 接线
  └─ Phase 2 Local 对象模型 → 单 Namespace/OpenFile
Phase 1 + Phase 2 → Phase 3 RemoteFs/FatFs + Block 过渡 + ksh/exec
Phase 3 → Phase 4 Block/littlefs/其他服务统一协议
Phase 1..4 功能与隔离门禁 → Phase 5 删除旧业务 Direct/Gate
```

当前已实现 Phase 1 的 KernelNative IPC 与 Phase 2 的 Local 对象模块；私有域 IPC、VFS 服务接线和 Phase 3..5 仍待实现。Phase 2 host/Local 不必等 IPC；Phase 3 真 Remote 必须依赖 Phase 1 真收发。Block 可在 Phase 3 用旧同步调用短期接入，但 Phase 4 必须迁移，不能以“FS 新 IPC 已通”结束整体通信任务。Isolated 的 Server Task 是独立门禁，不能用 K-only 结果删除现有 I Gate 支持。

| 阶段 | 文件级改动建议 | 验收与回退 |
|---|---|---|
| 0：当前审计 | AGENTS.md、reference-systems、两份 ADR、audit、本计划、docs/README；生产 host tests；checksum/CoreTest Echo 对照 | 旧生产逻辑不动，abi/check/QEMU/Arch 保持；记录 trace-on 基线 |
| 1a：Core truth | `os/core/src/component/endpoint.rs` 保留 identity；新增小模块 `os/core/src/component/exchange.rs`，不造 crate；`task/table.rs` waiter/permit；`sched.rs` internal wake；`failure.rs`/`exit.rs` Task/Endpoint 终态回收 | host 状态机覆盖每个拒绝前后无半提交、终态排列、退出与容量；没有 server Task 不能叫 IPC 完成 |
| 1b：入口与 Echo | `abi/core.toml` 窄 send/receive/reply/collect/wait/cancel；`component/exports.rs` 或对应手写导出；KABI 生成；SDK 新薄 `ipc.rs`/C helper；test-only `os/components/tests/kcomp_echo/` + CoreTest 编排，组件清单/manifest/package 同步 | 真实独立 server/caller Tasks，public Core API，正常/多请求/满/错误 owner/退出/取消/SMP wake；旧 checksum Direct/Gate 留作对照 |
| 1c：执行域 | `isolated_load.rs` imports、`containment.rs`、`task`/`sched`、`os/arch` 必要 AS/栈切换、range copy；deployment/scheduling/service-execution/lifecycle 原地更新 | K/I/I/K/I/I，RV64 Sv39 和 RV32 Sv32；非法页/旧 AS/退出/失败；NoMMU 仅可信 K，Sandbox 单列未实现 |
| 2：Local VFS | `vfs/src/provider.rs` 收窄对象语义；`namespace.rs` 单个 Path/Mount；`file.rs` 独立 OpenFile/cursor；`name.rs` bytes/name rule；只读 `local.rs` 小模块；`runtime.rs` 接实例、对外 VFS 协议；SDK vfs.rs | 嵌套、alias identity、挂载位置、多个 Local 实例、close/短读/EOF/missing/notdir/约束/无 Arc 环；不实现完整 tmpfs/cache/delete/ACL |
| 3：Remote/Fat | `vfs/src/remote.rs` connection/node/open proxy；`abi/filesystem.toml` 协调更新；Fat `fatfs_backend.c`/`fatfs_internal.h` node lease/open_node/read_at/owner；`fatfs_service.c` 改唯一协议处理；diskio 保留业务；C SDK 同源 codec | 一个 namespace 同时 Local + 真 FAT，两个独立 Fat/block，root/lookup/open/read/close/old handle；node release与创建 orphan，失效旧 open；真实 FAT 镜像，旧 FS 对照暂留 |
| 3 consumer | `init/src/lib.rs` 显式创建 VFS/连接 mounts；`ksh/src/runtime.rs`/`shell.rs` cat 与 ELF load 改 VFS；POSIX file fd 接口按实际消费者接 | fat/dual-fat shell内容 + RV64 ELF exec 继续通过，不能 cat 迁移而 exec 永久直连 |
| 4：服务收敛 | `virtio_blk/src/lib.rs` server 循环与 Block 协议、SDK block client/dispatch、`abi/block.toml`；littlefs `littlefs.c` 对应 node/open/reader；ram_blk fixtures、driver_prober/probe、posix/network 实际已发布服务逐项清点 | Fat→Block 新 IPC，业务 owner 不被中间调用偷换；littlefs 多实例隔离；driver probe/NoMatch/错误路径；未实现骨架只清接口，不凭空补网络 |
| 5：删除旧通道 | Core endpoint bind/table机制字段、旧普通 `call.rs` Gate 分支；SDK binding/Direct/Gate业务 backend；业务 Api table schema 与 dispatcher；C glue旧分支、old fixture/adapters与文档 | 全消费者已迁移且矩阵相当；协调 fingerprint，无老别名/版本后缀；按下面删除/保留清单逐项记录净变化 |

路径是当前文件或明确标为新增；ABI Core 导出适配需按真实 exports 布局实施，不复制手写第二套导出清单。先按小 patch 评审真相逻辑，再接入口/执行，避免一次修改所有 schema。

## 2. KABI 最小演进

1. 保留当前布局、常量、exports 生成和 `make abi-gen` / `abi-check` 流程；`genmk` 仍为唯一 config→build 映射。
2. 在已有 TOML 增固定 `method` 描述：method ID、request/reply 固定字段与一个 bounded bytes 段、reserved/flag 检查、长度上限；Core 不读取 FS method 表，只 SDK 生成器使用。
3. 先只支持固定整数 LE 和 bytes，禁止 Rust enum layout、指针、allocator/fmt/PanicInfo；C decode/encode 与 Rust typed codec 同源。先实现 Echo 和一个 FS read，证明新方法的编辑点是 schema+业务 handler。
4. 生成 client/server switch 骨架；业务 method handler 手写。C 代码可逐函数阅读，不新增复杂宏/动态注册/通用 trait 层；transport 失败与业务错误仍分开。
5. `kabi_gen.py selftest` 增 synthetic schema：duplicate method、坏字段/length、未知 reserved/flag、截断/oversize、RV32/RV64布局、C↔Rust golden bytes。真实契约固定 counts/fields/fingerprint 与 schema 协调更新，不以生成器测试的旧 counts 阻止合法演进，也不删除其 ABI 漂移防护。
6. 更新 schema → exact fingerprint → 生成物 → C/Rust调用者/provider →契约文档/测试，同时提交。首次阶段内迁移保持同一 contract 全图协调，registry 不支持同 contract 多 fingerprint 并行；测试私有新 contract 可独立验证新 IPC，但不得变成陈旧 ABI 兼容别名。

## 3. 真正可删与必须保留

| 删除条件达到后可删 | 必须保留或由新机制接替 |
|---|---|
| 业务 `FileSystemApi` / `BlockDeviceApi` / VfsApi function tables、api/ctx binding export | stable C ABI、contract exact fingerprint、Endpoint incarnation、owner/live 验证、staged publish 原子性 |
| SDK Direct/Gate enum 分支、table cast、本业务 Gate-specific header/scratch重复适配 | 一个 codec、长度/range/flags校验、read 分块、C库错误转换和thin wrapper |
| 普通 image service dispatcher 与 synchronous Gate 调用链 | create/destroy/image entry、panic containment/escape、真实 AS切换与copy、policy同步建议入口 |
| 仅验证旧机制分支且没有独立语义的重复测试 | owner/stale/failure/参数/exit/SMP/硬件回归；新协议重写验证，不直接删错误场景 |
| Direct publication pin 的实现（确认所有缓存 table 消失后） | K backing resident承诺，DMA静默、private-AS回收条件；禁止因删Direct立刻承诺热卸载 |
| VFS内部纯token桩与无消费者的丰富占位类型（先检查契约） | 外部wire的path/open handle，统一Namespace/OpenFile和Provider lease/connection状态 |

Phase 5 前临时保留旧 Direct/Gate、旧 FS/block clients、checksum race、现有 I Gate 测试。当前 scheduler_rr policy Gate 与 service Gate共用基础代码时不能把整个 call.rs/containment 一删了之；先把必要同步policy入口留窄，再清普通业务分支。NoMMU/MMU/profile构建与ABI glue不属于可删除冗余。

## 4. 规模与停止条件

每阶段记录 `git diff --stat`、业务路径条数、schema编辑点、状态种类/预算与 tests。新exchange增加16槽+receipt/wait/grant状态，是为真实park/owned buffers/匹配/退出安全支付的复杂度；是否让Core更小目前无数据，不能预告净减少。Phase5只以实际净行数和路径检查报告，不以文件改名作收敛证据。

如果 IPC 会引入通用 handle/object/rights-transfer框架、FS对每node需Core对象、Remote重复namespace树，或新增层比旧实现大而无具体消费者证明，停止扩大并重新审查ADR。可回退是保留旧行为与私有测试profile，不维护永久双ABI。

## 5. 尚需落实的设计问题

- 持久 I Task/AS 与failure后跨CPU正在执行的服务何时静默；协作式S-mode不能保证强制结束。
- exchange lock→Task commit、public owner与internal wake、waiter去注册：需要真SMP证据。
- Provider consumer-disconnect/session cleanup与创建类orphan lease；drop deferred release预算不能丢记录。
- 正常stop：现有liveTask EBUSY如何转到quiesce/drain/destroy，不能在Stopping禁止运行后才要求server drain。
- read_at的FatFs lseek能力/overflow与C库单server串行；Block polling不是interrupt I/O。
- 后续symlink/batch/cache/rename、共享buffer/DMA及Sandbox：先有新消费者和硬件证据再扩展。

下一步优先 Phase1a/1b：K-only真实Echo，保留部署缺口；随后Phase2只读Local。任何未实现能力继续由STATUS统一记录，ADR不代替状态页。
