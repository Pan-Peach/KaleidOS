//! 每实例 **runtime slot**：`ComponentId → 运行时自有的 opaque 状态指针` 的
//! 稳定位置（`docs/architecture/memory-and-heap.md` §5 的先行条件）。
//!
//! 组件运行时（SDK）把每个实例的运行时上下文（堆指针等）放在这里；Core
//! 只**存 / 取**这个指针，**从不解释、从不解引用**。切换路径把 slot 装进
//! RISC-V `tp` 寄存器（见 `arch::CpuArch::install_runtime_slot` /
//! `set_context_slot`），所以组件代码总在自己的 runtime context 下运行：
//!
//! ```text
//! tp = 当前执行的实例 runtime slot；0 = 无 slot
//! Core 是 tp 的唯一写者（psABI: tp unallocatable/fixed）
//! trap 帧已经保存 / 恢复 tp（TrapFrame.x[4]）
//! ```
//!
//! # 这是执行状态，不是内存记账
//!
//! 本表**不是**账本：没有 owner 记录、没有 region 注册表、没有字节计费。Core
//! 不知道指针指向什么、多大、归谁；`clear` 只丢掉指针，**不释放任何内存**
//! （释放是运行时 / `kcore_memory_release` 的事）。
//!
//! # 生命周期
//!
//! - **安装**（`install`）：由 SDK 经窄 ABI 在实例建立时完成（未安装时 slot 为
//!   0，所有路径行为不变）。
//! - **清除**（`clear`）：实例死亡（`failure::fail_component`）与优雅停止
//!   （`component/exit.rs`）时由 Core 清除，避免死实例的 runtime context 再被
//!   任何执行带着跑。
//!
//! 表按 `ComponentId` 线性查找（实例数很小，与 `registry` 同款）。

use crate::component::ComponentId;
use alloc::vec::Vec;
use spin::{Mutex, Once};

/// 运行时自有的 opaque 状态指针；Core 只存 / 取，从不解引用。
/// `0`（NULL）= 无 slot。
pub type RuntimeSlot = *mut ();

/// 一条 `ComponentId → slot` 绑定。
#[derive(Debug, PartialEq)]
struct SlotRecord {
    component: ComponentId,
    slot: RuntimeSlot,
}

// `slot` 是组件 opaque 指针：本表只存取、永不解引用。跨线程使用由外层
// `Mutex` 串行化（与 `registry::ComponentRecord::instance_state` 同一理由）。
unsafe impl Send for SlotRecord {}
unsafe impl Sync for SlotRecord {}

/// 每实例 runtime slot 表（Core 侧存储；可构造，测试友好）。
pub struct RuntimeSlotTable {
    records: Vec<SlotRecord>,
}

impl RuntimeSlotTable {
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
        }
    }

    /// 安装（或替换）`component` 的 runtime slot。
    ///
    /// Core 原样保存指针，**从不解释**；`null` 等价于 [`Self::clear`]。
    /// 不做存在性校验：调用方是 Core，键是身份 —— 表不建第二份生命周期真相。
    pub fn install(&mut self, component: ComponentId, slot: RuntimeSlot) {
        if slot.is_null() {
            self.clear(component);
            return;
        }
        match self.records.iter_mut().find(|r| r.component == component) {
            Some(record) => record.slot = slot,
            None => self.records.push(SlotRecord { component, slot }),
        }
    }

    /// 清除 `component` 的 runtime slot（实例死亡 / teardown）。
    ///
    /// 未知 id 是 no-op —— 清除不得让任何 teardown 路径失败。
    pub fn clear(&mut self, component: ComponentId) {
        self.records.retain(|record| record.component != component);
    }

    /// `component` 当前安装的 slot；`null` = 无（未知 id 同样是 `null`）。
    pub fn get(&self, component: ComponentId) -> RuntimeSlot {
        self.records
            .iter()
            .find(|record| record.component == component)
            .map_or(core::ptr::null_mut(), |record| record.slot)
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

impl Default for RuntimeSlotTable {
    fn default() -> Self {
        Self::new()
    }
}

// 进程全局表：**惰性初始化**（首次使用）—— 边界路径（任务切换 / 隔离栈）在
// host 测试里不经过 `core::init`，惰性初始化让两边共用同一份真相。
static SLOTS: Once<Mutex<RuntimeSlotTable>> = Once::new();

/// 取全局 slot 表（首次调用时初始化）。
pub fn get_slots() -> &'static Mutex<RuntimeSlotTable> {
    SLOTS.call_once(|| Mutex::new(RuntimeSlotTable::new()))
}

/// `component` 当前 slot 的寄存器宽度值（`0` = 无）。
///
/// 边界辅助：切换路径把它装进 incoming 执行的 runtime slot 寄存器。
pub(crate) fn slot_of(component: ComponentId) -> usize {
    get_slots().lock().get(component) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(raw: u32) -> ComponentId {
        ComponentId::from_raw(raw)
    }

    /// 纯逻辑：install / get / clear 按实例独立，未知 id 是 `null`，`clear`
    /// 未知 id 是 no-op。用局部表（零全局状态，无需串行锁）。
    #[test]
    fn install_get_and_clear_are_per_instance() {
        let mut table = RuntimeSlotTable::new();
        let a = id(1);
        let b = id(2);
        let mut state_a = 0xA1u8;
        let mut state_b = 0xB2u8;
        let slot_a = core::ptr::addr_of_mut!(state_a).cast::<()>();
        let slot_b = core::ptr::addr_of_mut!(state_b).cast::<()>();

        // 出生：无 slot。
        assert!(table.get(a).is_null());
        assert!(table.get(b).is_null());
        assert!(table.get(id(99)).is_null(), "未知 id = 无 slot");

        // 安装按实例独立；Core 原样保存指针。
        table.install(a, slot_a);
        table.install(b, slot_b);
        assert_eq!(table.get(a), slot_a);
        assert_eq!(table.get(b), slot_b);
        assert_eq!(table.len(), 2);

        // 清除一个不影响另一个；未知 id 的 clear 是 no-op。
        table.clear(a);
        assert!(table.get(a).is_null());
        assert_eq!(table.get(b), slot_b, "另一个实例的 slot 原样");
        table.clear(id(99));
        assert_eq!(table.len(), 1);
    }

    /// 重复安装 = 原地替换（位置稳定，绝不产生第二条记录）。
    #[test]
    fn install_replaces_previous_slot_in_place() {
        let mut table = RuntimeSlotTable::new();
        let a = id(7);
        let mut first = 1u8;
        let mut second = 2u8;
        let slot_first = core::ptr::addr_of_mut!(first).cast::<()>();
        let slot_second = core::ptr::addr_of_mut!(second).cast::<()>();

        table.install(a, slot_first);
        table.install(a, slot_second);
        assert_eq!(table.get(a), slot_second);
        assert_eq!(table.len(), 1, "替换不新增记录");
    }

    /// `install(null)` 等价于 clear（0 = 无 slot）。
    #[test]
    fn install_null_clears_the_slot() {
        let mut table = RuntimeSlotTable::new();
        let a = id(3);
        let mut state = 1u8;
        table.install(a, core::ptr::addr_of_mut!(state).cast::<()>());
        assert!(!table.get(a).is_null());

        table.install(a, core::ptr::null_mut());
        assert!(table.get(a).is_null());
        assert!(table.is_empty());
    }
}
