# KABI Request/Reply 方法生成

`abi/block.toml`、`abi/echo.toml`、`abi/filesystem.toml`、`abi/vfs.toml`、`abi/posix.toml` 的 `[[method]]` 是当前 IPC 方法结构的权威。
既有 `tools/kabi/kabi_gen.py` 生成 C/Rust 方法编号、typed client、Provider 接口、
LE 编解码、长度 validator 和 dispatcher。生成物是提交物，普通构建不运行生成器。
Core 不读取业务 schema；使用现有 Endpoint、Exchange 和 SDK envelope。

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

`args` 和 `reply` 字段按声明顺序连续编码，不含 native padding，只接受
u8/u16/u32/u64/i8/i16/i32/i64，以及同 schema 的固定命名结构；递归展开字段，
拒绝递归结构、指针、usize、函数和 bool。native repr(C) 大小/偏移断言与 wire 编码分开，
wire 不复制 native padding。公开结构 decoder/encoder 要求精确长度，错误返回 EINVAL。
input/output 各一个尾部字节缓冲，必须显式指定 min/max；缺省为空。
output 的 `matches = "input"` 只允许相同显式界限，表示两缓冲长度相同。
回复的固定字段位于 output 缓冲之前。所有数值为 LE。

生成器拒绝重复 ID/name/symbol、重复字段、未知键、非法界限及超 envelope 容量的方法。
容量取自现有 core/component schema。`infallible = true` 只影响 Rust Provider 方法
返回值：旧 Block capacity 返回 u64；普通方法返回 Result。客户端始终区分 transport
与 method 错误。C 的返回值是 transport，`method_status` 是业务状态，transport 失败
不解码业务回复。`mutable = true` 使 Rust Provider 接口借用 &mut self；
`reply_on_error = true` 保留负业务状态时的固定回复，例如 VFS domain-status，
客户端返回原始 i32 与 typed reply，语义有效性由 facade 判定。C handler 是镜像内普通函数，不发布 function table。

Rust Block Provider 接口直接重导出生成 trait；VirtIO Server 使用生成 dispatcher。
Rust/C Block IPC 分支使用生成 client；多扇区拆分、LBA 溢出与 DMA 纪律保持手写。
Echo 的普通请求使用生成 client/handler；原始生命周期/Exchange 控制探针仍属测试。
Server listen/receive/wait、reply 失败补偿、owner、资源对象和状态转换不由生成器推断。

验证入口：`make abi-gen`、`make abi-check`、`make check`、`make test-qemu`、`make test-arch`。
`tests/build/test_kabi_methods.py` 编译实际 generated client/dispatch 和 SDK envelope，
用独立 Python struct.pack golden 比较 C/Rust，覆盖所有八种整数（含 signed 最小值）、
Echo、真实 Block/FS/VFS 形状、嵌套结构、畸形帧拒绝及错误层次；C 启用 UBSan，
检测到 UB 立即失败。VFS 错误回复测试保留未知 errno 和 domain-status。
synthetic flush 仅添加 schema 和业务 handler/test，无生产 Block flush，也不改 emitter。
这是 host codec 证据；跨 CPU/地址空间事实由真实系统测试分别证明。

FatFs Server 的 IPC validator/dispatch 与 RemoteFs 的 client 已生成。FatFs 节点详情
业务返回 typed 字段，由生成 dispatcher 编码；FIL owner、shutdown 与 canceled-open
rollback 保持在 Server。旧 C backend 签名仍有本地薄 adapter，legacy table/Gate 未删。

VFS 的固定结构、client、validator 和 dispatch 已生成。Service 仅实现业务 Handler，
保留单一 Namespace/OpenFile、owner、Undo；runtime 使用共享 shutdown validator 和状态 codec。
SDK facade 保留 domain-status 判定、短读和便利引用清理，不再手写 Wire 方法 switch。

当前停点：Echo/Block/Filesystem/VFS/Posix IPC 的机械协议胶水已经生成。旧 Block Direct/Gate
仍有真实私有域和回归消费者。新增普通 IPC 方法的
生成物无需手改，但旧通道退出前，整个 Block SDK 仍未达到最终的一套业务机制验收。
后续实施范围见 [迁移清单](component-communication-migration.md)。

## 本阶段维护成本对照

| 编辑任务 | 基线人工维护点 | 当前 IPC 路径 |
|---|---|---|
| 普通 Block 方法 | schema method 常量、Rust IPC branch/dispatch、C IPC codec，以及 Direct/Gate table/client/dispatch | schema + Handler + 语义测试；两语言 codec/client/dispatch 自动生成，旧通道仍需迁移 |
| Filesystem 方法 | schema 常量、Fat IPC/Gate decoder、RemoteFs 手写 client；旧 Rust/C FS adapters | schema + Handler + 语义测试；Fat IPC/RemoteFs 同源，legacy 仍有真实消费者 |
| VFS 固定方法 | schema 常量、SDK 编码/解码、Service 长度检查/解码/回复、runtime 特殊 shape | schema + Handler + 语义测试；便利 facade 或新业务生命周期仍可能需要手写 |

VFS 这一步的生产手写 SDK/Service/runtime 合计净 -36 行，生成器净 +115 行，
新增 C/Rust VFS 生成物 1289 行；固定 ABI 定义移出重复常量净 -27 行。
生成方法接口显式展开，减少独立维护点而非追求总行数下降。Core 状态、registry、锁增量为零。
这些是相对 `d3e7a55` 的 diff 行数，不包含测试、文档和 schema 定义。

完整 check/host、RV64/RV32 QEMU 与 init/ksh、ArchTest 和 RV32 NoMMU K 回归通过。
raw Direct/Gate/IPC 串行重测及限制见[审计更新](component-communication-audit.md#10-授权实施后的更新)；
该探针没有计入 generated codec，不能据生成器迁移宣称性能提高。
CoreTest 另记录真实 VirtIO 512字节/4KiB读吞吐，包含SDK分块和generated Block调用；
原来没有吞吐探针，不能计算重构前后比例。Isolated IPC 与普通旧通道退出尚未完成。

Posix 这一步完整删除该 Contract 的旧 Direct table、Gate dispatcher 和 SDK 两个 Backend。
status/shutdown 均由 schema 生成 C/Rust client/dispatch；原子退出状态与 live 检查保持手写。
生产手写 SDK/Provider/ksh 相对 VFS 停点净 +24 行，新增 C/Rust 生成物 166 行；
新增一个 observer Task 是实际运行成本，shutdown 后退出；Core 新增状态/锁为零。
真实 QEMU 的 ELF/fork/exec/wait、timer、旧 Endpoint、shutdown/stop 与 OOM 恢复覆盖这条链。
极端 OOM 下既有 Core destroy 临时栈仍可能分配失败；不能由逻辑退役推导物理回收。
