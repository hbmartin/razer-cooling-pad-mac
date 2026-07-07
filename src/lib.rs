//! Control library for the Razer Laptop Cooling Pad (USB 1532:0F43):
//! protocol framing, device I/O, the fan-curve engine, lighting plans, and
//! the host-side helpers behind the `padctl` CLI.
//!
//! The binary in `main.rs` is a thin CLI layer over these modules; exposing
//! them as a library also lets the property tests (`tests/properties.rs`)
//! and fuzz targets (`fuzz/`) exercise the parsing surfaces directly.

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!("padctl supports macOS (its primary target) and Linux (protocol work, CI) only");

pub mod config;
pub mod curve;
pub mod device;
pub mod doctor;
pub mod fan;
pub mod lighting;
pub mod logging;
pub mod packet;
pub mod parse;
pub mod power;
pub mod reactive;
pub mod rgb;
pub mod service;
pub mod temp;
