//! Build glue for the x86_64 boot image.
//!
//! The image must be a fixed-address `ET_EXEC`: `entry.S` links it at physical
//! 0x100000 and the boot protocol enters that address directly (no dynamic
//! loader, no relocation).  The Makefile sets `RUSTFLAGS`, which *replaces*
//! `target.*.rustflags` from `.cargo/config.toml`, so the `-no-pie` link
//! argument is emitted here where it applies to every build path.

fn main() {
    println!("cargo:rustc-link-arg=-no-pie");
}
