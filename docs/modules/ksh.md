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
| `help [command]` | 列出命令与编辑快捷键，或显示单条命令用法与说明 |
| `echo [args...]` / `clear` / `exit` | 参数间输出一个空格，引号内空格保留；ANSI 清屏、结束任务 |
| `history` | 显示最近 8 条非空提交行（包含 history 自己），相邻重复行不重复登记 |
| `components` | 所有已登记实例的 ID / artifact 名 / execution domain / state，含终态记录 |
| `endpoints` | ID / provider ID / port / 名称 / contract 数值 / Pending、Live、Invalid |
| `devices` | DeviceId / 主 compatible / 认领状态 / owner 组件名与 ID；主 MMIO 或 PIO 窗口、首个 IRQ 与资源总数 |
| `load <artifact> [native\|isolated]` | 默认 native；可省略 `.kcomp`；Core 验证请求，失败打印 errno；不隐式回退域 |
| `inspect <loaded-artifact>` | 复用组件查询，显示该 artifact 的所有已加载实例、KCOMP 格式、域与状态 |
| `cat <provider-relative-path>` | 使用 init 指定的 filesystem endpoint；无配置时查找唯一 Live、exact ABI 匹配端口，bind / mount / open / read / close |
| `exec <provider-relative-path> [args...]` | 经选定 FS 读静态 ELF，创建 POSIX 镜像快照进程族，等待退出并显示 exit / signal |
| `ls` / `cd` / `pwd` | 明确报告 unsupported：现有 FS 没有目录枚举或工作目录 / namespace 契约 |

`inspect` 当前不读取未加载 artifact 或任意路径的字节，也不解析 ELF / PE / WASM。
这需要外置的只读文件 / artifact inspection helper；Core 不新增格式识别 API。

init 经 config ABI `0x4B53_4846_5343_4647` + 8 字节 native-endian u64 交付 FS
EndpointId；0 表示无预设（空 config 也表示无预设），非零 ID 在 create 时校验 exact
contract/ABI/liveness。config 无对齐要求，两个 32 位原子仅在 create 写入、任务启动后
只读，保持完整 ID 并支持 RV32。`cat` / `exec` 复用同一选择，不再每次全局重选。

monitor 无配置加载 shell 时仍只接受唯一 provider，多个 Live FS 明确报歧义。
路径解释交给 provider；FAT 示例为 `0:/HELLO.TXT`，littlefs 使用其自身路径。
mount 幂等；退出 shell 不 unmount 共享 provider。传输与方法错误分别保留，打开的
文件在正常完成或 read 错误后都 close。SDK read 接受普通数据缓冲区，不向 shell
暴露 Gate 的长度头；Direct 直接写入，Gate 单次最多 512 字节数据、校验后复制。

## 输入与编辑

输入最多 512 字节、17 个词（含命令），使用固定缓冲区；解析、编辑、历史和基础命令
不要求堆分配。输入限 ASCII；非 ASCII 字节或超长输入使整行失效，丢弃到换行后恢复，
绝不执行截断前缀。支持 CR / LF / CRLF，Ctrl-C 取消也能恢复失效行。
会话输入 / 历史 / 解码缓冲放在各实例自己的可写 image，仅由该实例的唯一会话任务访问，避免占用
Core 分配的单 granule 任务栈；不另建会话组件或扩大 Core 栈机制。

| 按键 | 行为 |
|---|---|
| ↑ / ↓ 或 Ctrl-P / Ctrl-N | 遍历最近 8 条历史；回到最新条目之后恢复原草稿及光标 |
| ← / → 或 Ctrl-B / Ctrl-F | 移动光标，普通字符插入光标处 |
| Home / End 或 Ctrl-A / Ctrl-E | 移到行首 / 行尾，支持 CSI 与 SS3 序列 |
| Backspace / Delete | 删除光标前 / 光标处字符 |
| Ctrl-U / Ctrl-K / Ctrl-W | 删除光标前内容 / 后内容 / 前一个词（按空格分界） |
| Tab | 在首个命令词末尾补全；唯一候选加空格，多候选列出并补公共前缀；不补文件名 |
| Ctrl-L / Ctrl-C / Ctrl-D | 清屏保留输入 / 取消当前输入 / 删除光标处字符，空行时退出 |

QEMU `-nographic` 自身使用 Ctrl-A 作为控制前缀；连续按两次 Ctrl-A 才向 shell 发送一次，
也可以直接用 Home 回到行首。

编辑重绘显示光标附近最多 64 个字符，避免长行在常见 80 列 ANSI 串口终端换行；
解析仍使用完整行。未知 CSI 序列不进入命令文本。历史保存原始提交行，不保存取消、
溢出、非 ASCII 输入；回忆后编辑不会改写已保存的历史。

单引号保留内部字节；双引号保留空格，`\"` 与 `\\` 分别表示引号与反斜杠。
双引号内的其他反斜杠保留；引号外反斜杠转义下一个字符。相邻片段拼成一个参数，
空引号保留空参数。未闭合引号、末尾反斜杠先拒绝整行，再分派命令。
词首未引用的 `#` 开始注释；词内 `#` 是普通字符。无变量 / glob / 命令替换或脚本；
未引用 / 转义的 `| & ; < >` 明确拒绝整行，不执行管道、重定向或命令列表的前缀。

```text
ksh> echo "hello  world" 'two words' escaped\ space
hello  world two words escaped space
ksh> cat "0:/HELLO WORLD.TXT"
ksh> exec 0:/APP.ELF "one argument" ''
ksh> help exec
```

带空格路径在解析后作为一个参数交给 provider；是否支持这种文件名由后端决定。
当前 FatFs 关闭 LFN，仍只接受短文件名；可以用 `cat "0:/HELLO.TXT"` 验证引号读取。

## 设备显示

`devices` 枚举固件交给 Core 的设备记录。当前 `DeviceDescriptor` 没有独立的节点名称，
因此设备列使用已有的主 compatible；认领者通过组件值查询显示为 `artifact#id`。
没有 compatible 时显示 `<unspecified>`，未认领或已隔离设备的 owner 显示 `-`。
`Claimed` 表示已有 owner，不承诺驱动 Ready 或设备工作正常；`Quarantined` 表示失败后
保持隔离到 reboot。设备描述也包括空的 virtio transport，记录数不等于工作的外设数。

详情显示固件主窗口的地址 / 长度，以及首个中断资源的逻辑 IRQ：没有中断资源显示 `-`，
有资源但没有可投递线号显示 `unmapped`。多 compatible、多窗口或多中断资源注明总数，
不把第一项伪装成全部。显示过程不 claim、不读寄存器、不推测 virtio 子类型。
Core 完整复制主 compatible，缓冲不足报 ENOBUFS；shell 使用 256 字节缓冲，失败行报告
查询错误后继续枚举。设备行是单次快照，owner 名称另查，不承诺整个表的原子快照。
查询布局与错误码以 `abi/core.toml` 为准，所有权语义见
[`driver-model.md`](../architecture/driver-model.md#121-已决设备选择原-q1)。

## Core ABI 的最小补充

shell 使用的观察导出（源在 `abi/core.toml`）：

| 导出 | 必要性与边界 |
|---|---|
| `kcore_console_read_byte` | 当前 console 尚无组件服务；复用已有 arch console 输入与有界 idle wait，SDK 屏蔽后端 |
| `kcore_component_nth` | Core 独占组件存在与状态；复制固定值结构与完整名称，不交付 registry 指针 |
| `kcore_endpoint_nth` | Core 独占端口存在与状态；复制固定值结构与完整名称，不交付 provider 函数表 |
| `kcore_device_info` | Core 独占设备描述与 ownership；复制已有描述及 owner / quarantine 快照，不交付私有指针或访问权限 |

查询每次返回一行的快照，名称缓冲不足返回 ENOBUFS，不截断；不承诺整张表原子快照。
既有 `kcore_device_nth` 接受空 compatible 字节串以枚举全部设备；既有
`kcore_component_load` 增加 domain 请求参数，沿用 id / -errno 返回编码。后者改变 import
签名；后续 create 同步增加 domain。本次设备查询扩充 import 集，`KCOMP_ABI` 原地协调替换为
`0xD58F_B296_4E73_A10C`；旧组件明确不兼容，Core 与全部组件一起重建。
SDK / C provider / test fixture 的指纹同步更新，生成的声明与导出表由
`make abi-gen` 更新、`make abi-check` 检查。

复用的机制包括 console write、device discovery、组件 load、task start/yield/exit，以及
Endpoint / FileSystemBinding。设备显示复用已有描述，不加入设备命名策略、artifact parser 或通用 exec API。
从组件任务调用 Isolated create / destroy 时，Core 使用与 service 相同的跨 AS 身份纪律：
被调实例的 identity / 恢复边界覆盖 caller，组件入口期间挂起 Core ABI 深度，返回后恢复。

## 代码与验证

| 文件 | 用途 |
|---|---|
| `os/components/ksh/src/{parser,input}.rs` | 固定容量解析、行编辑、历史与命令补全；14 个 host 用例 |
| `os/components/ksh/src/shell.rs` | 确定性单行分派、查询显示、FS consumer；静态 ELF 读取与 POSIX profile 组合 |
| `os/components/ksh/src/runtime.rs` | 组件入口与交互任务 |
| `os/components/kcomp-sdk/src/{console,management}.rs` | 安全值前端与 task 包装 |
| `os/core/src/component/export/query.rs` | console 与组件 / endpoint / device 的窄观察导出 |
| `abi/{core,component}.toml` 与 Core / SDK generated 文件 | 查询布局、load 请求与协调指纹 |
| `os/core/src/component/{containment,isolated_lifecycle}.rs` | 跨 AS 生命周期归因与恢复 |
| `os/core/src/{sched.rs,monitor/mod.rs}` | monitor 读串口前服务 Runnable 任务 |
| `os/core/src/{component/endpoint.rs,machine.rs}` | endpoint 值投影与 unfiltered device discovery |
| `Makefile` / `tests/qemu/runner.py` | 构建、格式 / lint / host 与串口 smoke 接线 |
| `tests/qemu/ksh.py` | CoreTest 后的串口用户流程 smoke，纳入 `make test-qemu` |

host 验证引号 / 转义 / 空参数 / 注释、命令识别与错误拒绝、输入边界、历史草稿恢复、
光标插入 / 删除、控制键、补全、长行窗口和失效后恢复；Core host 验证查询拷贝边界、
端口失效、unfiltered discovery 与跨 AS 生命周期归因。
QEMU smoke 检查真实串口编辑 / 补全 / 历史、组件查询、设备描述与 owner 显示、native / isolated load、
失败后 shell 存活、超长行恢复与退出；init 流程另检查真实 FAT 路径的引号 / 转义读取、
引号 exec 参数与 RV64 内存耗尽后基础命令继续响应。完整验证记录见 `STATUS.md`。

## 应用执行范围

`exec 0:/APP.ELF [args...]` 已能从真实 FAT 文件运行静态 RV64 ELF。当前使用配置选定或无配置下唯一的
filesystem provider，读取上限 1MiB；stdin 为 EOF、stdout/stderr 为 SDK console。
进程故障或非法 ELF 不结束 shell。POSIX profile 的镜像 key 固定为 `/main`，通用
execve 路径、cwd、动态链接、文件 fd、重定向 / pipe 和 Win32 未实现；不支持 `./hello`
隐式查找。实施与验证见 [userspace.md](../development/userspace.md)。
