# posix（os/components/personalities/posix/）

最小 RV64/MMU 用户进程 profile，尚无完整 POSIX / Linux 兼容承诺。
普通用户 ELF 不登记为组件；一个 `posix.kcomp` 实例拥有一个进程族，族内每个进程
对应独立的 Core task / 用户地址空间。Core 验证 owner、映射、现场与实际 trap 来源；
POSIX 解释 ELF、PID、syscall、父子关系、退出码和 wait。

## 当前行为

- ELF64 little-endian / EM_RISCV / 静态 ET_EXEC；拒绝 interpreter、dynamic、W+X、
  重叠页和非法范围。装载 PT_LOAD，清零 BSS，建立 RW-NX 栈与 argc/argv/envp/auxv。
- AT_RANDOM 当前填时钟 nonce；尚无熵源服务，不提供安全随机性承诺。
- 初始进程、fork（RV64 `clone` 的独立进程子集）、execve、wait4（指定 PID / 任意子进程、
  WNOHANG）；exec 先构造新 AS，成功才替换，失败保留旧镜像。fork 深拷贝映射和整数 / 浮点现场。
- fd 0 是 EOF，fd 1/2 通过现有 SDK console 输出真实字节；close 与 fork 的关闭状态独立。
  这里没有调用诊断日志来伪造 stdout，也没有通用文件 fd。
- brk 增长实际映射零页；mprotect 原子替换一段已映射范围的权限，仍限制可读与 W^X。
  brk 缩小只改逻辑边界，当前不回收 backing。零页保护、mmap/munmap、线程与 vfork 未实现。
- getpid/getppid/gettid、固定 uid/gid 0、set_tid_address、sched_yield、exit/exit_group。
  未实现 syscall 返回 ENOSYS，不以空成功代替。
- 用户异常终止对应进程，记录 wait signal 状态：非法指令 SIGILL、断点 SIGTRAP、
  非对齐访问 SIGBUS、页错误 SIGSEGV。未实现 signal handler / delivery。

进程族固定在创建 CPU；只读状态 endpoint 使用原子字段，可从别的 CPU 查询。
POSIX 提议每次用户运行的 10ms deadline，Core 保留已有更早 deadline 并编程 timer。
U-mode ecall / timer / fault 先恢复该 task 的内核栈与 kernel satp，再执行 POSIX 代码、
yield 或 park；不从 per-CPU trap 栈调度，不使用 SUM 解引用用户地址。

## 镜像来源与 ABI

`abi/posix.toml` 定义协调替换后的 create 配置：16 字节 LE header，随后是有名字的
不可变 ELF 快照、argv 和 envp。最多 8 个镜像、128 个参数与环境字符串，总配置 16MiB。
名字是这个 profile 的显式镜像 key；execve 只接受配置中存在的名字，其余返回 ENOENT。
它没有目录、cwd、权限或 VFS namespace 语义。

SDK `posix::encode` 构造配置；`management::create` 创建进程族；`posix.process`
endpoint 的 Direct C table / Gate method 0 只返回初始进程是否退出、wait status 与
live 进程数，回调不创建任务、不调度、不 park。布局与 exact 指纹以 schema 为准。

ksh 的 `exec 0:/APP.ELF [args...]` 经真实 filesystem provider 的 open/read/close
读取镜像，再以 `/main` 为镜像 key 创建这个 profile。它是 shell 的组合策略；
通用 execve 路径查找和应用 open/stat 仍需 VFS。多个 live FS provider 会明确拒绝。
已有 FAT/littlefs/block 可复用，不需要重写文件系统实现。

## 生命周期与缺口

wait 消费一次 zombie 状态；无效 status 指针不会提前消费 zombie。退出记录与已经
发布的内存 backing 保持驻留，成功 exec 的旧 backing 也保留；这不是物理回收机制。
未启动装载失败可以 discard staging task。组件只在 live 数为 0 时逻辑停止，observer
ctx 保持驻留，旧 binding 返回 ESRCH。

`process.rs` / `fd.rs` / `usermem.rs` / `syscall.rs` 中通用 VFS 语义模型仍是骨架；
当前运行期为 `execution.rs`、镜像配置为 `image.rs`、装载器为 `exec.rs`。
SandboxedNative **组件**装载仍返回 ENOTSUP；这里的普通用户 task 不等于已实现
SandboxedNative `.kcomp` import / heap / lifecycle 后端。

当前静态 glibc 启动仍被缺失的 uname、mmap、signal 等 syscall 阻断，libc-test
宿主 PASS 不能作为 KaleidOS PASS。Win32 还需同 ISA 的 PE、imports / API 与 HANDLE。

## 验证

`make test-host` 包括 ELF / 启动栈 / 配置截断测试与 Core 范围、权限编辑、timer 真相测试。
`make test-qemu` 的 RV64 CoreTest 用真实普通 ELF 检查 fork 的 backing / FP 独立性、
exec 成功与回滚、wait 消费与坏指针、U 权限、代码只读、栈 NX、mprotect 和 timer。
默认 init 的 FAT 串口流程另外验证实际文件执行与故障后的 shell / FS 存活。
RV32 保留既有回归，普通用户执行显式 ENOTSUP。操作说明见
[userspace.md](../development/userspace.md)。
