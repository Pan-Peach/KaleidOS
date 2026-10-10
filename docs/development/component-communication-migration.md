# Component 通信 Cleanup：阶段与文件级清单

> 2026-10-09；审计基线 develop `2e10304c389f`。用户进一步授权直接实现，当前已完成
> 基线回归修复与 Echo/Block/Filesystem/VFS/Posix 方法生成迁移，见 [方法生成](kabi-methods.md)。
> 历史 Phase 0–5 记录不代表本次已经删除旧通道。基线成本/性能见
> [专项审计](component-communication-audit.md)，当前进度权威为 [STATUS](../../STATUS.md)。

## 1. 新阶段与已有工作

| 阶段 | 当前事实 / 本轮结果 | 下一独立停点与回退 |
|---|---|---|
| A 最新源码审计 | 完成：远端/HEAD/clean tree、调用/依赖/维护矩阵、真实门禁、漂移、性能样本 | 提交文档与 test-only 补充；没有生产功能回退 |
| B KABI/SDK | 已实现 scalar/buffer/固定嵌套结构；五个 Contract 的 C/Rust client/codec/dispatch 同源 | 继续迁移旧通道消费者；结构生成不替代 owner/取消/业务语义 |
| C 普通服务统一 | 已有 virtio Block IPC-only、默认 Fat IPC-only、VFS/Local/Remote、ksh/ELF；posix.process 已迁并删旧路径；旧 SDK/RAM/little/probe 仍在 | 先 RAM/Block，再 FS/little/probe，一组消费者一次真实回归 |
| D 执行域/生命周期 | K IPC 真 Task 已有，I 持久 Task/IPC imports/copy 未有，legacy I 回归已修复并通过真实 QEMU | 保持旧 Gate 对照；实现 K/I/I/K/I/I 真域 Task，无 Sandbox 凭空实现 |
| E 删除旧机制 | 未实施、未满足门禁 | 查无普通旧消费者，I 替代通过后协调删表/分支/业务 Gate；policy 留窄 |
| F 文档/测试 | 本轮同步状态、契约描述、ADR与历史标签；新 C/Rust codec 与 SDK test-only 链接修补 | 不把文档完成等同 B–E 完成；旧对照测试到替代完成前保留 |

旧阶段对应：Phase 1 K IPC、Phase 2 Local、Phase 3 Remote/Fat/VFS/ksh 已有；
Phase 4 Block 生产路径已迁、RAM/little/SDK 尚未收敛；旧 Phase 1c 私有域与 Phase 5 删除仍未完成。
不能重复实现现有 VFS 或把 virtio 回退到 Direct。

## 2. 优先恢复基线门禁的小补丁

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

“立即可删”是候选；普通旧业务实现仍有消费者，必须等替代门禁。
已替换：Block/Filesystem IPC 和 VFS client/codec/dispatch 的机械处理；保留薄业务 facade。

| 类别 | 文件 / 内容 | 条件或保留理由 |
|---|---|---|
| 已删除 | vfs/src/stream.rs 无引用且导入已不存在 NodeRef 的旧草图 | 保留 file.rs/FsOpen/wire StreamInfo；没有另建对象模型 |
| 立即可改注释 | virtio_blk 旧 Block Gate token 注释；SDK Block/C Block “只有两路径”；C kcomp_vfs.h 的 Direct/Gate TODO；Fat config ASCII 旧说明 | 都与当前事实不符；不删真实业务，不增状态 |
| 生成后替换 | SDK block/dispatch.rs 的 LBA codec/shape/switch；filesystem dispatch codec；vfs/codec.rs | generated wire 与 client/handler 字节证据通过后删手写搬运；语义测试留 |
| 已生成替换 | SDK vfs.rs 的固定编码/解码、VFS service.rs shape switch、runtime shutdown shape | 保留 VfsError/domain、reaper/Undo、生命周期；共用生成 validator |
| Block 迁移后删 | SDK block.rs 的 BlockDeviceService/Direct extern adapters；block/backend.rs Direct/Gate；C kcomp_block.c 的对应分支 | RAM 和 I/domain fixture 等全部消费者接新路径；typed read 拆分/溢出仍有业务需求 |
| FS 迁移后删 | abi/filesystem.toml FileSystemApi；SDK filesystem.rs Direct service/table；backend.rs legacy branches；C kcomp_filesystem.c/header legacy binding | RemoteFs 使用 generated client；旧 eight-method消费者、littlefs全部迁移 |
| Provider 迁移后删 | Fat fatfs.c 静态 api + flags=0发布；fatfs_service.c Gate image wrapper；littlefs.c API + littlefs_service.c旧decoder | Fat唯一 business handler和IPC owner/rollback保留；little需node/read_at与真实多实例先通过 |
| 已迁移删除 | POSIX execution.rs PROCESS_API/status adapter/Gate dispatcher、ABI PosixProcessApi、SDK posix.rs两Backend | ksh/CoreTest 使用 generated IPC；真实 Task、live shutdown 拒绝、旧EP失效和stop已验证；U-mode进程语义不变 |
| Provider 迁移后删 | ram_blk/ram_blk_rw 静态 SERVICE、Gate dispatch，virtio probe.result Gate dispatcher | IPC server 与真实权限/设备/多实例/NoMatch/写回证明补齐 |
| 私有域替代后删 | kcomp_domain_service legacy adapter；普通业务 Core endpoint_call 路径、Mechanism::Direct/Gate业务分支 | 真 I Task/IPC copy/import/KI矩阵全部通过；旧真实AS/Gate对照不能提前删 |
| 全部裸表消失后删 | Core endpoint api/ctx 和 has_direct_exports、Native Direct stop pin特殊门禁；abi/core.toml相关输出 | 协调component ABI/fingerprint与loader/trace；policy同步prepare/dispatch先分离 |
| 替代后删重复测试 | SDK client Direct/Gate专属机制断言、旧 C FS重复 codec/detail测试 | 行为/错误/owner/stale/panic/exit/SMP/隔离改测新协议，不能直接减少回归覆盖 |
| 必须保留 | endpoint.rs staged/identity/live/exact、exchange.rs终态/grant/wait、registry/task/sched | 唯一资源真相与调度；不新增第二registry |
| 必须保留 | containment.rs、isolated_call/AS切换范围验证、failure/exit hooks、PolicyCall/IRQ/lifecycle窄入口 | 执行/安全机制；不是普通业务RPC胶水 |
| 必须保留 | virtio MMIO/DMA discipline、Fat/little格式库、VFS Namespace/Path/OpenFile/Local/Remote ownership | 业务语义与内存安全；删除Direct不改变backing物理回收限制 |

不存在未使用的旧 VfsApi 可再次删除：基线 schema 已移除。没有发布服务的 network
骨架不纳入“已迁移组件”计数，也不为清理实现未经需求的网络协议。

## 4. 每一组迁移的门禁

1. 先 schema/generator synthetic 测试、make abi-gen/abi-check 与 literal C↔Rust golden。
2. Echo + Block read 真 Request/Reply；不要把 mock 行为当隔离或 worker 证明。
3. RAM/virtio：capacity、非法长度、checked LBA、多块拆分/部分错误、真设备读写、多实例、
   prober Match/NoMatch、claim重复拒绝、DMA纪律；记录吞吐，当前尚无数据。
4. Fat/little/VFS：真实FAT、Local+Remote、多个Fat、little原有format/独立介质、Node身份、
   EOF/短读/独立游标、取消新open/晚reply、Task exit reaper、Provider失败与旧endpoint/handle不重绑、cat/ELF。
5. I域：RV64/Sv39、RV32/Sv32分别验证持久Task+AS、IPC import、copy坏范围、Caller/Server退出、
   Failed/Stopped/SMP wake；NoMMU仅K可信，Sandbox单列未实现。
6. 整套 make check/test-host/test-qemu/test-arch通过且普通旧consumer清单为空后，才删旧业务机制；
   `rg` 零命中只是辅助证据，不能代替语义/硬件门禁。

每组记录准确HEAD、resolved profile、临时日志、生成/手写/测试分别numstat、人工编辑点与
实际consumer数量；先恢复当前失败，不能拿过去PASS给本次打勾。取消不回滚已执行Block
write，endpoint close不替FS回收对象；业务rollback/reaper责任不能因生成而消失。

## 5. 未实施项与停止条件

方法生成与真实 Echo/Block/Fat/VFS 集成已完成。下一步顺真实依赖迁移普通服务，
实现私有Task/IPC，最后删旧机制；每小patch保持独立可验证。
当前没有透明local dispatch、shared memory、零拷贝、通用capability transfer、额外channel/
connection registry或动态RPC路由需求；若模板开始包办对象语义，停止扩大并重新审查。

本轮不以分析报告作为重构验收；当前有真实生产迁移，但最终旧机制退出尚未完成。
