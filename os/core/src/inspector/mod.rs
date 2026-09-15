//! TestInspector —— Core 对外暴露的**只读**测试观察口（见 docs/testing.md）。
//!
//! 约束（Oracle 审查结论）：只能读取 Core 状态用于断言，不能修改任何状态（无 god-mode）；
//! 返回**快照副本**而非内部引用；构造器仅 Core 私有，测试组合收到已建好的只读门面。
//!
//! 禁止：mutate task / grant handle / force scheduler / patch Core state。
//! 只返回 value copy、snapshot 或 visitor 结果，绝不泄露 Core 私有结构的可变引用；
//! CoreTest 不允许因此获得 god-mode。
//!
//! 已实现（第一阶段只做 CoreTest 真正会用的数据）：
//! - task(TaskId) -> Option<TaskSnapshot>         owner / state / CPU 投影
//! - component(ComponentId) -> Option<ComponentSnapshot>
//! - memory_region(base) -> Option<MemoryRegionSnapshot>   来自已提交 MachineInfo
//! - visit_trace_since(seq, visitor)             只读遍历 trace（纯转发）
//!
//! 观察结果类型在 [`snapshot`]。
//! 后续可再增加 handle / interface registry / irq ownership / dma / address-space
//! snapshot，但优先只支持现有 CoreTest 真正会使用的数据。

pub mod snapshot;

pub use snapshot::{ComponentSnapshot, MemoryRegionSnapshot, TaskSnapshot};

use crate::component::ComponentId;
use crate::task::{TaskId, TaskState};
use crate::trace::TraceRecord;

/// 只读观察门面。
///
/// 它是一个 **ZST**：没有任何字段，因此不可能持有 Core 内部结构的引用；所有
/// 方法只读全局真相并返回值拷贝。构造器 `pub(crate)` 私有 —— 外部（含组件）
/// 只能拿 Core 交付的实例，new 不出一个"更宽"的版本。
#[derive(Debug)]
pub struct Inspector;

impl Inspector {
    /// 仅 Core（含 crate 内测试）构造；对外由 Core 交付（见 `component/export`）。
    // 目前只有 crate 内测试构造它；组件侧的交付入口在 Phase 4 随 export 落地。
    #[allow(dead_code)]
    pub(crate) const fn new() -> Self {
        Self
    }

    /// 某个任务此刻的真相（不存在 → `None`）。
    ///
    /// 前置：`task::init()` 已调用（`core::init` 保证）。
    pub fn task(&self, id: TaskId) -> Option<TaskSnapshot> {
        let table = crate::task::get_task_table().lock();
        let record = table.get(id)?;
        let state = record.state();
        let running_on = match &state {
            TaskState::Running(cpu) => Some(*cpu),
            _ => None,
        };
        Some(TaskSnapshot {
            id,
            owner: record.owner(),
            state,
            running_on,
        })
    }

    /// 某个组件此刻的真相（未声明 / 已卸载 → `None`）。
    ///
    /// 前置：`component::registry::init()` 已调用（`core::init` 保证）。
    pub fn component(&self, id: ComponentId) -> Option<ComponentSnapshot> {
        let registry = crate::component::registry::get_registry().lock();
        let record = registry.get(id)?;
        Some(ComponentSnapshot {
            id: record.id,
            state: record.state,
            base: record.base,
            entry: record.entry,
        })
    }

    /// 已提交的物理内存 region 快照，按 `base` 查（未提交 / 无此 base → `None`）。
    pub fn memory_region(&self, base: usize) -> Option<MemoryRegionSnapshot> {
        let info = crate::machine::committed()?;
        info.memory_regions[..info.mem_count]
            .iter()
            .find(|region| region.base == base)
            .map(|region| MemoryRegionSnapshot {
                base: region.base,
                size: region.size,
            })
    }

    /// 只读遍历 trace（`seq >= since`），**纯转发**给 [`crate::trace::visit_since`]。
    ///
    /// visitor 在锁**外**调用（有界实时遍历，不是原子快照）：可以安全地
    /// emit / 查 `stats()`，不会死锁；遍历期间的新写入不参与本次遍历。
    pub fn visit_trace_since(&self, since: u64, visitor: impl FnMut(&TraceRecord)) {
        crate::trace::visit_since(since, visitor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::ComponentState;
    use crate::machine::{CpuId, CpuInfo, DeviceDescriptor, MachineInfo, MemoryRegion};

    /// 串行化所有触碰全局真相（task table / registry / machine 快照）的测试。
    static INSPECTOR_TEST_LOCK: spin::Mutex<()> = spin::Mutex::new(());

    /// 快照必须是**拷贝**，不是活引用：Core 真相随后改变，旧快照不动。
    #[test]
    fn task_snapshot_is_a_value_copy_not_a_live_view() {
        let _serial = INSPECTOR_TEST_LOCK.lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        crate::task::init();

        let owner = ComponentId::from_raw(0x51);
        let id = crate::task::get_task_table()
            .lock()
            .create(owner, 0x1000)
            .expect("create task");
        {
            let mut table = crate::task::get_task_table().lock();
            table.transition(id, TaskState::Runnable).unwrap();
            table.transition(id, TaskState::Running(CpuId(0))).unwrap();
        }

        let inspector = Inspector::new();
        let snapshot = inspector.task(id).expect("task exists");
        assert_eq!(snapshot.owner, owner);
        assert_eq!(snapshot.state, TaskState::Running(CpuId(0)));
        assert_eq!(snapshot.running_on, Some(CpuId(0)));

        // 改掉 Core 真相（并且让它消失）之后，旧快照必须原样不动。
        {
            let mut table = crate::task::get_task_table().lock();
            table.transition(id, TaskState::Runnable).unwrap();
            table.remove(id).unwrap();
        }
        assert_eq!(snapshot.state, TaskState::Running(CpuId(0)));
        assert_eq!(snapshot.running_on, Some(CpuId(0)));
        assert!(inspector.task(id).is_none(), "移除后真相里就没有它了");
    }

    /// 组件快照跟随注册表真相，且不随后续转换而变。
    #[test]
    fn component_snapshot_reflects_registry_truth() {
        let _serial = INSPECTOR_TEST_LOCK.lock();
        crate::component::registry::init();

        let id = crate::component::registry::get_registry()
            .lock()
            .declare(b"inspector-probe", 0x1234, 0x1000, None)
            .expect("declare");

        let inspector = Inspector::new();
        let snapshot = inspector.component(id).expect("component exists");
        assert_eq!(snapshot.id, id);
        assert_eq!(snapshot.state, ComponentState::Declared);
        assert_eq!(snapshot.base, 0x1000);
        assert_eq!(snapshot.entry, 0x1234);

        crate::component::registry::get_registry()
            .lock()
            .resolve(id)
            .expect("resolve");
        assert_eq!(
            inspector.component(id).expect("still exists").state,
            ComponentState::Resolved
        );
        assert_eq!(
            snapshot.state,
            ComponentState::Declared,
            "旧快照不随后续变化"
        );
    }

    /// 内存 region 快照来自已提交的 MachineInfo，按 base 查（未命中 → None）。
    #[test]
    fn memory_region_snapshot_comes_from_committed_machine_info() {
        let _serial = INSPECTOR_TEST_LOCK.lock();
        let _machine = crate::machine::test_support::GUARD.lock();

        crate::machine::commit(MachineInfo {
            boot_hart: 0,
            timebase_frequency: 10_000_000,
            cpu_count: 1,
            cpu_info: [CpuInfo {
                boot_cpu: true,
                hart_id: CpuId(0),
            }; 8],
            mem_count: 1,
            memory_regions: [MemoryRegion {
                base: 0x8000_0000,
                size: 0x4000,
            }; 16],
            dev_count: 0,
            devices: [DeviceDescriptor::empty(); 26],
        });

        let inspector = Inspector::new();
        let snapshot = inspector.memory_region(0x8000_0000).expect("region");
        assert_eq!(snapshot.size, 0x4000);
        assert!(
            inspector.memory_region(0xdead_beef).is_none(),
            "未命中必须是 None，而不是编造一个 region"
        );
    }

    /// trace 遍历是纯转发，且 `since` 过滤语义与 ring 一致。
    ///
    /// 只在 `CONFIG_TRACE=y` 时有意义：trace 编译掉后 `emit` 是空操作，
    /// 没有可遍历的事件。
    #[test]
    #[cfg(feature = "trace")]
    fn visit_trace_since_forwards_to_the_ring() {
        use crate::trace::{TraceEvent, emit, reset_for_test};
        use alloc::vec::Vec;

        let _serial = INSPECTOR_TEST_LOCK.lock();
        let _trace = crate::trace::test_support::GUARD.lock();
        reset_for_test();
        emit(TraceEvent::TaskSwitch {
            from: None,
            to: TaskId::from_raw(1),
        });
        emit(TraceEvent::IrqAck { irq: 9 });

        let inspector = Inspector::new();
        let mut seqs = Vec::new();
        inspector.visit_trace_since(2, |record| seqs.push(record.seq));
        assert_eq!(seqs, [2], "seq 1 应被 since 过滤掉");
    }
}
