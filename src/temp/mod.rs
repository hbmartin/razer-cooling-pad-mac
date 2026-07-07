//! CPU temperature reading.
//!
//! macOS (primary target): SMC temperature sensors via the private
//! `IOHIDEventSystemClient` API, falling back to coarse
//! `NSProcessInfo.thermalState` estimates.
//!
//! Linux (used for protocol work and CI): sysfs thermal zones.

#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
use mac as imp;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as imp;

use anyhow::Result;

/// A single named temperature sensor reading, for `padctl sensors`.
pub struct Reading {
    pub name: String,
    pub celsius: f64,
}

pub struct TempReader {
    inner: imp::Reader,
}

impl TempReader {
    pub fn new() -> Result<Self> {
        Ok(TempReader {
            inner: imp::Reader::new()?,
        })
    }

    /// Current CPU temperature estimate in °C.
    pub fn read(&self) -> Result<f64> {
        self.inner.read()
    }

    /// Human-readable name of the active temperature source.
    pub fn source_name(&self) -> &'static str {
        self.inner.source_name()
    }

    /// Whether the active source is a coarse fallback (e.g. thermal-pressure
    /// estimates) rather than real sensors. Surfaced by `padctl doctor`.
    pub fn is_fallback(&self) -> bool {
        self.inner.is_fallback()
    }

    /// All individual sensors the active source can see.
    pub fn sensors(&self) -> Result<Vec<Reading>> {
        self.inner.sensors()
    }
}
