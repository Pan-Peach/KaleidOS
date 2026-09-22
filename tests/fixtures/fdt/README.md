# FDT fixture（`qemu-virt.dts`）

`qemu-virt.dts` 是 **QEMU virt 机器的真实 DTB 转储**（`dtc` 反编译成 DTS），
不是手写近似。它给 machine discovery 的解析器当参考 / fixture；运行时 DTB 由
机器经 `a1` 传入，本文件不参与引导（见 `docs/architecture/overview.md`）。

## 再生成（regenerate）

```sh
# 1) 让 QEMU 把 virt 机器的 DTB 写到文件（QEMU 会预分配 1 MiB，DTB 在文件开头）
qemu-system-riscv64 -machine virt,dumpdtb=/tmp/qemu-virt.dtb -smp 1 -m 1G -nographic -display none

# 2) 反编译成 DTS（dtc 来自 device-tree-compiler 包）
dtc -I dtb -O dts /tmp/qemu-virt.dtb > tests/fixtures/fdt/qemu-virt.dts

# 3) 可选：把命令写回文件头的注释块（转储本身不带注释）
```

本仓库当前转储环境与事实：

- QEMU **5.2.0**（Ubuntu focal），命令 `-smp 1 -m 1G`；
- `totalsize = 3686` 字节、27 个节点 / 105 条属性；
- `/soc/pci@30000000` 带 `dma-coherent`；`virtio_mmio@1000X000`（irq 1..8）与
  `uart@10000000`（irq 10）**不带任何 DMA 属性** —— 这是 core_test DMA 授权模型
  不建模“设备是否为 DMA master”的 FDT 依据（见 `docs/development/testing.md` §3）；
- `plic@c000000` 的 compatible 是 `riscv,plic0`、reg 大小 `0x210000`；
- `cpus` 节点的 `riscv,isa` / `mmu-type` 随本次命令的目标架构变化（本转储是
  RV64：`rv64imafdcsu` / `riscv,sv48`）。DTS 描述的是**机器**，但 CPU 属性不独立于
  转储目标。

## 为什么不再手写

手写近似会与真实机器漂移，而漂移不会被发现：旧 fixture 完全没有任何 DMA 属性、
PLIC 写成 `sifive,plic-1.0.0` / `0x4000000`、内存大小也与实际命令不符。
真实 DTB + 反编译保证“fixture 里的机器 == 跑测试的机器”；命令写在这里，
可以随时再生成。若后续为 discovery 加 host test，请直接使用本文件（或它转出来的
DTB），不要再手写第二份。

## 现状说明

当前仓库**没有**直接解析本文件的 host test：machine discovery 的解析器在
`os/boot/riscv`（boot 层，按架构约定不可 host 测）。本文件当前的作用是：
机器事实的单一参考（CoreTest / 文档 / 排障）+ 未来 discovery 提取出可 host 测的
纯解析层时现成的 fixture。发现 fixture 与真实机器不一致时，先改转储，不要改手写副本。

