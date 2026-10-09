# KABI Request/Reply 方法生成

`abi/block.toml`、`abi/echo.toml`、`abi/filesystem.toml` 的 `[[method]]` 是当前 IPC 方法结构的权威。
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
u8/u16/u32/u64/i8/i16/i32/i64；不接受指针、usize、函数、bool 或未实现的命名结构。
input/output 各一个尾部字节缓冲，必须显式指定 min/max；缺省为空。
output 的 `matches = "input"` 只允许相同显式界限，表示两缓冲长度相同。
回复的固定字段位于 output 缓冲之前。所有数值为 LE。

生成器拒绝重复 ID/name/symbol、重复字段、未知键、非法界限及超 envelope 容量的方法。
容量取自现有 core/component schema。`infallible = true` 只影响 Rust Provider 方法
返回值：旧 Block capacity 返回 u64；普通方法返回 Result。客户端始终区分 transport
与 method 错误。C 的返回值是 transport，`method_status` 是业务状态，transport 失败
不解码业务回复。C handler 是镜像内普通函数，不发布 function table。

Rust Block Provider 接口直接重导出生成 trait；VirtIO Server 使用生成 dispatcher。
Rust/C Block IPC 分支使用生成 client；多扇区拆分、LBA 溢出与 DMA 纪律保持手写。
Echo 的普通请求使用生成 client/handler；原始生命周期/Exchange 控制探针仍属测试。
Server listen/receive/wait、reply 失败补偿、owner、资源对象和状态转换不由生成器推断。

验证入口：`make abi-gen`、`make abi-check`、`make check`、`make test-qemu`、`make test-arch`。
`tests/build/test_kabi_methods.py` 编译实际 generated client/dispatch 和 SDK envelope，
用独立 Python struct.pack golden 比较 C/Rust，覆盖所有八种整数（含 signed 最小值）、
Echo、真实 Block 形状、畸形帧拒绝及错误层次；C 启用 UBSan。
synthetic flush 仅添加 schema 和业务 handler/test，无生产 Block flush，也不改 emitter。
这是 host codec 证据；跨 CPU/地址空间事实由真实系统测试分别证明。

FatFs Server 的 IPC validator/dispatch 与 RemoteFs 的 client 已生成。FatFs 节点详情
业务返回 typed 字段，由生成 dispatcher 编码；FIL owner、shutdown 与 canceled-open
rollback 保持在 Server。旧 C backend 签名仍有本地薄 adapter，legacy table/Gate 未删。

当前停点：Echo/Block/Filesystem IPC 的机械协议胶水已经生成。旧 Block Direct/Gate 仍有真实
私有域和回归消费者，VFS 自身尚未生成；命名固定结构尚不支持。新增普通 IPC 方法的
生成物无需手改，但旧通道退出前，整个 Block SDK 仍未达到最终的一套业务机制验收。
后续实施范围见 [迁移清单](component-communication-migration.md)。
