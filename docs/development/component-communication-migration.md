# Component 通信 Cleanup：阶段与文件级清单

> 2026-10-10；审计基线 develop `2e10304c389f`。用户进一步授权直接实现，当前已完成
> 基线回归修复、六个 Contract 方法生成及普通业务 IPC-only 迁移，见 [方法生成](kabi-methods.md)。
> 历史 Phase 0–5 记录不代表本次已经删除旧通道。基线成本/性能见
> [专项审计](component-communication-audit.md)，当前进度权威为 [STATUS](../../STATUS.md)。

## 1. 新阶段与已有工作

| 阶段 | 当前事实 / 本轮结果 | 下一独立停点与回退 |
|---|---|---|
| A 最新源码审计 | 完成：远端/HEAD/clean tree、调用/依赖/维护矩阵、真实门禁、漂移、性能样本 | 提交文档与 test-only 补充；没有生产功能回退 |
| B KABI/SDK | 已实现 scalar/buffer/固定嵌套结构；六个 Contract 的 C/Rust client/codec/dispatch 同源 | 普通旧消费者已迁；结构生成不替代 owner/取消/业务语义 |
| C 普通服务统一 | 完成：VirtIO/两个 RAM Block、Fat/little、VFS、Posix、Echo、probe.result 统一 IPC；Block/FS C/Rust SDK 无旧 Backend | 真实双架构/NoMMU 多实例、设备、FS、ksh/exec 门禁通过；保留现有业务语义 |
| D 执行域/生命周期 | I RV64/RV32 S/MMU、U RV64 S/MMU 持久 Task/import/copy/IPC 已接通，CPU-only Force/reclaim 与1000轮真实回归通过 | 一般 Graceful drain、OOM/并发与精确回收核算仍缺；保持旧 Gate 硬件对照，见 [Runtime报告 §7](component-runtime-consolidation.md#7-授权后的生产实现与验证) |
| E 删除旧机制 | 普通 Block/FS 的表、SDK Backend、Provider Gate 已删除；Core 同步机制未删 | I 替代通过后审计 Core api/ctx 与同步入口；policy 留窄，隔离/生命周期诊断暂保留 |
| F 文档/测试 | 本轮同步状态、契约描述、ADR与历史标签；新 C/Rust codec 与 SDK test-only 链接修补 | 不把文档完成等同 B–E 完成；旧对照测试到替代完成前保留 |

旧阶段对应：Phase 1 K IPC、Phase 2 Local、Phase 3 Remote/Fat/VFS/ksh 已有；
Phase 4 Block/FS/RAM/little/probe 普通业务与 SDK 已收敛；旧 Phase 1c 私有域已接CPU-only IPC，Phase 5同步诊断删除仍未完成。
不能重复实现现有 VFS 或把 virtio 回退到 Direct。

## 2. 历史基线门禁修复（迁移前快照）

| 文件 | 精确问题 / 最小方案 | 证据与权限边界 |
|---|---|---|
| SDK block/backend.rs:123/154 | chunks_exact 改 as_chunks::<512>，保持非零倍数检查和 last-LBA overflow | 已实施；check PASS，长度/溢出语义保留 |
| Core component/call.rs::prepare | reserved policy 检查先于 ordinary IPC-only NoDispatcher；Policy 不进入普通 IPC listen | 已实施；host 拒绝优先级测试 PASS |
| CoreTest runtime/driver.rs、runtime.rs | attach/read/capacity 放 owned Task，退出后 report；不放开 Core 锚点 IPC | 已实施；RV64/RV32/NoMMU 两项 driver PASS |
| SDK block/backend.rs + tests/kcomp_domain_service | legacy fixture 隔离 IPC import 依赖；短期独立 test adapter，长期真 I IPC | 已实施 test-only legacy adapter；真实 I 回归 PASS，未加 IPC 白名单 |
| SDK ipc/service.rs | Request::decode 加 MESSAGE_MAX 上界，与 C 一致 | 已实施；1025 字节拒绝测试 PASS，移除 expectedFailure |
| SDK src/test_support.rs | 四个 IPC extern 的 test-only ENOTSUP 替身 | 本轮已完成，94 SDK tests pass；不模拟 Exchange |

这些是可审阅的小修补，不以一口气修改 Core/所有协议解决回归。

## 3. 逐文件 Cleanup 清单

普通业务协议已只剩 IPC；以下按当前文件状态区分已删除与仍须保留的机制。
Core 同步测试入口不作为普通 Block/Filesystem 的兼容 Backend。

| 类别 | 文件 / 内容 | 条件或保留理由 |
|---|---|---|
| 已删除 | vfs/src/stream.rs 无引用且导入已不存在 NodeRef 的旧草图 | 保留 file.rs/FsOpen/wire StreamInfo；没有另建对象模型 |
| 立即可改注释 | virtio_blk 旧 Block Gate token 注释；SDK Block/C Block “只有两路径”；C kcomp_vfs.h 的 Direct/Gate TODO；Fat config ASCII 旧说明 | 都与当前事实不符；不删真实业务，不增状态 |
| 已删除/生成替换 | SDK block/dispatch.rs 的 LBA codec/shape/switch；filesystem dispatch codec；vfs/codec.rs | generated wire 与 client/handler 字节证据通过后删手写搬运；语义测试留 |
| 已生成替换 | SDK vfs.rs 的固定编码/解码、VFS service.rs shape switch、runtime shutdown shape | 保留 VfsError/domain、reaper/Undo、生命周期；共用生成 validator |
| 已删除 | SDK block.rs 的 BlockDeviceService/Direct extern adapters；block/backend.rs Direct/Gate；C kcomp_block.c 的对应分支 | RAM/真实业务消费者使用 IPC；I fixture 改用专用 domain.test 同步诊断，typed read 拆分/溢出仍保留 |
| 已删除 | abi/filesystem.toml FileSystemApi；SDK filesystem.rs Direct service/table；backend.rs legacy branches；C kcomp_filesystem.c/header legacy binding | RemoteFs 使用 generated client；旧 eight-method消费者、littlefs全部迁移 |
| 已删除 | Fat fatfs.c 静态 api + flags=0发布；fatfs_service.c Gate image wrapper；littlefs.c API + littlefs_service.c旧decoder | Fat唯一 business handler和IPC owner/rollback保留；little mount/path-open/read/close 迁 IPC，多实例真实通过；node/read_at 仍 ENOTSUP |
| 已迁移删除 | POSIX execution.rs PROCESS_API/status adapter/Gate dispatcher、ABI PosixProcessApi、SDK posix.rs两Backend | ksh/CoreTest 使用 generated IPC；真实 Task、live shutdown 拒绝、旧EP失效和stop已验证；U-mode进程语义不变 |
| 已删除 | ram_blk/ram_blk_rw 静态 SERVICE、Gate dispatch，virtio probe.result Gate dispatcher | IPC server 与真实权限/设备/多实例/NoMatch/写回证明补齐 |
| 私有域替代后删 | 专用 domain.test/checksum 同步诊断；Core endpoint_call、Mechanism::Direct/Gate | 真 I Task/IPC copy/import/KI矩阵全部通过；旧真实AS/Gate对照不能提前删 |
| 全部裸表消失后删 | Core endpoint api/ctx 和 has_direct_exports、Native Direct stop pin特殊门禁；abi/core.toml相关输出 | 协调component ABI/fingerprint与loader/trace；policy同步prepare/dispatch先分离 |
| 已删除重复实现测试 | SDK client Direct/Gate专属机制断言、旧 C FS重复 codec/detail测试 | 行为/错误/owner/stale/panic/exit/SMP/隔离改测新协议，不能直接减少回归覆盖 |
| 必须保留 | endpoint.rs staged/identity/live/exact、exchange.rs终态/grant/wait、registry/task/sched | 唯一资源真相与调度；不新增第二registry |
| 必须保留 | containment.rs、isolated_call/AS切换范围验证、failure/exit hooks、PolicyCall/IRQ/lifecycle窄入口 | 执行/安全机制；不是普通业务RPC胶水 |
| 必须保留 | virtio MMIO/DMA discipline、Fat/little格式库、VFS Namespace/Path/OpenFile/Local/Remote ownership | 业务语义与内存安全；删除Direct不改变backing物理回收限制 |

不存在未使用的旧 VfsApi 可再次删除：基线 schema 已移除。未使用的 NetworkApi/NetworkService 占位已删除；
没有发布服务的 network 骨架仍 ENOTSUP，不纳入“已迁移组件”计数。

## 4. 每一组迁移的门禁

1. 先 schema/generator synthetic 测试、make abi-gen/abi-check 与 literal C↔Rust golden。
2. Echo + Block read 真 Request/Reply；不要把 mock 行为当隔离或 worker 证明。
3. RAM/virtio：capacity、非法长度、checked LBA、多块拆分/部分错误、真设备读写、多实例、
   prober Match/NoMatch、claim重复拒绝、DMA纪律；记录吞吐，当前尚无数据。
4. Fat/little/VFS：真实FAT、Local+Remote、多个Fat、little原有format/独立介质、Node身份、
   EOF/短读/独立游标、取消新open/晚reply、Task exit reaper、Provider失败与旧endpoint/handle不重绑、cat/ELF。
5. I域：RV64/Sv39、RV32/Sv32分别验证持久Task+AS、IPC import、copy坏范围、Caller/Server退出、
   Failed/Stopped/SMP wake；NoMMU仅K可信，Sandbox RV64支持见 Runtime 报告§7，RV32 U未实现。
6. 整套 make check/test-host/test-qemu/test-arch通过且普通旧consumer清单为空后，才删旧业务机制；
   `rg` 零命中只是辅助证据，不能代替语义/硬件门禁。

每组记录准确HEAD、resolved profile、临时日志、生成/手写/测试分别numstat、人工编辑点与
实际consumer数量；先恢复当前失败，不能拿过去PASS给本次打勾。取消不回滚已执行Block
write，endpoint close不替FS回收对象；业务rollback/reaper责任不能因生成而消失。

## 5. 未实施项与停止条件

普通业务方法生成、Block/Filesystem SDK 和全部现有普通 Provider 的 IPC 迁移已完成。
下一步实现私有Task/IPC，再删除失去必要诊断消费者的 Core 旧机制；性能优化后置。
私有域与停止回收的最新源码基线、依赖和验收拆分见
[Runtime第一轮审计](component-runtime-consolidation.md#3-文件级实施任务)，不另造迁移清单。
当前没有透明local dispatch、shared memory、零拷贝、通用capability transfer、额外channel/
connection registry或动态RPC路由需求；若模板开始包办对象语义，停止扩大并重新审查。

本轮不以分析报告作为重构验收；当前有真实生产迁移，但最终旧机制退出尚未完成。
