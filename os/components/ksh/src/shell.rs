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
/// 未来 exec(path, argv) 在此分派到 ExecService，当前没有应用执行。
pub fn execute(line: &[u8]) -> bool {
    let mut out = Console;
    let command = match parser::parse(line) {
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
                other => {
                    let _ = writeln!(out, "input rejected: {other:?}");
                }
            }
            return false;
        }
    };
    match command {
        Command::Help => {
            let _ = writeln!(
                out,
                "help | echo <args...> | clear | components | endpoints | devices\nload <artifact> [native|isolated] | inspect <loaded-artifact>\ncat <provider-relative-path> | exit\nls/cd/pwd: unavailable (no directory/namespace service)\nApplications: ExecService is not implemented"
            );
        }
        Command::Echo(words) => {
            for (index, word) in words
                .split(|b| b.is_ascii_whitespace())
                .filter(|s| !s.is_empty())
                .enumerate()
            {
                if index > 0 {
                    Console::write(b" ");
                }
                Console::write(word);
            }
            Console::write(b"\n");
        }
        Command::Clear => Console::write(b"\x1b[2J\x1b[H"),
        Command::Components => components(None),
        Command::Inspect(name) => components(Some(name)),
        Command::Endpoints => endpoints(),
        Command::Devices => {
            let _ = writeln!(out, "DEVICE ID");
            for ordinal in 0..u32::MAX {
                match management::device_nth(ordinal) {
                    Ok(Some(id)) => {
                        let _ = writeln!(out, "{id}");
                    }
                    Ok(None) => break,
                    Err(error) => {
                        let _ = writeln!(out, "devices: {error}");
                        break;
                    }
                }
            }
        }
        Command::Load(name, domain) => match management::load(name, domain) {
            Ok(id) => {
                let _ = writeln!(out, "load {}: OK (id={id})", text(name));
            }
            Err(error) => {
                let _ = writeln!(out, "load {}: {error}", text(name));
            }
        },
        Command::Cat(path) => cat(path),
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

fn cat(path: &[u8]) {
    let mut out = Console;
    // Minimal composition policy: use the sole live matching filesystem endpoint.
    // Never silently choose between multiple volumes; namespace/selection is deferred.
    let mut selected = None;
    let mut name = [0u8; 256];
    for ordinal in 0..u32::MAX {
        match management::endpoint_nth(ordinal, &mut name) {
            Ok(Some(info))
                if info.state == abi::EndpointState::Live as u32
                    && info.contract == FileSystem::ID
                    && info.abi == FileSystem::ABI =>
            {
                if selected.is_some() {
                    let _ = writeln!(
                        out,
                        "cat: multiple filesystem providers; selection is unavailable"
                    );
                    return;
                }
                selected = Some(info.id);
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(error) => {
                let _ = writeln!(out, "cat: {error}");
                return;
            }
        }
    }
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
            Ok(len) => Console::write(&buffer[8..8 + len]),
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
