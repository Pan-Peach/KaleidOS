//! Core-resolved authority principal for resource requests.

use crate::component::ComponentId;
use crate::task::TaskId;

/// The component and optional task on whose behalf Core handles a request.
pub struct RequestContext {
    pub component: ComponentId,
    pub task: Option<TaskId>,
}

impl RequestContext {
    /// Resolves the ambient caller identity at the Component → Core ABI boundary.
    pub(crate) fn ambient() -> Option<Self> {
        let task = crate::sched::current_task();
        let component = task
            .and_then(|id| {
                crate::task::get_task_table()
                    .lock()
                    .get(id)
                    .map(|record| record.owner())
            })
            .or_else(crate::component::load::current_component)?;

        Some(Self { component, task })
    }
}
