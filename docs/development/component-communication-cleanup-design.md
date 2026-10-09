# KABI 协议方法生成：可手写的小模块方案

> 2026-10-09，设计待实施。基线 `2e10304c389f` 的生成器没有 method 生成能力。
> 本轮授权范围为审计、文档和测试，不修改生产生成器/schema/Core。
> 维护点与真实失败见 [专项审计](component-communication-audit.md)；门禁见
> [迁移清单](component-communication-migration.md)。此页是建议，不是现行 ABI。

## 1. 最终模型与最小 Core

```text
唯一 Contract Schema → 现有 KABI → C / Rust Wire + Typed Client + Dispatch
                                         │
                       Endpoint Request/Reply（现有 envelope 与 Exchange）
                                         │
                                  手写 Provider Handler
```

不新增 channel、connection、capability space、动态路由或业务对象 registry。
Core 不消费 method schema，只验证执行域、Endpoint/owner/grant、消息范围/容量、
request/receipt 终态与真实 Task 等待。Provider 私有对象状态和生命周期仍由 Provider 负责。
组件内部 trait/Arc/普通函数、LocalFs/FIL 都保留；禁止跨 image 裸指针业务表。

## 2. 两个参考方案与采用范围

事实：Fuchsia FIDL 生成 protocol/domain/wire 类型、客户端结果和 server dispatch，
区分 transport 与 application error，复杂类型有完整 wire 规则。借鉴同源 client/dispatch，
不引入 handle transfer、异步 runtime、反射或完整 FIDL 编译器。
见 [官方 C++ bindings](https://fuchsia.dev/fuchsia-src/reference/fidl/bindings/cpp-bindings)
与 [wire specification](https://fuchsia.dev/fuchsia-src/reference/fidl/language/wire-format)。

事实：Wayland 官方文档描述 XML 同源接口、请求和生成 stub；其 wire 是 host byte-order，
还包含动态对象/FD。借鉴简单显式 schema 与薄 stub；KaleidOS 坚持 LE、exact fingerprint，
不照搬其对象 registry、版本兼容与 socket/FD 模型。
见 [官方 Protocol / Code Generation](https://wayland.freedesktop.org/docs/book/Protocol.html)。

这是 2026-10-09 核对的在线文档，不是固定 commit 源码审计，未复制外部代码。
以上采用范围是本项目设计判断，不声称 transport 等价或继承性能/隔离保证。
调度策略不能移到等待自身的 server；seL4 MCS 的 scheduling-context donation 依赖另一套
调度机制，当前不采用，见 [官方 API](https://docs.sel4.systems/projects/sel4/api-doc.html)。

## 3. 第一小 patch：method AST 与 codec

只扩展 `tools/kabi/kabi_gen.py`，普通 dataclass、显式校验/循环，无 crate、宏或框架。
复用 `load_schemas` / `_build_schema` / `_reject_unknown` / `_table_list` / emitters / Output。
Wire 类型与既有 ABI type AST 分开：后者允许指针/usize/fn，wire 一律拒绝这些类型。

建议 schema（下面 **不是当前可被生成器接受的语法**）：

```toml
[[method]]
name = "read"
id = 1
symbol = "KCOMP_BLOCK_METHOD_READ"

[[method.args]]
name = "lba"
type = "u64"

[method.output]
min = 512
max = 512
```

空 args/input/reply 字段默认长度零。`[[method.reply]]` 为固定 LE 字段；输入/输出
尾部字节段各至多一个，显式 min/max；固定前缀长度由字段类型累计计算，不能再手填
同一个 args_len 常量。第一步 Echo 只有 bounded input/output，约束 reply 长度等于
input 长度；Block capacity 固定 u64，Block read 固定 LBA + 512 字节输出，write 镜像。
只支持一个明确的 length equality 引用，不做通用表达式语言。

下一小 patch 才支持命名固定结构：按已有 schema 的字段顺序展开 LE codec；native
repr(C) 布局断言继续存在，wire 不用 memcpy/结构 cast，不引入隐式 C padding。
VfsPath 32、Lookup 80、OpenRequest 48 等当前 wire 保持字节一致，先迁现有形状。

支持 u8/u16/u32/u64/i8/i16/i32/i64。method ID 不重复，名字/symbol 唯一，字段重名、
未知键、递归结构、非法类型、负界、min>max、长度依赖悬空、request/reply 超过现有
1024 envelope 上限均在生成期拒绝。Rust 用 checked_add / get，C 先用减法校验剩余
长度；越界/截断/尾随字节在调用 Handler **之前**拒绝。不能生成索引 panic 的公开 decoder。

常量迁移：已有 METHOD 常量移入 method 定义，不能一边保留 const 一边手工同步 id。
Fingerprint 在 schema 唯一声明，仍显式协调更新，不拿平台 repr(C) 大小当 wire fingerprint。
第一步 codec 生成不改变既有 method 字节；删旧表/契约变更另协调 fingerprint 与全图工件。

生成物仍是提交物。新增 Rust generated/<contract>_wire.rs、C generated/<contract>_wire.h；
注册进现有 Output/check inventory，`make abi-check` 必须检查多余/缺失/漂移文件。
不在普通 Cargo 构建期生成、不写 Core business schema。

第一停点：synthetic schema 正负测试 + C↔Rust literal golden，Echo/真实 Block read
shape 同源；未接 client/dispatch 时明确称 codec proof，不能宣称维护点已降到一处。

## 4. 第二小 patch：client 与 dispatcher

现有 SDK `ipc::service::{Request,invoke,reply}` 与 C `kcomp_ipc_*` 保持唯一 envelope。
生成客户端选择方法号、编码固定字段、校验 bounded input/output、调用 envelope，解码
回复；`Result<method_result, transport_error>` / C transport+method 保持两层错误。
Provider 的负 errno 或 VFS domain-status 不被 generator 猜测或静默映射成成功。

Rust：每 contract 生成普通 Provider trait 与 typed per-method client（同镜像类型）。
生成 `dispatch<P: Provider>` 的 match，解码后调用 trait method，序列化 reply。
C：每 contract 生成 readable static inline codec/client 与一个 switch dispatcher；
通过固定命名的普通 C handler 原型调用，如 `block_handle_read(ctx,lba,buf,len)`。
ctx 只在 Provider 镜像内部借用，不向 Core/Consumer 发布跨组件 function table。
FatFs/littlefs handler 名字前缀由生成函数参数/编译单元适配，禁止复制各自 decoder。

通用 server 循环只做 listen/receive/wait/envelope/reply；dispatch 不持 Core 锁。
verified ComponentId/TaskId 作为本地 Context 值传给 handler，与 request 字段分开。
本轮不新增通用 Session、handle transfer 或 async executor。

重要边界：生成器仅机械映射请求/回复，不自动推断取消补偿。现有 VFS `Undo`、Fat
created FIL 记录由手写 handler/服务外层返回并在 reply 失败后回滚；reaper、shutdown
权限、consume-before-close、backend cursor/错误、DMA 承诺继续手写。要统一这些 hook
必须先有两个相同真实消费者，不能先造框架取代其业务真相。

Block 高层 `read(lba, &mut [u8])` 的长度/checked last-LBA/每 512 字节拆分策略保留
一个薄 facade；C/Rust 同一 schema 定义单条 512 payload，但拆分控制流仍各语言手写。
这与新普通 scalar method 的“schema+handler+test”目标不同，应如实计维护点。
一次多块写失败可能已有前缀副作用，不自动重试，也不承诺消息取消撤销设备写。

第二停点：Echo 与一个真实 Block Provider 使用生成 client/dispatch。新增 test-only
flush 方法只改 synthetic schema+Handler+语义测试；比较生成 diff，证实没有手改多套
codec/backend。不在 virtio 生产接口加入无需求的 flush。

## 5. 真实 FS/VFS 后续切口

FS 先让 RemoteFs 的 mount/root/lookup/node_details/open_node/read_at/close 使用生成
client，再让 Fat IPC 的 `fatfs_dispatch` 形状/编码改生成。旧 API client 与 littlefs
留待下一组迁移；不在长期 SDK 加第三个 FileSystem Backend 来复制 Block 过渡问题。
Node 仍是挂载期 borrowed identity，FIL 是 owned open，owner/Task 与回滚不能在 schema
中用“handle 类型”代替。littlefs 需先补实际 node/open_node/read_at 后才能使用同一 Remote。

VFS 只替 SDK codec、client 参数搬运和 service 的形状 switch；把业务 dispatch 收窄为
typed handler。runtime 的 grant/reaper/控制 shutdown/drain 留在 runtime；生成 validator
同时用于普通方法与 shutdown，避免两个长度定义。Namespace、Path、OpenFile 和 Local
接口不改。C VFS typed client 当前不存在，后续由 schema 生成，不能标为已统一。

## 6. 执行域、生命周期与可验证边界

三 Backend 的依赖耦合已经让 legacy I fixture 携带不允许的 IPC import。
短期修补选独立的 test-only legacy adapter，让该工件继续验证真实 Gate，并对 UNDEF
清单做回归。不同 `.kcomp` 的 import 面必须按实际链接结果检查，不能仅看源分支未执行。
不得为修补此问题把 IPC 字符串加入 I 白名单：当前没有范围 copy 与持久域 Task。

Phase D 的最小顺序：TaskRecord 关联现有 ExecutionDomain/AS；owned private 栈和
Task switch 真正进入/返回正确 AS；再接 IPC imports 的受检 copy-in/copy-out；最后
退出/失败/wait/SMP 静默矩阵。每步使用现有 AddressSpace/Task/Exchange，不另建
DomainTaskRegistry。跨 AS range/copy 可复用旧 Gate 验证思路，不能保留临时借用 VA
到排队请求。K/K、K/I、I/K、I/I 在 RV64/Sv39、RV32/Sv32 分别实测。

普通消费者还包括probe.result与posix.process退出状态observer；它们须纳入generated
协议与真实Task调用，CoreTest锚点轮询不能直接换成等待IPC。
全部普通消费者替代后，才能删除 Direct table/SDK legacy 分支与业务 Gate。
EndpointRegistry/Exchange、containment/AS、Task/owner、同步 PolicyCall 保留；
backing 居留和 DMA 静默条件按原契约，不宣称移除裸表后自动获得热卸载。

## 7. 设计验收与停止点

| 验收 | 当前 | 后续证据 |
|---|---|---|
| 一个方法结构一处定义 | 未满足 | synthetic 增方法只改 schema，输出 client/dispatch/codec 同步变化 |
| C/Rust 字节一致 | 现有 envelope 新测试 3 pass + 1 已知差异 | generated 方法独立 golden、两端互 decode、signed/structure/buffer 边界 |
| Provider 一个业务入口 | virtio Block、默认 Fat、VFS 已有 | RAM/little/probe 与全部旧普通消费者迁移清单为空 |
| 私有域替代 | 无 | 真 Task/AS copy/grant/坏范围/exit/fail/SMP；host 不代替 |
| 旧业务 mechanism 退出 | 无 | 查无普通 table/Backend/Gate 消费者，行为/隔离门禁保持 |

扩大成完整 IDL/runtime、复制 namespace、引入无实际消费者的对象账本或透明重绑时停止
扩大范围。先恢复现有门禁，再逐小 patch实施；生成器本身应减少机械维护，不把资源语义
写进模板。每阶段独立提交和回退，不维持永久双 ABI。
