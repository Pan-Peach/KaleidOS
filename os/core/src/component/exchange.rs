//! Bounded copied Request/Reply truth. No provider code or caller pointers here.
//! Lock order: registry -> endpoints -> exchange -> task table. Wake after unlock.
use super::{ComponentId, endpoint::EndpointId};
use crate::{errno::Errno, task::TaskId};
use alloc::vec::Vec;
use spin::{Mutex, Once};

pub const MESSAGE_MAX: usize = crate::generated::abi::KCORE_IPC_MESSAGE_MAX as usize;
const SLOTS: usize = 16;
const ENDPOINT_LIMIT: usize = 32;
const GRANT_LIMIT: usize = 16;
type Wakes = [Option<TaskId>; ENDPOINT_LIMIT + SLOTS];
type Result<T> = core::result::Result<T, Errno>;

struct Server {
    endpoint: EndpointId,
    owner: ComponentId,
    task: TaskId,
    waiting: bool,
    consumers: [Option<ComponentId>; GRANT_LIMIT],
}
struct Slot {
    id: u64,
    endpoint: EndpointId,
    caller: TaskId,
    consumer: ComponentId,
    accepted: bool,
    receipt: bool,
    caller_live: bool,
    waiting: bool,
    terminal: Option<i32>,
    data: Vec<u8>,
    len: usize,
}
impl Slot {
    fn new() -> Self {
        Self {
            id: 0,
            endpoint: EndpointId::from_raw(0),
            caller: TaskId::from_raw(0),
            consumer: ComponentId::from_raw(0),
            accepted: false,
            receipt: false,
            caller_live: false,
            waiting: false,
            terminal: None,
            data: alloc::vec![0; MESSAGE_MAX],
            len: 0,
        }
    }
    fn finish(&mut self, status: i32) -> Option<TaskId> {
        if self.terminal.is_some() {
            return None;
        }
        self.terminal = Some(status);
        let wake = (self.waiting && self.caller_live).then_some(self.caller);
        self.waiting = false;
        wake
    }
    fn retire(&mut self) {
        if !self.caller_live && !self.receipt {
            self.id = 0;
        }
    }
}

pub struct Exchange {
    servers: Vec<Server>,
    slots: Vec<Slot>,
    next: u64,
}
impl Default for Exchange {
    fn default() -> Self {
        Self::new()
    }
}
impl Exchange {
    pub fn new() -> Self {
        Self {
            servers: Vec::with_capacity(ENDPOINT_LIMIT),
            slots: (0..SLOTS).map(|_| Slot::new()).collect(),
            next: 1,
        }
    }
    fn server(&self, endpoint: EndpointId) -> Result<&Server> {
        self.servers
            .iter()
            .find(|s| s.endpoint == endpoint)
            .ok_or(Errno::ENOTCONN)
    }
    fn slot(&self, id: u64) -> Result<usize> {
        if id == 0 {
            return Err(Errno::EINVAL);
        }
        self.slots
            .iter()
            .position(|s| s.id == id)
            .ok_or(Errno::ENOENT)
    }
    pub fn listen(&mut self, owner: ComponentId, task: TaskId, endpoint: EndpointId) -> Result<()> {
        if self.servers.iter().any(|s| s.endpoint == endpoint) {
            return Err(Errno::EEXIST);
        }
        if self.servers.len() == ENDPOINT_LIMIT {
            return Err(Errno::ENOSPC);
        }
        self.servers.push(Server {
            endpoint,
            owner,
            task,
            waiting: false,
            consumers: [None; GRANT_LIMIT],
        });
        Ok(())
    }
    pub fn grant(
        &mut self,
        owner: ComponentId,
        endpoint: EndpointId,
        consumer: ComponentId,
    ) -> Result<()> {
        let server = self
            .servers
            .iter_mut()
            .find(|s| s.endpoint == endpoint)
            .ok_or(Errno::ENOTCONN)?;
        if server.owner != owner {
            return Err(Errno::EACCES);
        }
        if server.consumers.contains(&Some(consumer)) {
            return Ok(());
        }
        let grant = server
            .consumers
            .iter_mut()
            .find(|c| c.is_none())
            .ok_or(Errno::ENOSPC)?;
        *grant = Some(consumer);
        Ok(())
    }
    pub fn submit(
        &mut self,
        consumer: ComponentId,
        caller: TaskId,
        endpoint: EndpointId,
        input: &[u8],
    ) -> Result<(u64, Option<TaskId>)> {
        let server = self.server(endpoint)?;
        if !server.consumers.contains(&Some(consumer)) {
            return Err(Errno::EACCES);
        }
        if input.len() > MESSAGE_MAX {
            return Err(Errno::EMSGSIZE);
        }
        if self
            .slots
            .iter()
            .any(|s| s.id != 0 && s.caller == caller && s.caller_live)
        {
            return Err(Errno::EBUSY);
        }
        let mut task = server.task;
        for _ in 0..=SLOTS {
            if task == caller {
                return Err(Errno::EDEADLK);
            }
            let Some(pending) = self
                .slots
                .iter()
                .find(|s| s.id != 0 && s.caller == task && s.terminal.is_none())
            else {
                break;
            };
            task = self.server(pending.endpoint)?.task;
        }
        if self
            .slots
            .iter()
            .filter(|s| s.id != 0 && s.endpoint == endpoint)
            .count()
            >= 4
        {
            return Err(Errno::ENOBUFS);
        }
        let index = self
            .slots
            .iter()
            .position(|s| s.id == 0)
            .ok_or(Errno::ENOBUFS)?;
        let next = self.next.checked_add(1).ok_or(Errno::ENOSPC)?;
        let id = self.next;
        self.next = next;
        let slot = &mut self.slots[index];
        slot.id = id;
        slot.endpoint = endpoint;
        slot.caller = caller;
        slot.consumer = consumer;
        slot.accepted = false;
        slot.receipt = false;
        slot.caller_live = true;
        slot.waiting = false;
        slot.terminal = None;
        slot.len = input.len();
        slot.data[..input.len()].copy_from_slice(input);
        let server = self
            .servers
            .iter_mut()
            .find(|s| s.endpoint == endpoint)
            .unwrap();
        let wake = server.waiting.then_some(server.task);
        server.waiting = false;
        Ok((id, wake))
    }
    pub fn receive(
        &mut self,
        task: TaskId,
        endpoint: EndpointId,
        output: &mut [u8],
    ) -> Result<(u64, ComponentId, TaskId, usize)> {
        if self.server(endpoint)?.task != task {
            return Err(Errno::EACCES);
        }
        let slot = self
            .slots
            .iter_mut()
            .filter(|s| s.id != 0 && s.endpoint == endpoint && !s.accepted && s.terminal.is_none())
            .min_by_key(|s| s.id)
            .ok_or(Errno::EAGAIN)?;
        if output.len() < slot.len {
            return Err(Errno::EMSGSIZE);
        }
        output[..slot.len].copy_from_slice(&slot.data[..slot.len]);
        slot.accepted = true;
        slot.receipt = true;
        Ok((slot.id, slot.consumer, slot.caller, slot.len))
    }
    pub fn reply(&mut self, task: TaskId, id: u64, input: &[u8]) -> Result<Option<TaskId>> {
        let index = self.slot(id)?;
        if self.server(self.slots[index].endpoint)?.task != task {
            return Err(Errno::EACCES);
        }
        let slot = &mut self.slots[index];
        if !slot.receipt {
            return Err(Errno::EINVAL);
        }
        if input.len() > MESSAGE_MAX {
            return Err(Errno::EMSGSIZE);
        }
        let discarded = slot.terminal.is_some();
        let wake = if !discarded {
            slot.data[..input.len()].copy_from_slice(input);
            slot.len = input.len();
            slot.finish(0)
        } else {
            None
        };
        slot.receipt = false;
        slot.retire();
        if discarded {
            Err(Errno::ECANCELED)
        } else {
            Ok(wake)
        }
    }
    pub fn collect(&mut self, caller: TaskId, id: u64, output: &mut [u8]) -> Result<(i32, usize)> {
        let index = self.slot(id)?;
        let slot = &mut self.slots[index];
        if !slot.caller_live || slot.caller != caller {
            return Err(Errno::EACCES);
        }
        let status = slot.terminal.ok_or(Errno::EAGAIN)?;
        let len = if status == 0 { slot.len } else { 0 };
        if output.len() < len {
            return Err(Errno::EMSGSIZE);
        }
        output[..len].copy_from_slice(&slot.data[..len]);
        slot.caller_live = false;
        slot.waiting = false;
        slot.retire();
        Ok((status, len))
    }
    pub fn cancel(&mut self, caller: TaskId, id: u64) -> Result<Option<TaskId>> {
        let index = self.slot(id)?;
        let slot = &mut self.slots[index];
        if !slot.caller_live || slot.caller != caller {
            return Err(Errno::EACCES);
        }
        if slot.terminal.is_some() {
            return Err(Errno::EALREADY);
        }
        Ok(slot.finish(Errno::ECANCELED.code()))
    }
    /// Returns true only after registering a waiter under the predicate lock.
    pub fn wait(&mut self, task: TaskId, endpoint: EndpointId, id: u64) -> Result<bool> {
        if id != 0 {
            let index = self.slot(id)?;
            let slot = &mut self.slots[index];
            if slot.caller != task || !slot.caller_live {
                return Err(Errno::EACCES);
            }
            slot.waiting = slot.terminal.is_none();
            Ok(slot.waiting)
        } else {
            if self.server(endpoint)?.task != task {
                return Err(Errno::EACCES);
            }
            let pending = self.slots.iter().any(|s| {
                s.id != 0 && s.endpoint == endpoint && !s.accepted && s.terminal.is_none()
            });
            let server = self
                .servers
                .iter_mut()
                .find(|s| s.endpoint == endpoint)
                .unwrap();
            server.waiting = !pending;
            Ok(!pending)
        }
    }
    fn close_into(&mut self, endpoint: EndpointId, wake: &mut Wakes) {
        self.servers.retain(|s| {
            if s.endpoint != endpoint {
                return true;
            }
            if s.waiting {
                *wake.iter_mut().find(|t| t.is_none()).unwrap() = Some(s.task);
            }
            false
        });
        for slot in &mut self.slots {
            if slot.id == 0 || slot.endpoint != endpoint {
                continue;
            }
            if let Some(task) = slot.finish(Errno::ENOTCONN.code()) {
                *wake.iter_mut().find(|t| t.is_none()).unwrap() = Some(task);
            }
            slot.receipt = false;
            slot.retire();
        }
    }
    pub fn close(&mut self, endpoint: EndpointId) -> Wakes {
        let mut wakes = [None; ENDPOINT_LIMIT + SLOTS];
        self.close_into(endpoint, &mut wakes);
        wakes
    }
    fn exit(&mut self, task: TaskId) -> ([Option<EndpointId>; ENDPOINT_LIMIT], Wakes) {
        let mut endpoints = [None; ENDPOINT_LIMIT];
        for (slot, server) in endpoints
            .iter_mut()
            .zip(self.servers.iter().filter(|s| s.task == task))
        {
            *slot = Some(server.endpoint);
        }
        let mut wakes = [None; ENDPOINT_LIMIT + SLOTS];
        for endpoint in endpoints.iter().flatten() {
            self.close_into(*endpoint, &mut wakes);
        }
        for slot in &mut self.slots {
            if slot.id == 0 || slot.caller != task {
                continue;
            }
            slot.waiting = false;
            slot.caller_live = false;
            slot.finish(Errno::ECANCELED.code());
            slot.retire();
        }
        (endpoints, wakes)
    }
}

static EXCHANGE: Once<Mutex<Exchange>> = Once::new();
pub fn init() {
    EXCHANGE.call_once(|| Mutex::new(Exchange::new()));
}
pub(crate) fn get() -> &'static Mutex<Exchange> {
    init();
    EXCHANGE.get().unwrap()
}
pub(crate) fn wake(tasks: impl IntoIterator<Item = TaskId>) {
    for task in tasks {
        let owner = crate::task::get_task_table()
            .lock()
            .get(task)
            .map(|r| r.owner());
        if let Some(owner) = owner {
            let _ = crate::sched::unpark_task(owner, task);
        }
    }
}
pub(crate) fn task_exited(task: TaskId) {
    let Some(exchange) = EXCHANGE.get() else {
        return;
    };
    let _irq = crate::irq::IrqSaveGuard::new();
    let mut endpoints = super::endpoint::get_endpoints().lock();
    let (closed, wakes) = exchange.lock().exit(task);
    for endpoint in closed.into_iter().flatten() {
        endpoints.invalidate(endpoint);
    }
    drop(endpoints);
    wake(wakes.into_iter().flatten());
}
pub(crate) fn owner_failed(owner: ComponentId) {
    let Some(exchange) = EXCHANGE.get() else {
        return;
    };
    let _irq = crate::irq::IrqSaveGuard::new();
    let wakes = {
        let mut state = exchange.lock();
        let mut wakes = [None; ENDPOINT_LIMIT + SLOTS];
        while let Some(endpoint) = state
            .servers
            .iter()
            .find(|s| s.owner == owner)
            .map(|s| s.endpoint)
        {
            state.close_into(endpoint, &mut wakes);
        }
        // Consumer failure loses its outstanding result even on another server.
        for slot in &mut state.slots {
            if slot.id != 0 && slot.consumer == owner {
                slot.caller_live = false;
                slot.waiting = false;
                slot.finish(Errno::ECANCELED.code());
                slot.retire();
            }
        }
        for server in &mut state.servers {
            for grant in &mut server.consumers {
                if *grant == Some(owner) {
                    *grant = None;
                }
            }
        }
        wakes
    };
    wake(wakes.into_iter().flatten());
}

#[cfg(test)]
mod tests;
