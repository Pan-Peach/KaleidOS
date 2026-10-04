# init（os/components/init/）

> 现状描述：KernelNative 的最小启动编排组件。生命周期以
> `docs/architecture/component-lifecycle.md` 为准；不是 POSIX PID1。

## 启动链

普通 RISC-V profile 的 `CONFIG_BOOT_COMPONENT="init"`。boot 在 Core、长期地址空间、
组件仓库与中断就绪后，仅调用一次既有 `load_and_start` 装载指定 artifact；RV64 的 AP
也已启动。组件名通过 Kconfig → genmk.py → Make 环境变量 → boot 的 `option_env!` 传递，
不写在 Core 内。`SELFTEST` 镜像绕过这条入口。

`init` 的 create 同步执行以下固定策略：

1. 加载 `scheduler_rr`，查找并选择它的 `scheduler.policy` endpoint。
2. 加载 `driver_prober`，经既有 `kcore_sched_run` 跑完当前有限、不 yield 的 dispatch 任务。
   prober 创建 `virtio_blk`，由驱动自己认领设备，遇到首个 Match 停止。
3. 从 Core 值查询中选择唯一 Live、exact ABI 匹配的 `block.device` endpoint。
   没有块端口则启动纯 console 会话；多个候选返回 EBUSY。
4. 把选中的 opaque EndpointId 用 FatFs 的扁平 create config 传给 `fatfs`。
   FatFs 用 memcpy 解码到本地结构，不要求字节负载具有 u64 对齐。
   创建 init 自己的有限任务，在任务上下文 bind / mount filesystem，结果写回实例私有原子。
   FAT 挂载不格式化磁盘；保留 transport / method 分类的错误诊断。
5. 挂载成功后加载 `ksh`。init 的 create 返回 0，Core 提交 Ready；monitor 的调度安全点
   随后运行 shell 任务。没有块盘也可正常进入 ksh，此时 cat 报缺少 filesystem provider。

这是 provider root 挂载，没有 VFS namespace 或 `/` 路由；shell 使用 `0:/HELLO.TXT`。
init 不导出新服务，也没有新增 Core ABI。SDK 只增加既有 `component_create` 和 `sched_run`
的安全包装；Core 仍裁决实例、端口、任务与设备所有权。

## 失败与生命周期

load / config / mount 失败使 create 返回 errno，Core 留下 Failed 的 init 记录；boot 打印
错误后进入 monitor。已成功创建的其它组件各自保留其生命周期与端口，供诊断与手动 unload，
没有整张图的事务回滚。缺失的启动 artifact 同样回到 monitor。

init 完成后保持 Ready，没有常驻管理任务；退出 ksh 回到 monitor，不自动重启会话。
卸载 init 不级联卸载它组合出的独立组件。当前没有 reaper、watchdog、依赖解析、热插拔或
自动恢复；从 shell 任务再 load init 会在改变图之前返回 EINVAL（需要 boot/monitor 锚点）。

`sched_run` 返回只代表控制回到锚点，不是 join。当前策略依赖 prober dispatch 和挂载任务
有限且不 yield；未来异步设备等待 / prober 必须引入组件侧完成契约，不能直接沿用这条假设。

## 运行与验证

```sh
make qemu_rv64_defconfig       # RV32 可换 qemu_rv32_defconfig
make qemu                     # 自动 init → FAT root → ksh
```

```text
ksh> cat 0:/HELLO.TXT
HELLO FROM KALEIDOS FAT ROOTFS
ksh> exit
core>
```

`make monitor_defconfig` 在当前 profile 上设置 `CONFIG_BOOT_COMPONENT=""`，下次启动直接
进入 monitor，可手动 `load init`；重新选择 board defconfig 恢复默认 init。
`make core` 的空仓库会使配置选中的 init 缺失，仍可回到 monitor。

实现位于 `os/components/init/src/{runtime,root}.rs`，boot 入口位于
`os/boot/riscv/src/composition.rs`。host 用例检查根端口歧义与生命周期 / 指纹过滤。
`make test-init` 的 RV64/RV32 各跑 FAT、no-block、bad-fat 三条真实串口流程，包括读取文件、
init 状态、拒绝 task-context 重入、坏盘后的 monitor 恢复、shell exit 与 shutdown。
它同时纳入 `make test-qemu`；原 CoreTest 流程使用 `configs/monitor.fragment` 自行组合图。
CoreTest 的 block-chain 额外把 FatFs 配置放在对齐缓冲的偏移 1 字节处，验证非对齐负载。
RV32 NoMMU 也已用独立 profile 跑过 FAT 自动启动与文件读取流程；证据见 `STATUS.md`。
