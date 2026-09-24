//! C 前端冒烟：加载**真实** `kcomp_c_smoke` fixture（clang 编出的 freestanding C
//! `.kcomp`，不是内联到 CoreTest 的 C 代码），断言它走完完整的前端链路：
//!
//! ```text
//! store（cpio）→ loader（ELF32/ELF64 ET_REL 放段 + RV32/RV64 重定位 +
//! kcore_* 白名单校验 + kcomp_abi 指纹）→ C kcomp_instance_create 执行 → Ready
//! ```
//!
//! 断言 = Core 自己报告的可观测结果：`kcore_component_load` 返回的 instance id +
//! 该 id 的**确切**生命周期 trace（`Declared → Resolved → Starting → Ready`）。
//! C 组件打印的 `[c-smoke] hello from C` 是**串口输出**：组件边界内没有读取它的
//! Core API，精确 stdout 字节**故意不断言**（过细，留空间）；monitor
//! `load` / `unload kcomp_c_smoke` 的 UX 与 destroy 路径由 runner 在机器级检查。

use kcomp_sdk::abi::kcore_component_load;

use super::report::Checks;
use super::trace;

/// C 前端 fixture 的 artifact 名（= `init.kpkg` 里的文件名）。
const C_SMOKE: &[u8] = b"kcomp_c_smoke";

/// 加载 fixture 并断言其生命周期。
pub fn run(checks: &mut Checks) {
    checks.group("c frontend");
    let cursor = trace::cursor();
    let id = unsafe { kcore_component_load(C_SMOKE.as_ptr(), C_SMOKE.len()) };
    checks.check(
        41,
        "c-frontend",
        id >= 0 && trace::component_lifecycle(cursor, id),
    );
}
