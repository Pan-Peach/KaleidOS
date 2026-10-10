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
   prober 创建 `virtio_blk`，由驱动自己认领设备，完成所有候选；每台 Match 设备有独立驱动实例。
3. 从 Core 值查询中按 profile 指定序号选择 Live、exact ABI 匹配的 `block.device`。
   默认策略选择第 0 个，枚举顺序属于本启动 profile；增加第二张盘不使根选择歧义。
   可用 config ABI `0x494E_4954_524F_4F54` + 4 字节 native-endian u32 指定序号，
   不要求字节对齐。指定非零序号不存在返回 ENODEV；默认无盘进入纯 console 会话。
4. 创建FatFs：LE config为block EndpointId u64、control ComponentId u32、reserved=0 u32。
   Fat仅发布IPC，config fingerprint以 filesystem schema 为准；init作为真实创建祖先显式grant Block给Fat。运行任务让listener就绪。
5. 创建VFS：control u32、mount count u32、FS EndpointId列表u64；grant Fat给VFS。
   VFS在自己的Server Task中mount/root，把Remote Fat挂到`/fat`，Local始终存在。
6. init的有限Task通过VfsBinding取root/release确认bootstrap结果，写回原子状态。
   成功后ksh config为VFS Endpoint LE u64加选定根路径`/fat`（无盘为`/`），grant VFS给ksh。

shell使用同一namespace，`0:/HELLO.TXT`映射选定根，绝对`/fat`/`/local`由VFS解析。
没有块盘也可cat `/local/README.TXT`；缺失文件明确报错。init不导出新服务，也不自动
格式化FAT或另一张盘。Core只裁决实例/Task/Endpoint/设备/grant，不知道mount语义。

## 失败与生命周期

load / config / mount 失败使 create 返回 errno，Core 留下 Failed 的 init 记录；boot 打印
错误后进入 monitor。已成功创建的其它组件各自保留其生命周期与端口，供诊断与手动 unload，
没有整张图的事务回滚。缺失的启动 artifact 同样回到 monitor。

init 完成后保持 Ready，没有常驻管理任务；退出 ksh 回到 monitor，不自动重启会话。
卸载 init 不级联卸载它组合出的独立组件。init没有常驻reaper、watchdog、依赖解析、热插拔或
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
`os/boot/riscv/src/composition.rs`。host 用例检查显式序号选择与生命周期 / 指纹过滤。
`make test-init` 在RV64跑FAT、dual-fat、OOM、no-block、bad-fat五条，RV32除OOM外四条，包含Local/Remote路径、读取文件与RV64 ELF exec、
init 状态、拒绝 task-context 重入、坏盘后的 monitor 恢复、shell exit 与 shutdown。
它同时纳入 `make test-qemu`；原 CoreTest 流程使用 `configs/monitor.fragment` 自行组合图。
CoreTest 的 block-chain 额外把 FatFs 配置放在对齐缓冲的偏移 1 字节处，验证非对齐负载。
RV32 NoMMU 也已用独立 profile 跑过 FAT 自动启动与文件读取流程；证据见 `STATUS.md`。
