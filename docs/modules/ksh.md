# ksh（os/components/ksh/）

> 现状描述：第一个 KernelNative shell 组件。执行域与生命周期契约见
> `docs/architecture/component-lifecycle.md`、`docs/architecture/deployment.md`。

## 职责与启动

`ksh` 拥有输入行、tokenization、命令分派与人类可读输出。组件 / endpoint / device
的存在与生命周期仍由 Core 裁决；文件语义由 filesystem provider 提供。业务代码只依赖
SDK 值查询与服务前端，没有 Core registry 指针或执行域分支。

普通 QEMU profile 默认由 [`init`](init.md) 组合调度器 / driver / FAT root 并启动 ksh。
选择 `make monitor_defconfig` 后也可在 Core Monitor 手动输入：

```text
core> load scheduler_rr
core> load ksh
KaleidOS ksh
ksh> help
ksh> exit
core>
```

create 只启动一个普通组件任务；会话占用串口输入直到 `exit` 或空行 Ctrl-D。
退出后任务 Exited，组件仍是 Ready，可由 monitor `unload ksh`。没有替换 boot monitor
的调试入口；自动启动策略在 init 组件。当前没有 console session 仲裁，不要并发启动多个 shell。
monitor 在读输入前先服务本 CPU Runnable 任务；单个 shell 在 idle yield 后先恢复，
monitor 不抢读它的下一条命令。未安装调度策略时 monitor 仍可用于完成初始组合。

## 实际命令

| 命令 | 当前行为 |
|---|---|
| `help` / `echo <args...>` / `clear` / `exit` | 静态帮助、whitespace 合并、ANSI 清屏、结束任务 |
| `components` | 所有已登记实例的 ID / artifact 名 / execution domain / state，含终态记录 |
| `endpoints` | ID / provider ID / port / 名称 / contract 数值 / Pending、Live、Invalid |
| `devices` | 纯发现得到的 DeviceId；不认领设备，不为显示名称扩充 ABI |
| `load <artifact> [native\|isolated]` | 默认 native；可省略 `.kcomp`；Core 验证请求，失败打印 errno；不隐式回退域 |
| `inspect <loaded-artifact>` | 复用组件查询，显示该 artifact 的所有已加载实例、KCOMP 格式、域与状态 |
| `cat <provider-relative-path>` | 查找唯一 Live、exact ABI 匹配的 filesystem endpoint，bind / mount / open / read / close |
| `exec <provider-relative-path> [args...]` | 经唯一 FS 读静态 ELF，创建 POSIX 镜像快照进程族，等待退出并显示 exit / signal |
| `ls` / `cd` / `pwd` | 明确报告 unsupported：现有 FS 没有目录枚举或工作目录 / namespace 契约 |

`inspect` 当前不读取未加载 artifact 或任意路径的字节，也不解析 ELF / PE / WASM。
这需要外置的只读文件 / artifact inspection helper；Core 不新增格式识别 API。

`cat` 不替用户选择多个 provider，也不自动组合 driver / filesystem。多个 Live FS 返回
歧义错误，无匹配端口返回缺失错误。路径解释交给 provider；FAT 示例为 `0:/HELLO.TXT`，
littlefs 使用其自身路径。mount 幂等；退出 shell 不 unmount 共享 provider。传输与方法错误
分别保留，打开的文件在正常完成或 read 错误后都 close。

输入最多 128 字节、17 个词（含命令）。无引号 / 展开 / 管道 / 脚本语法；支持退格、CRLF、
Ctrl-C 取消。超长行整行丢弃到换行，下一行恢复，绝不执行截断后的命令。

## Core ABI 的最小补充

新增三个导出（源在 `abi/core.toml`）：

| 导出 | 必要性与边界 |
|---|---|
| `kcore_console_read_byte` | 当前 console 尚无组件服务；复用已有 arch console 输入与有界 idle wait，SDK 屏蔽后端 |
| `kcore_component_nth` | Core 独占组件存在与状态；复制固定值结构与完整名称，不交付 registry 指针 |
| `kcore_endpoint_nth` | Core 独占端口存在与状态；复制固定值结构与完整名称，不交付 provider 函数表 |

查询每次返回一行的快照，名称缓冲不足返回 ENOBUFS，不截断；不承诺整张表原子快照。
既有 `kcore_device_nth` 接受空 compatible 字节串以枚举全部设备；既有
`kcore_component_load` 增加 domain 请求参数，沿用 id / -errno 返回编码。后者改变 import
签名，所以 `KCOMP_ABI` 原地协调替换为 `0x9D73_405B_B2F8_16C0`；旧组件明确不兼容。
SDK / C provider / test fixture 的指纹与既有 load 调用同步更新，生成的声明与导出表由
`make abi-gen` 更新、`make abi-check` 检查。

复用的机制包括 console write、device discovery、组件 load、task start/yield/exit，以及
Endpoint / FileSystemBinding。没有加入 device metadata、artifact parser、通用 exec API。
从组件任务调用 Isolated create / destroy 时，Core 使用与 service 相同的跨 AS 身份纪律：
被调实例的 identity / 恢复边界覆盖 caller，组件入口期间挂起 Core ABI 深度，返回后恢复。

## 代码与验证

| 文件 | 用途 |
|---|---|
| `os/components/ksh/src/{parser,input}.rs` | 有界解析与行编辑，8 个 host 用例 |
| `os/components/ksh/src/shell.rs` | 确定性单行分派、查询显示、FS consumer；静态 ELF 读取与 POSIX profile 组合 |
| `os/components/ksh/src/runtime.rs` | 组件入口与交互任务 |
| `os/components/kcomp-sdk/src/{console,management}.rs` | 安全值前端与 task 包装 |
| `os/core/src/component/export/query.rs` | 三个窄导出的 Core 实现 |
| `abi/{core,component}.toml` 与 Core / SDK generated 文件 | 查询布局、load 请求与协调指纹 |
| `os/core/src/component/{containment,isolated_lifecycle}.rs` | 跨 AS 生命周期归因与恢复 |
| `os/core/src/{sched.rs,monitor/mod.rs}` | monitor 读串口前服务 Runnable 任务 |
| `os/core/src/{component/endpoint.rs,machine.rs}` | endpoint 值投影与 unfiltered device discovery |
| `Makefile` / `tests/qemu/runner.py` | 构建、格式 / lint / host 与串口 smoke 接线 |
| `tests/qemu/ksh.py` | CoreTest 后的串口用户流程 smoke，纳入 `make test-qemu` |

host 验证空行 / whitespace / tokenization / 命令识别 / 错参 / 未知命令 / 域解析 / 输入上限
与恢复；Core host 验证查询拷贝边界、端口失效、unfiltered discovery 与跨 AS 生命周期归因。
QEMU smoke 验证真实串口、组件查询、native / isolated load、失败后 shell 存活、cat、
超长行恢复、exit 并回到 monitor。完整验证记录见 `STATUS.md`。

## 应用执行范围

`exec 0:/APP.ELF [args...]` 已能从真实 FAT 文件运行静态 RV64 ELF。当前需要唯一
filesystem provider，读取上限 1MiB；stdin 为 EOF、stdout/stderr 为 SDK console。
进程故障或非法 ELF 不结束 shell。POSIX profile 的镜像 key 固定为 `/main`，通用
execve 路径、cwd、动态链接、文件 fd、重定向 / pipe 和 Win32 未实现；不支持 `./hello`
隐式查找。实施与验证见 [userspace.md](../development/userspace.md)。
