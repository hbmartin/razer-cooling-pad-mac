//! RGB lighting commands, replicated from openrazer's accessory driver for
//! this device (reference/linux cooling pad.diff -> razerchromacommon.c):
//! extended-matrix effects, class 0x0F, transaction id 0x1F, VARSTORE,
//! ZERO_LED. The pad is a 1x18 LED strip.

use crate::packet::{Packet, TID_RGB};

const CLASS_EXTENDED_MATRIX: u8 = 0x0F;
const CMD_EFFECT: u8 = 0x02;
const CMD_BRIGHTNESS_SET: u8 = 0x04;
const CMD_BRIGHTNESS_GET: u8 = 0x84;

const VARSTORE: u8 = 0x01;
const ZERO_LED: u8 = 0x00;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum WaveDirection {
    Left,
    Right,
}

pub fn off() -> Packet {
    Packet::new(
        TID_RGB,
        CLASS_EXTENDED_MATRIX,
        CMD_EFFECT,
        0x06,
        &[VARSTORE, ZERO_LED, 0x00],
    )
}

pub fn static_color(r: u8, g: u8, b: u8) -> Packet {
    Packet::new(
        TID_RGB,
        CLASS_EXTENDED_MATRIX,
        CMD_EFFECT,
        0x09,
        &[VARSTORE, ZERO_LED, 0x01, 0x00, 0x00, 0x01, r, g, b],
    )
}

pub fn spectrum() -> Packet {
    Packet::new(
        TID_RGB,
        CLASS_EXTENDED_MATRIX,
        CMD_EFFECT,
        0x06,
        &[VARSTORE, ZERO_LED, 0x03],
    )
}

pub fn wave(direction: WaveDirection) -> Packet {
    let dir = match direction {
        WaveDirection::Left => 0x01,
        WaveDirection::Right => 0x02,
    };
    Packet::new(
        TID_RGB,
        CLASS_EXTENDED_MATRIX,
        CMD_EFFECT,
        0x06,
        &[VARSTORE, ZERO_LED, 0x04, dir, 0x28],
    )
}

pub fn breath_random() -> Packet {
    Packet::new(
        TID_RGB,
        CLASS_EXTENDED_MATRIX,
        CMD_EFFECT,
        0x06,
        &[VARSTORE, ZERO_LED, 0x02],
    )
}

pub fn breath_single(r: u8, g: u8, b: u8) -> Packet {
    Packet::new(
        TID_RGB,
        CLASS_EXTENDED_MATRIX,
        CMD_EFFECT,
        0x09,
        &[VARSTORE, ZERO_LED, 0x02, 0x01, 0x00, 0x01, r, g, b],
    )
}

pub fn breath_dual(c1: (u8, u8, u8), c2: (u8, u8, u8)) -> Packet {
    Packet::new(
        TID_RGB,
        CLASS_EXTENDED_MATRIX,
        CMD_EFFECT,
        0x0C,
        &[
            VARSTORE, ZERO_LED, 0x02, 0x02, 0x00, 0x02, c1.0, c1.1, c1.2, c2.0, c2.1, c2.2,
        ],
    )
}

/// brightness: 0-255 (CLI maps percent onto this).
pub fn brightness_set(brightness: u8) -> Packet {
    Packet::new(
        TID_RGB,
        CLASS_EXTENDED_MATRIX,
        CMD_BRIGHTNESS_SET,
        0x03,
        &[VARSTORE, ZERO_LED, brightness],
    )
}

pub fn brightness_get() -> Packet {
    Packet::new(
        TID_RGB,
        CLASS_EXTENDED_MATRIX,
        CMD_BRIGHTNESS_GET,
        0x03,
        &[VARSTORE, ZERO_LED],
    )
}

/// Firmware version query: class 0x00, cmd 0x81 (openrazer standard).
pub fn firmware_version() -> Packet {
    Packet::new(TID_RGB, 0x00, 0x81, 0x02, &[])
}

/// Serial number query: class 0x00, cmd 0x82; response args are 22 ASCII bytes.
pub fn serial() -> Packet {
    Packet::new(TID_RGB, 0x00, 0x82, 0x16, &[])
}
