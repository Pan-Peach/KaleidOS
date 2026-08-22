# 测试策略（testing.md）

## 1. 开发者优先（Developer-First）原则

> **Core 逻辑必须 host-testable —— 永远不需要启动 QEMU 才能测试 Core。**

如果某个 Core 功能只能通过启动整个 OS 来测试，第一反应应该是：**它是不是和 Arch 耦合得太深了？**
正确姿势是把真相逻辑（任务状态机、所有权、handle 生命周期）做成与 Arch 无关的纯逻辑，在宿主上直接 `cargo test`。

架构原则：

> 如果某个 Core 功能必须启动整个 OS 才能测试，首先检查它是不是和 Arch 耦合得太深。

## 2. 测试金字塔

```text
              Real Hardware（真实硬件测试）
              QEMU CoreTest（板上集成测试）
          Concurrency Exploration（并发探索，未来）
        Model Checking（模型检查，未来）
      Property Test（属性测试）
    Host Test（宿主单测 —— 主体，日常主力）
```

- **Host Test**：Core 的一切真相逻辑（帧所有权、任务状态机、handle 生命周期、资源权限、组件生命周期、依赖解析器）都在宿主上测；
- **Property Test**：对 Core 的不变式做随机化验证（未来引入 proptest 类工具）；
- **Model Checking / Concurrency Exploration**：未来用 Kani / Loom 类工具（见 references.md）；
- **QEMU CoreTest**：集成验证 Arch/Platform/Core 的真实行为；
- **Real Hardware**：最终在真机上验证。

## 3. CoreTest 组件（components/core_test/）

CoreTest 是特殊的测试组件，运行在 QEMU / 真实硬件上，验证 Core 与 Arch 的**真实行为**。

### 验证清单（Core truth）

- 帧所有权（frame ownership）：分配、归属、释放
- 任务状态转换（task state transitions）：所有合法路径
- 地址空间映射（address-space mapping）
- 定时器（timer）
- IRQ（中断分配、mask、dispatch）
- handle 生命周期（handle lifetime）：创建、使用、过期
- 资源回收（resource revocation）：组件停止时 ResourceDomain 完整回收

### 对抗性测试（adversarial tests）—— 重点

> 核心目标：**即使 Component 是错的，Core invariant 仍然不能被破坏。**

- double free（重复释放）
- wrong owner（错误的所有者尝试操作资源）
- stale handle（过期 handle 使用）
- invalid task transition（非法任务状态转换）
- duplicate claim（重复声明资源）
- illegal map（非法映射）
- invalid scheduler proposal（无效调度提案，如调度不存在/非 Runnable/已在别的 CPU 的任务）

### 约束：没有 god-mode

- CoreTest **不得**拥有任意修改 Core 私有状态的"神权"；
- 正常测试只通过真实 Core API；
- 最多允许一个只读的 `TestInspector`（观察内部状态用于断言，不能写）。

> 理由：如果测试组件能直接改 Core 私有状态，那么"Core 不可破坏"这个结论就没被真正验证过。CoreTest 必须和任何其他组件一样受限。

## 4. Trace 与 Invariant

### 最小结构化 Trace（从早期就提供）

至少覆盖这些事件：

- task switch（任务切换）
- block / wake（阻塞 / 唤醒）
- resource grant / revoke（资源授予 / 回收）
- component lifecycle（组件生命周期变化）
- IRQ（中断事件）
- fault（故障）
- policy proposal（策略提案）
- Core rejection（Core 拒绝）

### Trace 的用途

- 调试：复现问题时的证据链；
- CoreTest：断言事件序列符合预期；
- 未来发展方向：Flight Recorder（飞行记录器）、Causal Debugger、Hunt Scheduler（确定性重放）、Fault Injection、Replay、Performance Attribution。

### Invariant 支持

Core 内提供 invariant check 机制：在关键路径断言不变式（如"同一帧最多一个 owner"、"Task 状态机合法"）。
第一阶段只需要最简单的 assert 级检查，配合 Trace 记录违规点。

## 5. 测试纪律

- 每个 Core 新功能：先写 host test，再写实现（至少同 PR 提交）；
- 对抗性测试与功能测试同等重要 —— Core 的"拒绝错误提案"行为必须显式测试，不能只测"正常路径能过"；
- Core 的 API 每多一个，就多一份必须验证的承诺 —— 这反过来约束 Core 词汇表保持最小。

## 6. 参考（详见 references.md）

- **CHESS**：确定性并发测试 —— 未来 Test Scheduler / Hunt Mode 的思路来源；
- **Kani / Loom / Miri / Verus**：模型检查 / 并发探索 / UB 检查 / 演绎验证 —— 未来工具链；
- **FSCQ**：文件系统验证与崩溃一致性 —— 对 Component contract 强验证的方法论参考。