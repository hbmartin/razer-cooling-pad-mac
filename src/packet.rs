//! Razer HID protocol framing for the Laptop Cooling Pad (1532:0F43).
//!
//! Every command is a 91-byte HID feature report: a 0x00 report-ID byte
//! followed by a 90-byte packet. Layout of the packet `P`:
//!
//! | bytes     | meaning                                            |
//! |-----------|----------------------------------------------------|
//! | P[0]      | status (0x00 on send; see [`Status`] on responses) |
//! | P[1]      | transaction id                                     |
//! | P[2..=3]  | remaining packets (always 0 here)                  |
//! | P[4]      | protocol type (0)                                  |
//! | P[5]      | data size                                          |
//! | P[6]      | command class                                      |
//! | P[7]      | command id                                         |
//! | P[8..=87] | arguments (80 bytes)                               |
//! | P[88]     | crc = XOR of P[2..=87]                             |
//! | P[89]     | 0x00                                               |
//!
//! Byte layouts are replicated exactly from two known-good implementations:
//! a FanControl Windows plugin for this pad (fan, transaction id 0x02; see
//! <https://github.com/Rem0o/FanControl.Releases>) and openrazer's accessory
//! driver (RGB, transaction id 0x1F;
//! <https://github.com/openrazer/openrazer>, driver/razerchromacommon.c).

pub const PACKET_LEN: usize = 90;
pub const REPORT_LEN: usize = 91;
pub const ARGS_OFFSET: usize = 8;
pub const CRC_OFFSET: usize = 88;

/// Transaction id used by the FanControl Windows plugin for fan commands.
pub const TID_FAN: u8 = 0x02;
/// Transaction id used by openrazer for this device's RGB/info commands.
pub const TID_RGB: u8 = 0x1F;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    New,          // 0x00
    Busy,         // 0x01
    Ok,           // 0x02
    Failure,      // 0x03
    Timeout,      // 0x04
    NotSupported, // 0x05
    Unknown(u8),
}

impl From<u8> for Status {
    fn from(b: u8) -> Self {
        match b {
            0x00 => Status::New,
            0x01 => Status::Busy,
            0x02 => Status::Ok,
            0x03 => Status::Failure,
            0x04 => Status::Timeout,
            0x05 => Status::NotSupported,
            other => Status::Unknown(other),
        }
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Status::New => write!(f, "new (0x00)"),
            Status::Busy => write!(f, "busy (0x01)"),
            Status::Ok => write!(f, "ok (0x02)"),
            Status::Failure => write!(f, "failure (0x03)"),
            Status::Timeout => write!(f, "timeout (0x04)"),
            Status::NotSupported => write!(f, "not supported (0x05)"),
            Status::Unknown(b) => write!(f, "unknown (0x{b:02x})"),
        }
    }
}

/// A 90-byte Razer command packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    bytes: [u8; PACKET_LEN],
}

impl Packet {
    pub fn new(transaction_id: u8, class: u8, cmd: u8, data_size: u8, args: &[u8]) -> Self {
        assert!(args.len() <= 80, "args must fit in 80 bytes");
        let mut bytes = [0u8; PACKET_LEN];
        bytes[1] = transaction_id;
        bytes[5] = data_size;
        bytes[6] = class;
        bytes[7] = cmd;
        bytes[ARGS_OFFSET..ARGS_OFFSET + args.len()].copy_from_slice(args);
        Self { bytes }
    }

    pub fn crc(&self) -> u8 {
        crc(&self.bytes)
    }

    /// The full 91-byte feature report: report id 0x00 + packet with crc filled in.
    pub fn to_report(&self) -> [u8; REPORT_LEN] {
        let mut report = [0u8; REPORT_LEN];
        report[1..].copy_from_slice(&self.bytes);
        report[1 + CRC_OFFSET] = self.crc();
        report
    }
}

/// Razer XOR checksum: XOR of packet bytes 2..=87 (transaction id excluded).
pub fn crc(packet: &[u8]) -> u8 {
    packet[2..=87].iter().fold(0, |acc, b| acc ^ b)
}

/// A parsed 90-byte response packet.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: Status,
    #[allow(dead_code)] // kept for verbose/debug inspection
    pub transaction_id: u8,
    #[allow(dead_code)]
    pub data_size: u8,
    pub class: u8,
    pub cmd: u8,
    pub args: [u8; 80],
}

impl Response {
    /// Parse from a full 91-byte feature report (leading report-ID byte).
    pub fn from_report(report: &[u8; REPORT_LEN]) -> Self {
        let p = &report[1..];
        let mut args = [0u8; 80];
        args.copy_from_slice(&p[ARGS_OFFSET..ARGS_OFFSET + 80]);
        Self {
            status: Status::from(p[0]),
            transaction_id: p[1],
            data_size: p[5],
            class: p[6],
            cmd: p[7],
            args,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fan;
    use crate::rgb;

    /// `Header90` from the known-good FanControl Windows plugin,
    /// with the fan-set command bytes for 2700 RPM
    /// and its crc filled in exactly as `SetCurveRpm(2700)` produces.
    fn windows_plugin_report_2700rpm() -> [u8; REPORT_LEN] {
        let mut buf = [0u8; REPORT_LEN];
        // buf[0] = report id 0x00
        let header: [u8; 12] = [
            0x00, 0x02, 0x00, 0x00, 0x00, 0x03, 0x0D, 0x10, 0x01, 0x02, 0x36, 0x00,
        ];
        buf[1..=12].copy_from_slice(&header);
        // SetCurveRpm overrides: REPORT_CODE=0x01, SUB_VER=0x01, CURVE_ID=0x05, rpm=2700 -> raw 0x36
        buf[8] = 0x01;
        buf[9] = 0x01;
        buf[10] = 0x05;
        buf[11] = 0x36;
        buf[12] = 0x00;
        // CHK_L = RPM_L ^ 0x0B
        buf[89] = 0x36 ^ 0x0B;
        buf[90] = 0x00;
        buf
    }

    #[test]
    fn fan_set_2700_matches_windows_plugin_byte_for_byte() {
        let report = fan::set_rpm(2700).to_report();
        assert_eq!(report, windows_plugin_report_2700rpm());
        assert_eq!(report[89], 0x3D);
    }

    #[test]
    fn fan_off_crc_matches_windows_plugin_constant() {
        // windows.cs hardcodes CHK_L = 0x18 for the off packet.
        let report = fan::off().to_report();
        assert_eq!(report[89], 0x18);
        assert_eq!(
            &report[1..=12],
            &[
                0x00, 0x02, 0x00, 0x00, 0x00, 0x03, 0x0D, 0x10, 0x00, 0x06, 0x00, 0x00
            ]
        );
    }

    #[test]
    fn fan_set_clamps_and_rounds() {
        // 100 RPM clamps to 500 (raw 10); 9999 clamps to 3200 (raw 64).
        assert_eq!(fan::set_rpm(100).to_report()[11], 10);
        assert_eq!(fan::set_rpm(9999).to_report()[11], 64);
        // 1525 rounds to nearest 50 -> raw 31 (1550).
        assert_eq!(fan::set_rpm(1525).to_report()[11], 31);
    }

    #[test]
    fn rgb_static_red_layout_and_crc() {
        let report = rgb::static_color(0xFF, 0x00, 0x00).to_report();
        // tid 0x1F, class 0x0F, cmd 0x02, ds 0x09, args 01 00 01 00 00 01 R G B
        assert_eq!(report[2], 0x1F);
        assert_eq!(report[6], 0x09);
        assert_eq!(report[7], 0x0F);
        assert_eq!(report[8], 0x02);
        assert_eq!(
            &report[9..=17],
            &[0x01, 0x00, 0x01, 0x00, 0x00, 0x01, 0xFF, 0x00, 0x00]
        );
        assert_eq!(report[89], 0xFA);
    }

    #[test]
    fn rgb_wave_speed_byte() {
        // args: varstore, led, effect 4, direction, speed
        let report = rgb::wave(rgb::WaveDirection::Left, 0x28).to_report();
        assert_eq!(&report[9..=13], &[0x01, 0x00, 0x04, 0x01, 0x28]);
        let report = rgb::wave(rgb::WaveDirection::Right, 0x10).to_report();
        assert_eq!(&report[9..=13], &[0x01, 0x00, 0x04, 0x02, 0x10]);
    }

    #[test]
    fn rgb_custom_frame_layout() {
        // openrazer razer_chroma_extended_matrix_set_custom_frame2 with
        // dynamic packet length: class 0x0f, cmd 0x03, ds = 3*len + 5,
        // args [0, 0, row, start, stop, rgb...].
        let report = rgb::custom_frame(2, &[(1, 2, 3), (4, 5, 6)]).to_report();
        assert_eq!(report[2], 0x1F); // transaction id
        assert_eq!(report[6], 11); // data size = 2*3 + 5
        assert_eq!(report[7], 0x0F);
        assert_eq!(report[8], 0x03);
        assert_eq!(&report[9..=13], &[0x00, 0x00, 0x00, 2, 3]); // row 0, cols 2-3
        assert_eq!(&report[14..=19], &[1, 2, 3, 4, 5, 6]);

        // A full 18-LED frame: ds = 59, stop col 17.
        let full = vec![(0xAA, 0xBB, 0xCC); rgb::NUM_LEDS];
        let report = rgb::custom_frame(0, &full).to_report();
        assert_eq!(report[6], 59);
        assert_eq!(&report[9..=13], &[0x00, 0x00, 0x00, 0, 17]);
    }

    #[test]
    fn rgb_custom_apply_layout() {
        // openrazer razer_chroma_extended_matrix_effect_custom_frame:
        // NOSTORE, ZERO_LED, effect 0x08, ds 0x0C.
        let report = rgb::custom_apply().to_report();
        assert_eq!(report[2], 0x1F);
        assert_eq!(report[6], 0x0C);
        assert_eq!(report[7], 0x0F);
        assert_eq!(report[8], 0x02);
        assert_eq!(&report[9..=11], &[0x00, 0x00, 0x08]);
    }

    #[test]
    fn device_mode_layout() {
        // openrazer razer_chroma_standard_set_device_mode: class 0x00,
        // cmd 0x04, ds 0x02, args [mode, 0x00].
        let report = rgb::device_mode(0x03).to_report();
        assert_eq!(report[6], 0x02);
        assert_eq!(report[7], 0x00);
        assert_eq!(report[8], 0x04);
        assert_eq!(&report[9..=10], &[0x03, 0x00]);
    }

    #[test]
    fn response_roundtrip() {
        let mut report = [0u8; REPORT_LEN];
        report[1] = 0x02; // status ok
        report[2] = 0x1F;
        report[6] = 0x03;
        report[7] = 0x0F;
        report[8] = 0x84;
        report[9] = 0x01;
        report[11] = 0xFF;
        let r = Response::from_report(&report);
        assert_eq!(r.status, Status::Ok);
        assert_eq!(r.transaction_id, 0x1F);
        assert_eq!(r.class, 0x0F);
        assert_eq!(r.cmd, 0x84);
        assert_eq!(r.args[0], 0x01);
        assert_eq!(r.args[2], 0xFF);
    }
}
