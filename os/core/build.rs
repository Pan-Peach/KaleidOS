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
        run_component_build(&component_dir, &target_dir, target);
        let object = find_object(&target_dir, name, target)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let destination = out.join(format!("{name}.kcomp"));
        fs::copy(object, &destination).unwrap();
        objects.push((name, fs::read(destination).unwrap()));
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

fn run_component_build(component_dir: &Path, target_dir: &Path, target: &str) {
    let status = Command::new("cargo")
        .current_dir(component_dir)
        .args([
            "rustc",
            "--release",
            "--target",
            target,
            "--target-dir",
            target_dir.to_str().unwrap(),
            "--",
            "--emit=obj",
        ])
        .status()
        .unwrap_or_else(|error| {
            panic!(
                "failed to run cargo for {}: {error}",
                component_dir.display()
            )
        });
    assert!(
        status.success(),
        "cargo failed for {}",
        component_dir.display()
    );
}

fn find_object(target_dir: &Path, name: &str, target: &str) -> Result<PathBuf, String> {
    let deps = target_dir.join(target).join("release").join("deps");
    let entries =
        fs::read_dir(&deps).map_err(|error| format!("read {}: {error}", deps.display()))?;
    for entry in entries {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path.extension().is_some_and(|extension| extension == "o")
            && path
                .file_name()
                .is_some_and(|file| file.to_string_lossy().starts_with(&format!("{name}-")))
        {
            return Ok(path);
        }
    }
    Err(format!("object not found in {}", deps.display()))
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
