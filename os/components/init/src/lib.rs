//! Minimal boot composition policy. Core owns each component's lifecycle.
#![no_std]

#[cfg(test)]
extern crate std;

#[cfg_attr(not(target_os = "none"), allow(dead_code))]
mod root;
#[cfg(target_os = "none")]
mod runtime;
