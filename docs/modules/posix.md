# posix（os/components/personalities/posix/）

> 现状描述，当前只有骨架。分层见 `docs/interfaces/filesystem.md`；
> 用户态实施顺序见 `docs/development/userspace.md`。没有 POSIX 或 Linux 兼容承诺。

## 当前状态与边界

独立 Rust `.kcomp`，已接入构建 / fmt / clippy。create / destroy 返回 `-ENOTSUP`，
没有进程、用户 task、syscall 路由或 service endpoint。POSIX 是 VFS 等组件的消费者 /
语义聚合者，不默认发布“所有系统功能”的 service。

`abi/posix.toml` 只定义 create config：exact fingerprint + 恰好 8 字节的 LE
VFS endpoint identity。配置由组合者选择；未来 create 经 `Endpoint<Vfs>` 校验，
再 bind，不自行扫描或加载“第一个 VFS”。SDK 的 VfsBinding 也尚未实现。

Core 的 SandboxedNative 是执行域机制，代码在 `os/core/src/component/sandbox.rs`，
不是 sandbox.kcomp。POSIX 拥有进程、fd、ELF 与 syscall 的语义；Core 拥有 task /
AS / 实际 trap 来源 / 生命周期。多个 POSIX 进程如何使用独立 AS、如何验证 syscall
调用者仍须定稿，不能把每个 PID 直接当成 ComponentId。

## 源码落点

| 文件 | 声明 / 待手写入口 |
|---|---|
| `src/lib.rs` | per-instance VFS binding、process / thread / fd 槽位形状与内部错误 |
| `src/process.rs` | PID / TID、进程 root / cwd、Core task 关联；UserExecution start / exit 占位 |
| `src/fd.rs` | 进程局部 fd 与 VFS file token 分离；install / duplicate / close 占位 |
| `src/usermem.rs` | 不可直接解引用的 UserAddress；copy_in / copy_out 占位 |
| `src/exec.rs` | 静态 ELF 装载计划、段权限、argv/envp；inspect / prepare 占位 |
| `src/syscall.rs` | 真实 Core task 上下文与请求解码接缝；未定义跨组件 trap ABI |
| `src/runtime.rs` | 校验配置、构造失败回滚与退役的待实现入口 |
| `abi/posix.toml` / SDK `posix.rs` | 仅组件 create 配置；没有 PosixApi / Contract |

`FdEntry.close_on_exec` 属于单个 fd；游标、访问和 share 属于 VFS 打开实例。
duplicate 应先 VFS retain 再安装 fd，失败回滚；close 解除 fd 后消费一次 VFS 用户引用。
Console 只是未绑定的类型占位，不能以 Core 日志接口当作用户 stdout。

用户 ELF 与 Core 的 ET_REL 组件 loader 是不同入口。初期目标为 RV64 MMU / 静态
ET_EXEC / 单进程；尚不处理动态链接、static PIE、fork、signal、完整 shell。
第三方 BusyBox / libc 也尚未引入，后续按项目规则使用 `third_party/` submodule。

## 检查入口

```sh
cargo fmt --manifest-path os/components/personalities/posix/Cargo.toml -- --check
cargo clippy --manifest-path os/components/personalities/posix/Cargo.toml --target riscv64gc-unknown-none-elf -- -D warnings
```

跨目标构建与 packer 仅证明镜像形状，不证明进程、隔离或软件兼容性。
