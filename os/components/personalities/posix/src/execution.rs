//! Minimal single-CPU process family. Core tasks carry the actual user AS/frame;
//! this instance owns PIDs, parent/zombie/wait state and the immutable image set.
use crate::{
    exec::{ImageLoader, check},
    image::Profile,
};
use alloc::{boxed::Box, vec, vec::Vec};
use core::{
    cell::RefCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};
use kcomp_sdk::{
    Errno, abi,
    console::Console,
    endpoint::{Endpoint, publish_ipc},
    generated::posix_wire as wire,
    ipc, management,
    posix::PosixProcess,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Running,
    Zombie(u32),
    Reaped,
}
struct Record {
    pid: u32,
    parent: u32,
    task: u32,
    state: State,
    waiting: bool,
}
pub struct Family {
    profile: Profile,
    records: RefCell<Vec<Record>>, // all mutation on the family's fixed CPU
    pub alive: AtomicBool,
    pub exited: AtomicBool,
    pub status: AtomicU32,
    pub live: AtomicU32,
}
#[derive(Clone)]
struct Process {
    family: *const Family,
    pid: u32,
    task: u32,
    heap_start: u64,
    heap: u64,
    heap_mapped: u64,
    closed: [bool; 3],
    clear_tid: u64,
}

fn random_seed() -> [u8; 16] {
    // Startup nonce only; no cryptographic random service exists in this profile.
    let now = unsafe { abi::kcore_now() };
    let mut result = [0; 16];
    result[..8].copy_from_slice(&now.to_le_bytes());
    result[8..].copy_from_slice(&now.rotate_left(29).to_le_bytes());
    result
}
fn create_task(
    process: &mut Process,
    image: &[u8],
    argv: &[Vec<u8>],
    envp: &[Vec<u8>],
) -> Result<u32, Errno> {
    let mut task = 0;
    check(unsafe { abi::kcore_user_create(run, process as *mut _ as *mut (), &mut task) })?;
    match ImageLoader.load(task, image, argv, envp, random_seed()) {
        Ok(heap) => {
            process.task = task;
            process.heap_start = heap;
            process.heap = heap;
            process.heap_mapped = heap;
            Ok(task)
        }
        Err(error) => {
            let _ = unsafe { abi::kcore_user_discard(task) };
            Err(error)
        }
    }
}

pub fn start(profile: Profile) -> Result<*mut Family, Errno> {
    let family = Box::new(Family {
        profile,
        records: RefCell::new(Vec::new()),
        alive: AtomicBool::new(true),
        exited: AtomicBool::new(false),
        status: AtomicU32::new(0),
        live: AtomicU32::new(1),
    });
    let mut process = Box::new(Process {
        family: &*family,
        pid: 1,
        task: 0,
        heap_start: 0,
        heap: 0,
        heap_mapped: 0,
        closed: [false; 3],
        clear_tid: 0,
    });
    let task = create_task(
        &mut process,
        &family.profile.images[0].bytes,
        &family.profile.argv,
        &family.profile.envp,
    )?;
    family.records.borrow_mut().push(Record {
        pid: 1,
        parent: 0,
        task,
        state: State::Running,
        waiting: false,
    });
    // No user code can run until both observer and initial Task are admitted.
    let result = publish_ipc::<PosixProcess>(kcomp_sdk::posix::KCOMP_POSIX_PROCESS_NAME);
    if let Err(error) = result {
        let _ = unsafe { abi::kcore_user_discard(task) };
        return Err(error);
    }
    let mut observer = 0;
    if let Err(error) = check(unsafe {
        abi::kcore_task_create(server, &*family as *const Family as *mut (), &mut observer)
    }) {
        let _ = unsafe { abi::kcore_user_discard(task) };
        return Err(error);
    }
    if let Err(error) = check(unsafe { abi::kcore_task_start(observer) })
        .and_then(|_| check(unsafe { abi::kcore_task_start(task) }))
    {
        let _ = unsafe { abi::kcore_user_discard(task) };
        // Both Tasks are fixed to this CPU; create never yields, so neither
        // can dereference the state before failed-create retirement.
        return Err(error);
    }
    let _ = Box::into_raw(process);
    Ok(Box::into_raw(family))
}

impl wire::Provider for Family {
    fn status(&self) -> Result<wire::StatusReply, Errno> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(Errno::ESRCH);
        }
        let exited = self.exited.load(Ordering::Acquire);
        Ok(wire::StatusReply {
            exited: u32::from(exited),
            wait_status: self.status.load(Ordering::Relaxed),
            live: self.live.load(Ordering::Acquire),
        })
    }
    fn shutdown(&self) -> Result<(), Errno> {
        if self.live.load(Ordering::Acquire) != 0 {
            return Err(Errno::EBUSY);
        }
        Ok(())
    }
}

extern "C" fn server(arg: *mut ()) {
    // State is instance-owned and remains resident after native logical stop.
    let family = unsafe { &*arg.cast::<Family>() };
    let owner = management::current_component().unwrap();
    let endpoint =
        Endpoint::<PosixProcess>::lookup(owner, kcomp_sdk::posix::KCOMP_POSIX_PROCESS_NAME)
            .unwrap()
            .id();
    ipc::listen(endpoint).unwrap();
    let mut request = [0; ipc::MESSAGE_MAX];
    let mut reply = [0; ipc::MESSAGE_MAX];
    loop {
        let (receipt, _, _, length) = match ipc::receive(endpoint, &mut request) {
            Ok(message) => message,
            Err(Errno::EAGAIN) => {
                ipc::wait_receive(endpoint).unwrap();
                continue;
            }
            Err(_) => break,
        };
        reply.fill(0);
        let (status, output, shutdown) = match ipc::service::Request::decode(&request[..length]) {
            Ok(message) => {
                let status = wire::dispatch(
                    family,
                    &message,
                    &mut reply
                        [ipc::service::REPLY_HEADER..ipc::service::REPLY_HEADER + message.output],
                );
                (
                    status,
                    message.output,
                    status == 0 && message.method == kcomp_sdk::posix::KCOMP_POSIX_METHOD_SHUTDOWN,
                )
            }
            Err(error) => (error.code(), 0, false),
        };
        // A late/canceled status query owns no new business resource.
        let _ = ipc::service::reply(
            receipt,
            status,
            &mut reply[..ipc::service::REPLY_HEADER + output],
        );
        if shutdown {
            family.alive.store(false, Ordering::Release);
            let _ = ipc::close(endpoint);
            break;
        }
    }
    management::exit_task();
}

impl Process {
    fn family(&self) -> &Family {
        unsafe { &*self.family }
    }
    fn read(&self, address: u64, buffer: &mut [u8]) -> Result<(), Errno> {
        check(unsafe {
            abi::kcore_user_read(self.task, address, buffer.as_mut_ptr(), buffer.len())
        })
    }
    fn write(&self, address: u64, buffer: &[u8]) -> Result<(), Errno> {
        check(unsafe { abi::kcore_user_write(self.task, address, buffer.as_ptr(), buffer.len()) })
    }
    fn cstring(&self, address: u64) -> Result<Vec<u8>, Errno> {
        let mut result = Vec::new();
        for offset in 0..4096 {
            let mut byte = [0];
            self.read(address.checked_add(offset).ok_or(Errno::EFAULT)?, &mut byte)?;
            if byte[0] == 0 {
                return Ok(result);
            }
            result.push(byte[0]);
        }
        Err(Errno::ENAMETOOLONG)
    }
    fn strings(&self, address: u64) -> Result<Vec<Vec<u8>>, Errno> {
        let mut result = Vec::new();
        if address == 0 {
            return Ok(result);
        }
        for index in 0..128 {
            let mut pointer = [0; 8];
            self.read(
                address.checked_add(index * 8).ok_or(Errno::EFAULT)?,
                &mut pointer,
            )?;
            let pointer = u64::from_le_bytes(pointer);
            if pointer == 0 {
                return Ok(result);
            }
            result.push(self.cstring(pointer)?);
        }
        Err(Errno::E2BIG)
    }
    fn fork(
        &mut self,
        flags: u64,
        stack: u64,
        parent_tid: u64,
        child_tid: u64,
    ) -> Result<i64, Errno> {
        // Linux RV64 exposes fork through clone. Shared-VM/thread/vfork profiles
        // are unsupported; copying backing gives a real independent process.
        const PARENT_SETTID: u64 = 0x100000;
        const CHILD_CLEARTID: u64 = 0x200000;
        const CHILD_SETTID: u64 = 0x1000000;
        if flags & 255 != 17
            || flags & !(255 | PARENT_SETTID | CHILD_CLEARTID | CHILD_SETTID) != 0
            || stack != 0
        {
            return Err(Errno::ENOTSUP);
        }
        let pid = {
            let records = self.family().records.borrow();
            if records.len() >= 128 {
                return Err(Errno::EAGAIN);
            }
            records.len() as u32 + 1
        };
        let mut child = Box::new(self.clone());
        child.pid = pid;
        child.clear_tid = if flags & CHILD_CLEARTID != 0 {
            child_tid
        } else {
            0
        };
        let mut task = 0;
        check(unsafe { abi::kcore_user_clone(run, &mut *child as *mut _ as *mut (), &mut task) })?;
        child.task = task;
        let setup = (|| {
            if flags & CHILD_SETTID != 0 {
                child.write(child_tid, &pid.to_le_bytes())?;
            }
            if flags & PARENT_SETTID != 0 {
                self.write(parent_tid, &pid.to_le_bytes())?;
            }
            Ok(())
        })();
        if let Err(error) = setup {
            let _ = unsafe { abi::kcore_user_discard(task) };
            return Err(error);
        }
        self.family().records.borrow_mut().push(Record {
            pid,
            parent: self.pid,
            task,
            state: State::Running,
            waiting: false,
        });
        if let Err(error) = check(unsafe { abi::kcore_task_start(task) }) {
            self.family().records.borrow_mut().pop();
            let _ = unsafe { abi::kcore_user_discard(task) };
            return Err(error);
        }
        self.family().live.fetch_add(1, Ordering::AcqRel);
        let _ = Box::into_raw(child);
        Ok(pid as i64)
    }
    fn exec(&mut self, path: u64, argv: u64, envp: u64) -> Result<i64, Errno> {
        let path = self.cstring(path)?;
        let argv = self.strings(argv)?;
        let envp = self.strings(envp)?;
        let image = self
            .family()
            .profile
            .images
            .iter()
            .find(|image| image.name == path)
            .ok_or(Errno::ENOENT)?;
        let mut staged = self.clone();
        let task = create_task(&mut staged, &image.bytes, &argv, &envp)?;
        if let Err(error) = check(unsafe { abi::kcore_user_replace(task) }) {
            let _ = unsafe { abi::kcore_user_discard(task) };
            return Err(error);
        }
        self.heap_start = staged.heap_start;
        self.heap = staged.heap;
        self.heap_mapped = staged.heap_mapped;
        self.clear_tid = 0;
        Ok(0)
    }
    fn wait(&self, requested: i64, status: u64, options: u64, rusage: u64) -> Result<i64, Errno> {
        if (requested != -1 && requested <= 0) || options & !1 != 0 || rusage != 0 {
            return Err(Errno::ENOTSUP);
        }
        loop {
            let candidate = {
                let records = self.family().records.borrow();
                let children: Vec<_> = records
                    .iter()
                    .filter(|record| {
                        record.parent == self.pid
                            && record.state != State::Reaped
                            && (requested == -1 || record.pid == requested as u32)
                    })
                    .collect();
                if children.is_empty() {
                    return Err(Errno::ECHILD);
                }
                children.iter().find_map(|child| match child.state {
                    State::Zombie(code) => Some((child.pid, code)),
                    _ => None,
                })
            };
            if let Some((pid, code)) = candidate {
                if status != 0 {
                    self.write(status, &code.to_le_bytes())?;
                }
                self.family()
                    .records
                    .borrow_mut()
                    .iter_mut()
                    .find(|r| r.pid == pid)
                    .unwrap()
                    .state = State::Reaped;
                return Ok(pid as i64);
            }
            if options & 1 != 0 {
                return Ok(0);
            }
            self.family()
                .records
                .borrow_mut()
                .iter_mut()
                .find(|r| r.pid == self.pid)
                .unwrap()
                .waiting = true;
            let result = check(unsafe { abi::kcore_task_park() });
            self.family()
                .records
                .borrow_mut()
                .iter_mut()
                .find(|r| r.pid == self.pid)
                .unwrap()
                .waiting = false;
            result?;
        }
    }
    fn finish(&self, code: u32) {
        if self.clear_tid != 0 {
            let _ = self.write(self.clear_tid, &0u32.to_le_bytes());
        }
        let wake = {
            let mut records = self.family().records.borrow_mut();
            let record = records.iter_mut().find(|r| r.pid == self.pid).unwrap();
            record.state = State::Zombie(code);
            let parent = record.parent;
            records
                .iter()
                .find(|r| r.pid == parent && r.waiting)
                .map(|r| r.task)
        };
        if let Some(task) = wake {
            let _ = unsafe { abi::kcore_task_unpark(task) };
        }
        self.family().live.fetch_sub(1, Ordering::AcqRel);
        if self.pid == 1 {
            self.family().status.store(code, Ordering::Relaxed);
            self.family().exited.store(true, Ordering::Release);
        }
        kcomp_sdk::klog!(
            "posix: pid={} task={} wait_status={}",
            self.pid,
            self.task,
            code
        );
    }
    fn syscall(&mut self, event: &abi::UserTrap) -> Result<i64, Errno> {
        let a = [
            event.arg0, event.arg1, event.arg2, event.arg3, event.arg4, event.arg5,
        ];
        match event.number {
            64 => {
                if a[0] != 1 && a[0] != 2 || self.closed[a[0].min(2) as usize] {
                    return Err(Errno::EBADF);
                }
                let len = usize::try_from(a[2]).map_err(|_| Errno::EINVAL)?;
                if len > 1024 * 1024 {
                    return Err(Errno::EINVAL);
                }
                if len == 0 {
                    return Ok(0);
                }
                let mut bytes = vec![0; len];
                self.read(a[1], &mut bytes)?;
                Console::write(&bytes);
                Ok(len as i64)
            }
            63 => {
                if a[0] != 0 || self.closed[0] {
                    return Err(Errno::EBADF);
                }
                Ok(0) // Profile stdin is EOF.
            }
            57 => {
                if a[0] >= 3 || self.closed[a[0] as usize] {
                    return Err(Errno::EBADF);
                }
                self.closed[a[0] as usize] = true;
                Ok(0)
            }
            96 => {
                self.clear_tid = a[0];
                Ok(self.pid as i64)
            }
            124 => {
                management::yield_task()?;
                Ok(0)
            }
            172 | 178 => Ok(self.pid as i64),
            173 => Ok(self
                .family()
                .records
                .borrow()
                .iter()
                .find(|r| r.pid == self.pid)
                .unwrap()
                .parent as i64),
            174..=177 => Ok(0), // profile credentials: uid/gid/euid/egid 0
            214 => {
                if a[0] == 0 {
                    return Ok(self.heap as i64);
                }
                if a[0] < self.heap_start || a[0] >= 0x1800_0000 {
                    return Ok(self.heap as i64);
                }
                let end = (a[0] + 4095) & !4095;
                if end > self.heap_mapped {
                    if unsafe {
                        abi::kcore_user_map(self.task, self.heap_mapped, end - self.heap_mapped, 3)
                    } != 0
                    {
                        return Ok(self.heap as i64);
                    }
                    self.heap_mapped = end;
                }
                self.heap = a[0];
                Ok(self.heap as i64)
            }
            226 => {
                if a[1] == 0 {
                    return Ok(0);
                }
                let len = a[1].checked_add(4095).ok_or(Errno::EINVAL)? & !4095;
                check(unsafe {
                    abi::kcore_user_protect(
                        self.task,
                        a[0],
                        len,
                        u32::try_from(a[2]).map_err(|_| Errno::EINVAL)?,
                    )
                })?;
                Ok(0)
            }
            220 => self.fork(a[0], a[1], a[2], a[4]), // RISC-V clone: flags,stack,ptid,tls,ctid
            221 => self.exec(a[0], a[1], a[2]),
            260 => self.wait(a[0] as i64, a[1], a[2], a[3]),
            _ => {
                kcomp_sdk::klog!(
                    "posix: pid={} unsupported syscall={}",
                    self.pid,
                    event.number
                );
                Err(Errno::ENOSYS)
            }
        }
    }
}
extern "C" fn run(arg: *mut ()) {
    let mut process = unsafe { Box::from_raw(arg.cast::<Process>()) };
    let mut result = 0;
    loop {
        let mut event = MaybeUninit::uninit();
        let hz = unsafe { abi::kcore_timebase_hz() };
        if hz < 100 {
            process.finish(127 << 8);
            break;
        }
        let deadline = unsafe { abi::kcore_now() }.saturating_add(hz / 100);
        let code = unsafe { abi::kcore_user_step(result, deadline, event.as_mut_ptr()) };
        if code != 0 {
            process.finish(127 << 8);
            break;
        }
        let event = unsafe { event.assume_init() };
        assert_eq!(event.task, process.task, "Core user trap source changed");
        if event.cause >> 63 != 0 {
            let _ = management::yield_task();
            continue;
        }
        if event.cause != 8 {
            kcomp_sdk::klog!(
                "posix: pid={} fault scause={} pc={:#x} address={:#x}",
                process.pid,
                event.cause,
                event.pc,
                event.address
            );
            process.finish(match event.cause {
                2 => 4,
                3 => 5,
                0 | 4 | 6 => 7,
                _ => 11,
            });
            break;
        }
        if event.number == 93 || event.number == 94 {
            process.finish(((event.arg0 as u32) & 255) << 8);
            break;
        }
        result = match process.syscall(&event) {
            Ok(value) => value,
            Err(error) => error.code() as i64,
        };
    }
    drop(process);
    management::exit_task();
}
