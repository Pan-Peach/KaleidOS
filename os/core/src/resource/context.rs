//! Core-resolved authority principal for resource requests.
//!
//! # Principal rule
//!
//! A Core ABI request is attributed to the **innermost currently-active
//! Core-managed execution boundary** ([`containment::active_escape`]):
//!
//! - inside a component `kcomp_instance_create` (including a **nested** create)
//!   the principal is the component instance being created;
//! - inside a component task the principal is that task's owner;
//! - inside an IRQ callback scope (`containment::with_irq_scope`) the principal
//!   is the **IRQ line's owner** with no task — never the interrupted execution;
//! - after a nested create returns **or panics**, the containment guard stack
//!   restores the previous boundary (each guard stores the pointer it replaced),
//!   so the enclosing task/create principal is effective again.
//!
//! Only when no boundary is active (Core anchor / host tests) does resolution
//! fall back to the running task's owner and then the loader-recorded
//! `call_create` identity. This is the single source of identity for every
//! authority / task / interface entry point: no entry point may prefer one
//! source over the other.

use crate::component::ComponentId;
use crate::component::containment::{self, EscapeKind};
use crate::task::TaskId;

/// The component and optional task on whose behalf Core handles a request.
pub struct RequestContext {
    pub component: ComponentId,
    pub task: Option<TaskId>,
}

impl RequestContext {
    /// Resolves the ambient caller identity at the Component → Core ABI boundary.
    pub(crate) fn ambient() -> Option<Self> {
        // Innermost active boundary wins. `call_on_isolated_stack` /
        // `call_component_exit` install the init / exit guard over the task
        // guard and restore it on return/panic, so a nested create is
        // attributed to the nested component rather than to the task that
        // requested the create, and a stopping component's hook is attributed to
        // the instance being stopped.
        if let Some(escape) = containment::active_escape()
            && let Some(component) = escape.owner()
        {
            return Some(Self {
                component,
                task: escape.task(),
            });
        }
        // No boundary: fall back to the running task's owner, then the
        // loader-recorded `create` identity.
        let task = crate::sched::current_task().and_then(|id| {
            crate::task::get_task_table()
                .lock()
                .get(id)
                .map(|record| (id, record.owner()))
        });
        if let Some((task, component)) = task {
            return Some(Self {
                component,
                task: Some(task),
            });
        }
        crate::component::load::current_component().map(|component| Self {
            component,
            task: None,
        })
    }

    /// Like [`Self::ambient`] but restricted to an active `kcomp_instance_create`
    /// boundary.
    ///
    /// Interface publication is a create-time operation: a component task, a
    /// `kcomp_instance_destroy` hook, and an **IRQ callback scope** are active
    /// boundaries but are not valid publication principals.  In particular, an
    /// IRQ scope must not confer create-time publication permission even though
    /// it establishes a principal for resource requests.
    pub(crate) fn ambient_init() -> Option<Self> {
        match containment::active_escape()?.kind {
            EscapeKind::Init { owner } => owner.map(|component| Self {
                component,
                task: None,
            }),
            EscapeKind::Exit { .. } | EscapeKind::Task { .. } | EscapeKind::Irq { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plain running task resolves to its owner component.
    #[test]
    fn running_task_resolves_to_task_owner() {
        let _boundary = containment::test_boundary_lock();
        containment::enter_anchor();
        containment::enter_task(TaskId::from_raw(7), ComponentId::from_raw(3));

        let ctx = RequestContext::ambient().expect("task boundary active");
        assert_eq!(ctx.component, ComponentId::from_raw(3));
        assert_eq!(ctx.task, Some(TaskId::from_raw(7)));

        containment::enter_anchor();
    }

    /// A nested init boundary overrides the requesting task's owner, and the
    /// previous principal is restored once the nested init returns.
    #[test]
    fn nested_init_overrides_task_owner_and_restores() {
        let _boundary = containment::test_boundary_lock();
        containment::enter_anchor();
        containment::enter_task(TaskId::from_raw(7), ComponentId::from_raw(3));
        assert_eq!(
            RequestContext::ambient().unwrap().component,
            ComponentId::from_raw(3)
        );

        let nested = ComponentId::from_raw(9);
        containment::with_test_init_boundary(Some(nested), || {
            let ctx = RequestContext::ambient().expect("nested init boundary active");
            assert_eq!(ctx.component, nested, "nested init principal wins");
            assert_eq!(ctx.task, None, "init boundary carries no task");
        });

        let restored = RequestContext::ambient().expect("task boundary restored");
        assert_eq!(restored.component, ComponentId::from_raw(3));
        assert_eq!(restored.task, Some(TaskId::from_raw(7)));
        containment::enter_anchor();
    }

    /// A nested init that panics still restores the previous principal.
    #[test]
    fn nested_init_panic_restores_previous_principal() {
        let _boundary = containment::test_boundary_lock();
        containment::enter_anchor();
        containment::enter_task(TaskId::from_raw(7), ComponentId::from_raw(3));

        let nested = ComponentId::from_raw(9);
        containment::with_test_init_boundary(Some(nested), || {
            // The boot panic handler marks the active guard before escaping.
            containment::test_mark_active_panicked();
            assert_eq!(RequestContext::ambient().unwrap().component, nested);
        });

        let restored = RequestContext::ambient().expect("task boundary restored");
        assert_eq!(restored.component, ComponentId::from_raw(3));
        assert_eq!(restored.task, Some(TaskId::from_raw(7)));
        containment::enter_anchor();
    }

    /// Publish resolution rejects a plain task boundary (init-time operation).
    #[test]
    fn ambient_init_rejects_plain_task_boundary() {
        let _boundary = containment::test_boundary_lock();
        containment::enter_anchor();
        containment::enter_task(TaskId::from_raw(7), ComponentId::from_raw(3));

        assert!(RequestContext::ambient_init().is_none());
        assert_eq!(
            RequestContext::ambient().unwrap().component,
            ComponentId::from_raw(3)
        );

        containment::enter_anchor();
    }

    /// An IRQ callback scope resolves to the IRQ line's owner with `task = None`,
    /// overriding the interrupted task, and the task boundary is restored after.
    #[test]
    fn irq_scope_resolves_to_line_owner_and_restores_task_boundary() {
        let _boundary = containment::test_boundary_lock();
        containment::enter_anchor();
        containment::enter_task(TaskId::from_raw(7), ComponentId::from_raw(3));

        let irq_owner = ComponentId::from_raw(0xBEEF);
        containment::with_irq_scope(irq_owner, || {
            let ctx = RequestContext::ambient().expect("IRQ scope is a boundary");
            assert_eq!(ctx.component, irq_owner, "IRQ owner is the principal");
            assert_eq!(ctx.task, None, "an IRQ callback is not a task");
        });

        let restored = RequestContext::ambient().expect("interrupted task restored");
        assert_eq!(restored.component, ComponentId::from_raw(3));
        assert_eq!(restored.task, Some(TaskId::from_raw(7)));
        containment::enter_anchor();
    }

    /// Nested IRQ scopes resolve to the innermost line owner and restore in
    /// order (inner → outer → interrupted task).
    #[test]
    fn nested_irq_scopes_resolve_innermost_and_restore_in_order() {
        let _boundary = containment::test_boundary_lock();
        containment::enter_anchor();
        containment::enter_task(TaskId::from_raw(7), ComponentId::from_raw(3));

        let outer = ComponentId::from_raw(9);
        let inner = ComponentId::from_raw(11);
        containment::with_irq_scope(outer, || {
            assert_eq!(RequestContext::ambient().unwrap().component, outer);
            containment::with_irq_scope(inner, || {
                let ctx = RequestContext::ambient().unwrap();
                assert_eq!(ctx.component, inner, "innermost IRQ owner wins");
                assert_eq!(ctx.task, None);
            });
            assert_eq!(
                RequestContext::ambient().unwrap().component,
                outer,
                "inner scope restored the outer IRQ scope"
            );
        });

        let restored = RequestContext::ambient().unwrap();
        assert_eq!(restored.component, ComponentId::from_raw(3));
        assert_eq!(restored.task, Some(TaskId::from_raw(7)));
        containment::enter_anchor();
    }

    /// The IRQ scope is an attribution boundary, **not** a create boundary:
    /// `ambient_init()` stays unavailable inside it, and a surrounding create
    /// boundary is effective again once the IRQ scope returns.
    #[test]
    fn irq_scope_does_not_confer_create_publication() {
        let _boundary = containment::test_boundary_lock();
        containment::enter_anchor();

        let creator = ComponentId::from_raw(5);
        containment::with_test_init_boundary(Some(creator), || {
            assert_eq!(RequestContext::ambient_init().unwrap().component, creator);
            containment::with_irq_scope(ComponentId::from_raw(6), || {
                assert!(
                    RequestContext::ambient_init().is_none(),
                    "IRQ scope must not look like a create boundary"
                );
            });
            assert_eq!(
                RequestContext::ambient_init().unwrap().component,
                creator,
                "create boundary is effective again after the IRQ scope"
            );
        });

        containment::enter_anchor();
    }

    /// With no containment boundary active, no running task, and no
    /// loader-recorded `call_init` identity, the fallback chain is exhausted and
    /// `ambient()` reports no principal.
    #[test]
    fn ambient_without_boundary_task_or_loader_identity_is_none() {
        // Given：进程全局边界栈位于锚点，且本 CPU 未运行任务（sched::init 后
        // current == None）；不设置 load::CURRENT。
        let _boundary = containment::test_boundary_lock();
        crate::sched::init();
        crate::task::init();
        crate::component::registry::init();
        containment::enter_anchor();

        // When：在锚点上解析 ambient principal。
        let ambient = RequestContext::ambient();

        // Then：无边界、无任务、无 loader 身份时解析链耗尽 → None。
        // 其它测试共享这些进程全局量；仅在确认没有 transient 活跃身份时做
        // 确定性断言，否则退化为一致性断言（组件必须能在 registry 解析）。
        if crate::sched::current_task().is_none()
            && crate::component::load::current_component().is_none()
        {
            assert!(
                ambient.is_none(),
                "无边界/任务/loader 身份时必须解析为 None"
            );
        } else if let Some(ctx) = &ambient {
            assert!(
                crate::component::registry::get_registry()
                    .lock()
                    .get(ctx.component)
                    .is_some(),
                "ambient 组件必须在 registry 中可解析"
            );
        }

        containment::enter_anchor();
    }
}
