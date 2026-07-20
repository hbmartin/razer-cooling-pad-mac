//! RGB lighting commands, replicated from openrazer's accessory driver
//! (<https://github.com/openrazer/openrazer>, driver/razerchromacommon.c):
//! extended-matrix effects, class 0x0F, transaction id 0x1F, VARSTORE,
//! ZERO_LED. The pad is a 1x18 LED strip.

use crate::packet::{Packet, TID_RGB};

pub const NUM_LEDS: usize = 18;

const CLASS_EXTENDED_MATRIX: u8 = 0x0F;
const CMD_EFFECT: u8 = 0x02;
const CMD_SET_CUSTOM_FRAME: u8 = 0x03;
const CMD_BRIGHTNESS_SET: u8 = 0x04;
const CMD_BRIGHTNESS_GET: u8 = 0x84;

const VARSTORE: u8 = 0x01;
const NOSTORE: u8 = 0x00;
const ZERO_LED: u8 = 0x00;
const EFFECT_CUSTOM_FRAME: u8 = 0x08;

/// Default wave speed byte used by openrazer for accessories.
pub const DEFAULT_WAVE_SPEED: u8 = 0x28;

/// An RGB color triple.
pub type Rgb = (u8, u8, u8);

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

pub fn wave(direction: WaveDirection, speed: u8) -> Packet {
    let dir = match direction {
        WaveDirection::Left => 0x01,
        WaveDirection::Right => 0x02,
    };
    Packet::new(
        TID_RGB,
        CLASS_EXTENDED_MATRIX,
        CMD_EFFECT,
        0x06,
        &[VARSTORE, ZERO_LED, 0x04, dir, speed],
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

pub fn breath_dual(c1: Rgb, c2: Rgb) -> Packet {
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

/// Store one row of per-LED colors starting at `start_col` (row 0 — the pad
/// is a single 1x18 strip). Layout from openrazer's
/// `razer_chroma_extended_matrix_set_custom_frame2` with a dynamic packet
/// length (`row length + 5`), as used by the 0x1F accessory family
/// (Laptop Stand Chroma, Base Station V2 Chroma, ...).
pub fn custom_frame(start_col: u8, colors: &[Rgb]) -> Packet {
    assert!(
        !colors.is_empty() && start_col as usize + colors.len() <= NUM_LEDS,
        "custom frame must fit in the {NUM_LEDS}-LED strip"
    );
    let stop_col = start_col + colors.len() as u8 - 1;
    let mut args = vec![0x00, 0x00, 0x00, start_col, stop_col];
    for &(r, g, b) in colors {
        args.extend([r, g, b]);
    }
    let data_size = (colors.len() * 3 + 5) as u8;
    Packet::new(
        TID_RGB,
        CLASS_EXTENDED_MATRIX,
        CMD_SET_CUSTOM_FRAME,
        data_size,
        &args,
    )
}

/// Switch the device to displaying the stored custom frame
/// (openrazer `razer_chroma_extended_matrix_effect_custom_frame`: NOSTORE,
/// effect 0x08, data size 0x0C).
pub fn custom_apply() -> Packet {
    Packet::new(
        TID_RGB,
        CLASS_EXTENDED_MATRIX,
        CMD_EFFECT,
        0x0C,
        &[NOSTORE, ZERO_LED, EFFECT_CUSTOM_FRAME],
    )
}

/// Set the device mode: 0x00 = normal, 0x03 = driver. Some Razer devices
/// only display custom frames in driver mode
/// (openrazer `razer_chroma_standard_set_device_mode`).
pub fn device_mode(mode: u8) -> Packet {
    Packet::new(TID_RGB, 0x00, 0x04, 0x02, &[mode, 0x00])
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

/// Decode the printable ASCII serial from a serial-query response.
pub fn serial_text(resp: &crate::packet::Response) -> String {
    resp.args[..22]
        .iter()
        .take_while(|&&b| b != 0)
        .map(|&b| b as char)
        .filter(|c| c.is_ascii_graphic())
        .collect()
}

// ---------------------------------------------------------------------------
// Color helpers for custom frames.

/// Spread `colors` over `n` LEDs in contiguous blocks (e.g. 2 colors over
/// 18 LEDs gives 9 LEDs of each).
pub fn stretch(colors: &[Rgb], n: usize) -> Vec<Rgb> {
    assert!(!colors.is_empty());
    (0..n).map(|i| colors[i * colors.len() / n]).collect()
}

/// Linear gradient from `from` to `to`, inclusive of both ends.
pub fn gradient(from: Rgb, to: Rgb, n: usize) -> Vec<Rgb> {
    let lerp = |a: u8, b: u8, t: f64| (a as f64 + (b as f64 - a as f64) * t).round() as u8;
    (0..n)
        .map(|i| {
            let t = if n > 1 {
                i as f64 / (n - 1) as f64
            } else {
                0.0
            };
            (
                lerp(from.0, to.0, t),
                lerp(from.1, to.1, t),
                lerp(from.2, to.2, t),
            )
        })
        .collect()
}

/// Map 0.0..=1.0 (cool..hot) onto a green→yellow→red hue sweep.
pub fn temp_color(frac: f64) -> Rgb {
    let frac = frac.clamp(0.0, 1.0);
    hsv_to_rgb(120.0 * (1.0 - frac), 1.0, 1.0)
}

/// LED-meter frame: the hotter, the more LEDs lit, colored green→red along
/// the strip; unlit LEDs are black.
pub fn meter_frame(frac: f64, n: usize) -> Vec<Rgb> {
    let lit = (frac.clamp(0.0, 1.0) * n as f64).round() as usize;
    (0..n)
        .map(|i| {
            if i < lit {
                temp_color(i as f64 / (n - 1) as f64)
            } else {
                (0, 0, 0)
            }
        })
        .collect()
}

fn hsv_to_rgb(h: f64, s: f64, v: f64) -> Rgb {
    let c = v * s;
    let hp = (h % 360.0) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let to8 = |f: f64| ((f + m) * 255.0).round() as u8;
    (to8(r), to8(g), to8(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stretch_blocks() {
        let two = stretch(&[(1, 1, 1), (2, 2, 2)], 18);
        assert_eq!(two.len(), 18);
        assert!(two[..9].iter().all(|&c| c == (1, 1, 1)));
        assert!(two[9..].iter().all(|&c| c == (2, 2, 2)));
        // A full frame passes through unchanged.
        let full: Vec<Rgb> = (0..18).map(|i| (i, i, i)).collect();
        assert_eq!(stretch(&full, 18), full);
    }

    #[test]
    fn gradient_endpoints() {
        let g = gradient((0, 0, 0), (255, 255, 255), 18);
        assert_eq!(g.len(), 18);
        assert_eq!(g[0], (0, 0, 0));
        assert_eq!(g[17], (255, 255, 255));
        assert_eq!(gradient((9, 9, 9), (1, 2, 3), 1), vec![(9, 9, 9)]);
    }

    #[test]
    fn temp_color_anchors() {
        assert_eq!(temp_color(0.0), (0, 255, 0)); // green
        assert_eq!(temp_color(0.5), (255, 255, 0)); // yellow
        assert_eq!(temp_color(1.0), (255, 0, 0)); // red
    }

    #[test]
    fn meter_frame_lights_prefix() {
        assert!(meter_frame(0.0, 18).iter().all(|&c| c == (0, 0, 0)));
        let half = meter_frame(0.5, 18);
        assert_eq!(half.iter().filter(|&&c| c != (0, 0, 0)).count(), 9);
        assert!(half[..9].iter().all(|&c| c != (0, 0, 0)));
        let full = meter_frame(1.0, 18);
        assert!(full.iter().all(|&c| c != (0, 0, 0)));
        assert_eq!(full[0], (0, 255, 0));
        assert_eq!(full[17], (255, 0, 0));
    }
}
