//! kcomp_panic —— panic containment 的**真实 `.kcomp`** 测试组件（step 2 D）。
//!
//! 与 `kcomp_smoke` 不同，这里故意在 `kcomp_instance_create` 里 `panic!`：panic
//! 进入的是组件私有 SDK panic adapter（本镜像自带），它打印诊断后调
//! `kcore_panic_escape` 协作式逃逸，Core 的 create 边界负责把该 instance 提交为
//! Failed。
//!
//! 这是「真实加载的 `.kcomp` 的 panic 被 Core 容纳」的端到端证明——不是 boot
//! 镜像里直接调 `containment::call_on_isolated_stack` 的白盒用例。

#![no_std]

// 引入 SDK 的 panic adapter / panic_escape 绑定（组件私有携带）。
use kcomp_sdk as _;

kcomp_sdk::kcomp_instance_create!(|_args, _out_state| {
    panic!("kcomp_panic: deliberate panic for containment test");
});

// 析构入口：create 必然 panic → 实例永不进入 Ready，destroy 路径不会到达这里；
// 显式 no-op 只为保持 ABI 形状（Core 对 panic / 未完整构造的实例不调 destroy，
// 见 docs/architecture/component-lifecycle.md §3）。
kcomp_sdk::kcomp_instance_destroy!(|_state| { 0 });
