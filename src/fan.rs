//! Fan commands, byte-for-byte identical to the known-good FanControl
//! Windows plugin (reference/windows.cs): class 0x0D, transaction id 0x02,
//! speed encoded as RPM/50.

use crate::packet::{Packet, REPORT_LEN, TID_FAN};

pub const MIN_RPM: u32 = 500;
pub const MAX_RPM: u32 = 3200;
pub const RPM_STEP: u32 = 50;

const CLASS_FAN: u8 = 0x0D;

/// Clamp to the device's supported range and round to the 50 RPM step.
pub fn normalize_rpm(rpm: u32) -> u32 {
    let clamped = rpm.clamp(MIN_RPM, MAX_RPM);
    ((clamped + RPM_STEP / 2) / RPM_STEP) * RPM_STEP
}

/// Map 0-100% onto the supported RPM range.
pub fn percent_to_rpm(pct: u32) -> u32 {
    normalize_rpm(MIN_RPM + (MAX_RPM - MIN_RPM) * pct.min(100) / 100)
}

pub fn set_rpm(rpm: u32) -> Packet {
    let raw = (normalize_rpm(rpm) / RPM_STEP) as u16;
    Packet::new(
        TID_FAN,
        CLASS_FAN,
        0x01,
        0x03,
        &[0x01, 0x05, (raw & 0xFF) as u8, (raw >> 8) as u8],
    )
}

pub fn off() -> Packet {
    Packet::new(TID_FAN, CLASS_FAN, 0x10, 0x03, &[0x00, 0x06, 0x00, 0x00])
}

/// Extract the current RPM from a feature report read (leading report-ID
/// byte included), as the Windows plugin does: bytes 11-12 little-endian,
/// times 50.
pub fn rpm_from_report(report: &[u8; REPORT_LEN]) -> u32 {
    (report[11] as u32 | (report[12] as u32) << 8) * 50
}
