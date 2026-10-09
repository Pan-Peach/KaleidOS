//! 命令只走 SDK 值查询 / 服务前端；没有 Core 指针或域特例。
use crate::parser::{self, Command, ParseError};
use core::ffi::CStr;
use core::fmt::Write;
use kcomp_sdk::console::Console;
use kcomp_sdk::endpoint::{Contract, Endpoint};
use kcomp_sdk::filesystem::{FILESYSTEM_OPEN_READ, FileSystem};
use kcomp_sdk::{abi, management};

fn text(bytes: &[u8]) -> &str {
    core::str::from_utf8(bytes).unwrap_or("<non-UTF8>")
}
fn state(raw: u32) -> &'static str {
    match raw {
        0 => "Declared",
        1 => "Resolved",
        2 => "Starting",
        3 => "Ready",
        4 => "Stopping",
        5 => "Stopped",
        6 => "Failed",
        _ => "Unknown",
    }
}
fn domain(raw: u32) -> &'static str {
    match raw {
        0 => "KernelNative",
        1 => "IsolatedNative",
        2 => "SandboxedNative",
        _ => "Unknown",
    }
}

/// 确定性单行执行入口；返回 true = 退出会话。
/// exec reads a real FS image and composes a POSIX process family.
pub fn execute(
    line: &[u8],
    input: &crate::input::Input,
    decoded: &mut [u8; parser::LINE_MAX],
) -> bool {
    let mut out = Console;
    let command = match parser::parse(line, decoded) {
        Ok(None) => return false,
        Ok(Some(command)) => command,
        Err(error) => {
            match error {
                ParseError::Usage(usage) => {
                    let _ = writeln!(out, "usage: {usage}");
                }
                ParseError::Unknown(name) => {
                    let _ = writeln!(out, "unknown command '{}' (try help)", text(name));
                }
                ParseError::UnsupportedDomain => {
                    let _ = writeln!(
                        out,
                        "load: supported domains: native, isolated; sandboxed is unavailable"
                    );
                }
                ParseError::UnclosedQuote => {
                    let _ = writeln!(out, "input rejected: unclosed quote");
                }
                ParseError::TrailingEscape => {
                    let _ = writeln!(out, "input rejected: trailing backslash");
                }
                ParseError::UnsupportedSyntax => {
                    let _ = writeln!(
                        out,
                        "input rejected: pipelines, redirects and command lists are unavailable"
                    );
                }
                other => {
                    let _ = writeln!(out, "input rejected: {other:?}");
                }
            }
            return false;
        }
    };
    match command {
        Command::Help(topic) => help(topic),
        Command::Echo(words) => {
            for (index, word) in words.iter().enumerate() {
                if index > 0 {
                    Console::write(b" ");
                }
                Console::write(word);
            }
            Console::write(b"\n");
        }
        Command::Clear => Console::write(b"\x1b[2J\x1b[H"),
        Command::History => {
            for (index, line) in input.history().enumerate() {
                let _ = writeln!(out, "{:>2}  {}", index + 1, text(line));
            }
        }
        Command::Components => components(None),
        Command::Inspect(name) => components(Some(name)),
        Command::Endpoints => endpoints(),
        Command::Devices => devices(),
        Command::Load(name, domain) => match management::load(name, domain) {
            Ok(id) => {
                let _ = writeln!(out, "load {}: OK (id={id})", text(name));
            }
            Err(error) => {
                let _ = writeln!(out, "load {}: {error}", text(name));
            }
        },
        Command::Cat(path) => cat(path),
        Command::Exec(words) => {
            let argv: alloc::vec::Vec<_> = words.iter().collect();
            if let Err(error) = exec(&argv) {
                let _ = writeln!(out, "exec: {error}");
            }
        }
        Command::Unsupported(name) => {
            let _ = writeln!(
                out,
                "{}: unsupported by current filesystem service",
                text(name)
            );
        }
        Command::Exit => {
            let _ = writeln!(out, "ksh: exit");
            return true;
        }
    }
    false
}

fn help(topic: Option<&[u8]>) {
    let mut out = Console;
    if let Some(topic) = topic {
        let Some(&(_, usage)) = parser::COMMANDS.iter().find(|&&(name, _)| name == topic) else {
            let _ = writeln!(out, "help: unknown command '{}'", text(topic));
            return;
        };
        let _ = writeln!(out, "{usage}");
        let detail = match topic {
            b"cat" => "Read from the selected filesystem. Quote paths containing spaces.",
            b"exec" => {
                "Static RV64 ELF; image snapshot, stdin EOF, stdout/stderr console. Quotes preserve each argument, including empty arguments."
            }
            b"load" => {
                "Defaults to native; .kcomp suffix is optional. Core validates the requested domain."
            }
            b"inspect" => {
                "Show all loaded instances of an artifact; does not inspect stored files."
            }
            b"history" => {
                "Last 8 submitted nonempty lines, including this command; consecutive duplicates are omitted. Up/Down recalls lines and restores the draft."
            }
            b"echo" => {
                "Print arguments separated by one space. Single/double quotes preserve spaces; backslash escapes outside single quotes. No variable expansion."
            }
            b"exit" => "End this shell session. Ctrl-D on an empty line also exits.",
            b"devices" => {
                "Show primary compatible, address/IRQ, and current component owner. Firmware descriptors include empty virtio transports; Unclaimed does not mean absent."
            }
            _ => "",
        };
        if !detail.is_empty() {
            let _ = writeln!(out, "{detail}");
        }
        return;
    }
    for &(_, usage) in parser::COMMANDS {
        let _ = writeln!(out, "  {usage}");
    }
    let _ = writeln!(
        out,
        "\nhelp <command> for details. Input: max {} bytes, 16 arguments.\nUp/Down or Ctrl-P/N: history | Left/Right: cursor | Home/End or Ctrl-A/E\nBackspace/Delete: erase | Ctrl-U/K: erase before/after cursor | Ctrl-W: erase word\nTab: command completion | Ctrl-L: clear/redraw | Ctrl-C: cancel | Ctrl-D: delete/exit\nQEMU -nographic: press Ctrl-A twice to send Ctrl-A.\nSingle/double quotes, backslash escapes, # comments; no expansion, pipes or redirects.",
        parser::LINE_MAX
    );
}

fn components(only: Option<&[u8]>) {
    let mut out = Console;
    let mut name = [0u8; 256];
    let mut found = false;
    if only.is_none() {
        let _ = writeln!(out, "ID   NAME                 DOMAIN           STATE");
    }
    for ordinal in 0..u32::MAX {
        match management::component_nth(ordinal, &mut name) {
            Ok(Some(info)) => {
                let name = &name[..info.name_len as usize];
                if only.is_some_and(|wanted| wanted != name) {
                    continue;
                }
                found = true;
                if only.is_some() {
                    let _ = writeln!(
                        out,
                        "Artifact: {}.kcomp\nFormat:   KCOMP (loaded component)\nInstance: {}\nDomain:   {}\nState:    {}",
                        text(name),
                        info.id,
                        domain(info.domain),
                        state(info.state)
                    );
                } else {
                    let _ = writeln!(
                        out,
                        "{:<4} {:<20} {:<16} {}",
                        info.id,
                        text(name),
                        domain(info.domain),
                        state(info.state)
                    );
                }
            }
            Ok(None) => break,
            Err(error) => {
                let _ = writeln!(out, "components: {error}");
                return;
            }
        }
    }
    if only.is_some() && !found {
        let _ = writeln!(
            out,
            "inspect: no loaded instance; stored-artifact byte inspection is unavailable"
        );
    }
}

fn endpoints() {
    let mut out = Console;
    let mut name = [0u8; 256];
    let _ = writeln!(
        out,
        "ID   PROVIDER PORT NAME                 CONTRACT           STATE"
    );
    for ordinal in 0..u32::MAX {
        match management::endpoint_nth(ordinal, &mut name) {
            Ok(Some(info)) => {
                let state = match info.state {
                    0 => "Pending",
                    1 => "Live",
                    2 => "Invalid",
                    _ => "Unknown",
                };
                let _ = writeln!(
                    out,
                    "{:<4} {:<8} {:<4} {:<20} {:#018x} {}",
                    info.id,
                    info.provider,
                    info.port,
                    text(&name[..info.name_len as usize]),
                    info.contract,
                    state
                );
            }
            Ok(None) => break,
            Err(error) => {
                let _ = writeln!(out, "endpoints: {error}");
                break;
            }
        }
    }
}

fn devices() {
    use management::{DeviceClaimState, DeviceSpaceKind};
    let mut out = Console;
    let mut compatible = [0; 256];
    let mut owner_name = [0; 256];
    let _ = writeln!(
        out,
        "ID  DEVICE (PRIMARY COMPATIBLE)     STATE        OWNER"
    );
    let mut count = 0;
    for ordinal in 0..u32::MAX {
        let id = match management::device_nth(ordinal) {
            Ok(Some(id)) => id,
            Ok(None) => break,
            Err(error) => {
                let _ = writeln!(out, "devices: {error}");
                return;
            }
        };
        count += 1;
        let info = match management::device_info(id, &mut compatible) {
            Ok(info) => info,
            Err(error) => {
                let _ = writeln!(out, "{id}: device query failed: {error}");
                continue;
            }
        };
        let label = if info.compatible_count == 0 {
            "<unspecified>"
        } else {
            text(&compatible[..info.compatible_len as usize])
        };
        let state = match info.state {
            n if n == DeviceClaimState::Claimed as u32 => "Claimed",
            n if n == DeviceClaimState::Quarantined as u32 => "Quarantined",
            _ => "Unclaimed",
        };
        let _ = write!(out, "{id:<3} {label:<30} {state:<12} ");
        if info.state == DeviceClaimState::Claimed as u32 {
            // Resolve a copied owner identity through the existing component
            // query. Never infer a driver from a DeviceId or compatible string.
            let mut found = false;
            for ordinal in 0..u32::MAX {
                match management::component_nth(ordinal, &mut owner_name) {
                    Ok(Some(owner)) if u64::from(owner.id) == info.owner => {
                        let _ = writeln!(
                            out,
                            "{}#{}",
                            text(&owner_name[..owner.name_len as usize]),
                            owner.id
                        );
                        found = true;
                        break;
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(error) => {
                        let _ = writeln!(out, "#{} (name query: {error})", info.owner);
                        found = true;
                        break;
                    }
                }
            }
            if !found {
                let _ = writeln!(out, "#{}", info.owner);
            }
        } else {
            let _ = writeln!(out, "-");
        }
        match info.space_kind {
            n if n == DeviceSpaceKind::None as u32 => {
                let _ = write!(out, "    primary window: -");
            }
            kind => {
                let kind = if kind == DeviceSpaceKind::Mmio as u32 {
                    "MMIO"
                } else {
                    "PIO"
                };
                let _ = write!(out, "    primary {kind}: {:#x}+{:#x}", info.base, info.size);
            }
        }
        if info.interrupt_count == 0 {
            let _ = writeln!(out, "  IRQ: -");
        } else if info.irq_known != 0 {
            let _ = writeln!(out, "  IRQ[0]: {}", info.irq_line);
        } else {
            let _ = writeln!(out, "  IRQ[0]: unmapped");
        }
        if info.compatible_count > 1 || info.space_count > 1 || info.interrupt_count > 1 {
            let _ = writeln!(
                out,
                "    totals: {} compatible(s), {} window(s), {} IRQ resource(s)",
                info.compatible_count, info.space_count, info.interrupt_count
            );
        }
    }
    let _ = writeln!(out, "{count} device record(s)");
}

fn filesystem_endpoint() -> kcomp_sdk::Result<Option<u64>> {
    let configured = crate::runtime::filesystem();
    if configured != 0 {
        return Ok(Some(configured));
    }
    // Monitor-created standalone shell retains a deliberately conservative fallback.
    let mut selected = None;
    let mut name = [0; 256];
    for ordinal in 0..u32::MAX {
        let Some(info) = management::endpoint_nth(ordinal, &mut name)? else {
            break;
        };
        if info.state == abi::EndpointState::Live as u32
            && info.contract == FileSystem::ID
            && info.abi == FileSystem::ABI
            && selected.replace(info.id).is_some()
        {
            return Err(kcomp_sdk::Errno::EBUSY);
        }
    }
    Ok(selected)
}

fn cat(path: &[u8]) {
    let mut out = Console;
    let selected = match filesystem_endpoint() {
        Ok(id) => id,
        Err(error) => {
            if error == kcomp_sdk::Errno::EBUSY {
                let _ = writeln!(
                    out,
                    "cat: multiple filesystem providers; selection is unavailable"
                );
            } else {
                let _ = writeln!(out, "cat: {error}");
            }
            return;
        }
    };
    let Some(id) = selected else {
        let _ = writeln!(out, "cat: no filesystem provider");
        return;
    };
    let endpoint = match Endpoint::<FileSystem>::from_id(id) {
        Ok(endpoint) => endpoint,
        Err(error) => {
            let _ = writeln!(out, "cat: endpoint failed: {error}");
            return;
        }
    };
    let binding = match endpoint.bind() {
        Ok(binding) => binding,
        Err(error) => {
            let _ = writeln!(out, "cat: bind failed: {error:?}");
            return;
        }
    };
    if let Err(error) = binding.mount() {
        let _ = writeln!(out, "cat: mount failed: {error:?}");
        return;
    }
    let mut bytes = [0u8; kcomp_sdk::generated::filesystem::KCOMP_FILESYSTEM_PATH_MAX];
    if path.len() >= bytes.len() {
        let _ = writeln!(out, "cat: path too long");
        return;
    }
    bytes[..path.len()].copy_from_slice(path);
    let Ok(path) = CStr::from_bytes_with_nul(&bytes[..path.len() + 1]) else {
        let _ = writeln!(out, "cat: invalid path");
        return;
    };
    let handle = match binding.open(path, FILESYSTEM_OPEN_READ) {
        Ok(handle) => handle,
        Err(error) => {
            let _ = writeln!(out, "cat: open failed: {error:?}");
            return;
        }
    };
    let mut buffer = [0u8; 520]; // Existing FS wire: 8-byte length header + data.
    loop {
        match binding.read(handle, &mut buffer) {
            Ok(0) => break,
            Ok(len) => Console::write(&buffer[..len]),
            Err(error) => {
                let _ = writeln!(out, "cat: read failed: {error:?}");
                break;
            }
        }
        if let Err(error) = management::yield_task() {
            let _ = writeln!(out, "cat: yield failed: {error}");
            break;
        }
    }
    if let Err(error) = binding.close(handle) {
        let _ = writeln!(out, "cat: close failed: {error:?}");
    }
    Console::write(b"\n");
}

// This is shell composition: read an executable through the actual mounted FS,
// then give POSIX an immutable image snapshot. It is not a VFS namespace.
fn exec(argv: &[&[u8]]) -> kcomp_sdk::Result<()> {
    use alloc::vec::Vec;
    use kcomp_sdk::{Errno, posix};
    let selected = filesystem_endpoint()?;
    let binding = Endpoint::<FileSystem>::from_id(selected.ok_or(Errno::ENODEV)?)?
        .bind()
        .map_err(fs_error)?;
    binding.mount().map_err(fs_error)?;
    let path = argv[0];
    let mut bytes = [0u8; kcomp_sdk::generated::filesystem::KCOMP_FILESYSTEM_PATH_MAX];
    if path.len() >= bytes.len() {
        return Err(Errno::ENAMETOOLONG);
    }
    bytes[..path.len()].copy_from_slice(path);
    let path = CStr::from_bytes_with_nul(&bytes[..path.len() + 1]).map_err(|_| Errno::EINVAL)?;
    let handle = binding.open(path, FILESYSTEM_OPEN_READ).map_err(fs_error)?;
    let loaded = (|| {
        let mut image = Vec::new();
        let mut buffer = [0u8; 520];
        loop {
            let len = binding.read(handle, &mut buffer).map_err(fs_error)?;
            if len == 0 {
                return Ok(image);
            }
            if image.len() + len > 1024 * 1024 {
                return Err(Errno::E2BIG);
            }
            image.extend_from_slice(&buffer[..len]);
            management::yield_task()?;
        }
    })();
    let closed = binding.close(handle).map_err(fs_error);
    let image = loaded?;
    closed?;
    let config = posix::encode(&[(b"/main", &image)], argv, &[])?;
    let id = management::create(
        b"posix",
        management::ExecutionDomain::KernelNative,
        posix::KCOMP_POSIX_CREATE_CONFIG_ABI,
        &config,
    )?;
    let process =
        Endpoint::<posix::PosixProcess>::lookup(id, posix::KCOMP_POSIX_PROCESS_NAME)?.bind()?;
    loop {
        let status = process.status()?;
        if status.exited && status.live == 0 {
            let mut out = Console;
            if status.wait_status & 127 != 0 {
                let _ = writeln!(out, "exec: signal={}", status.wait_status & 127);
            } else {
                let _ = writeln!(out, "exec: exit={}", (status.wait_status >> 8) & 255);
            }
            return Ok(());
        }
        management::yield_task()?;
    }
}

fn fs_error(error: kcomp_sdk::endpoint::InvokeError) -> kcomp_sdk::Errno {
    use kcomp_sdk::endpoint::InvokeError;
    match error {
        InvokeError::Transport(errno) | InvokeError::Method(errno) => errno,
        InvokeError::InvalidReply => kcomp_sdk::Errno::EIO,
    }
}
