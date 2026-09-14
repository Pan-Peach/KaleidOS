use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const RV32_TARGET: &str = "riscv32imac-unknown-none-elf";
const RV64_TARGET: &str = "riscv64gc-unknown-none-elf";

fn main() {
    let target = if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("riscv32") {
        RV32_TARGET
    } else {
        RV64_TARGET
    };
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let repo = manifest_dir.parent().unwrap().parent().unwrap();
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let target_dir = out.join("component-target");

    // 组件 → .kcomp 的单一构建管线（与 Makefile 共用 tools/build-kcomp.sh）：
    // staticlib → rust-lld -r --gc-sections -u kcomp_init → strip。host 测试
    // fixture（<name>.kcomp / init.kpkg / smoke_min.kcomp）保持不变。
    let script = repo.join("tools/build-kcomp.sh");
    println!("cargo:rerun-if-changed={}", script.display());
    let sdk_dir = repo.join("os/components/kcomp-sdk");
    for changed in [
        sdk_dir.join("Cargo.toml"),
        // SDK 已拆成多模块：跟踪整个 src/ 目录，任一源文件变化都触发重建。
        sdk_dir.join("src"),
        repo.join("tools/build-kcomp.sh"),
    ] {
        println!("cargo:rerun-if-changed={}", changed.display());
    }

    let components = ["core_test", "kcomp_smoke", "kcomp_min"];
    let mut objects = Vec::new();
    for name in components {
        let component_dir = repo.join("os/components").join(name);
        println!(
            "cargo:rerun-if-changed={}",
            component_dir.join("Cargo.toml").display()
        );
        println!(
            "cargo:rerun-if-changed={}",
            component_dir.join("src/lib.rs").display()
        );
        let destination = out.join(format!("{name}.kcomp"));
        run_kcomp_build(&script, &component_dir, target, &destination, &target_dir);
        objects.push((name, fs::read(&destination).unwrap()));
    }

    write_newc(
        &out.join("init.kpkg"),
        &[
            ("manifest", b"kcomp_smoke.kcomp\n".to_vec()),
            ("kcomp_smoke.kcomp", objects[1].1.clone()),
        ],
    );
    fs::copy(out.join("kcomp_min.kcomp"), out.join("smoke_min.kcomp")).unwrap();
}

/// 调用共享构建脚本（与 Makefile 同一条管线），失败即 panic。
fn run_kcomp_build(
    script: &Path,
    component_dir: &Path,
    target: &str,
    destination: &Path,
    target_dir: &Path,
) {
    let status = Command::new(script)
        .args([
            component_dir.to_str().unwrap(),
            target,
            destination.to_str().unwrap(),
            target_dir.to_str().unwrap(),
        ])
        .status()
        .unwrap_or_else(|error| {
            panic!(
                "failed to run {} for {}: {error}",
                script.display(),
                component_dir.display()
            )
        });
    assert!(
        status.success(),
        "kcomp build failed for {}",
        component_dir.display()
    );
}

fn write_newc(path: &Path, files: &[(&str, Vec<u8>)]) {
    let mut archive = Vec::new();
    for (name, data) in files {
        append_newc_entry(&mut archive, name, data);
    }
    append_newc_entry(&mut archive, "TRAILER!!!", &[]);
    fs::write(path, archive).unwrap();
}

fn append_newc_entry(archive: &mut Vec<u8>, name: &str, data: &[u8]) {
    let name_size = name.len() + 1;
    let header = format!(
        "070701{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}",
        0,
        0o100644,
        0,
        0,
        1,
        0,
        data.len(),
        0,
        0,
        0,
        0,
        name_size,
        0
    );
    assert_eq!(header.len(), 110);
    archive.extend_from_slice(header.as_bytes());
    archive.extend_from_slice(name.as_bytes());
    archive.push(0);
    pad4(archive);
    archive.extend_from_slice(data);
    pad4(archive);
}

fn pad4(buffer: &mut Vec<u8>) {
    while !buffer.len().is_multiple_of(4) {
        buffer.push(0);
    }
}
