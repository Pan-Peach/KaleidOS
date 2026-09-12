//! kcomp_panic —— panic containment 的**真实 `.kcomp`** 测试组件（step 2 D）。
//!
//! 与 `kcomp_smoke` 不同，这里故意在 `kcomp_init` 里 `panic!`：panic 进入的是
//! 组件私有 SDK panic adapter（本镜像自带），它打印诊断后调 `kcore_panic_escape`
//! 协作式逃逸，Core 的 init 边界负责把该 instance 提交为 Failed。
//!
//! 这是「真实加载的 `.kcomp` 的 panic 被 Core 容纳」的端到端证明——不是 boot
//! 镜像里直接调 `containment::call_on_isolated_stack` 的白盒用例。

#![no_std]

// 引入 SDK 的 panic adapter / panic_escape 绑定（组件私有携带）。
use kcomp_sdk as _;

kcomp_sdk::kcomp_init!({
    panic!("kcomp_panic: deliberate panic for containment test");
});
