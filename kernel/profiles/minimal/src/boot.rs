//! 启动编排（M0 目标）—— 属于最终镜像（profiles/），不属于 kernel。
//!
//! 流程：arch `_start(a0=hartid, a1=dtb)` → early console（QEMU 16550 直写，
//! 仅限 pre-Core 引导期；Core 就绪后 UART 必须走 MmioHandle 驱动）→
//! 校验 DTB header → `fdt::Fdt::parse` → 归一化 MachineInfo（固定容量数组，
//! 合并固件/内核镜像/DTB 自身保留区）→ Core 初始化 → compatible 匹配驱动 →
//! Core 授予类型化 Handle → 静态组件图启动。
//!
//! 验收：BOOT ARCH_ENTRY OK / BOOT DISCOVERY OK backend=fdt / BOOT MEMORY OK / BOOT CORE OK。