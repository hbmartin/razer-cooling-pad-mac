//! Temperature-reactive lighting: map CPU temperature onto the pad's 18-LED
//! strip via custom frames (green→red meter or a solid color).
//!
//! Custom frames are experimental on this device — the packet layout follows
//! openrazer's extended-matrix accessories, but the cooling pad itself has
//! not been verified upstream. If nothing changes, try `--driver-mode`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Args;

use crate::device::Pad;
use crate::rgb;
use crate::temp::TempReader;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Style {
    /// Light 0-18 LEDs like a level meter, green→red
    Meter,
    /// All LEDs one color that shifts green→red
    Solid,
}

#[derive(Args)]
pub struct ThermalArgs {
    /// Temperature (°C) mapped to the cool end
    #[arg(long, default_value_t = 45.0)]
    min: f64,

    /// Temperature (°C) mapped to the hot end
    #[arg(long, default_value_t = 85.0)]
    max: f64,

    /// Seconds between updates
    #[arg(long, default_value_t = 2)]
    interval: u64,

    #[arg(long, value_enum, default_value = "meter")]
    style: Style,

    /// Put the device in driver mode first (some Razer devices only show
    /// custom frames in driver mode); normal mode is restored on exit
    #[arg(long)]
    driver_mode: bool,
}

pub fn run(pad: Pad, args: ThermalArgs) -> Result<()> {
    if !args.min.is_finite() || !args.max.is_finite() || args.max <= args.min {
        bail!("--max must be greater than --min");
    }
    let interval = args.interval.max(1);
    let reader = TempReader::new()?;
    println!(
        "thermal lighting: {}..{}°C, {:?} style | temp source: {} | Ctrl-C to stop",
        args.min,
        args.max,
        args.style,
        reader.source_name()
    );

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || r.store(false, Ordering::SeqCst))
        .context("installing signal handler")?;

    if args.driver_mode {
        pad.send(&rgb::device_mode(0x03))?;
    }

    let mut last_frame: Option<Vec<rgb::Rgb>> = None;
    while running.load(Ordering::SeqCst) {
        match reader.read() {
            Ok(temp) => {
                let frac = ((temp - args.min) / (args.max - args.min)).clamp(0.0, 1.0);
                let frame = match args.style {
                    Style::Meter => rgb::meter_frame(frac, rgb::NUM_LEDS),
                    Style::Solid => vec![rgb::temp_color(frac); rgb::NUM_LEDS],
                };
                if last_frame.as_ref() != Some(&frame) {
                    if pad.verbose {
                        eprintln!("{temp:5.1}°C -> frame update");
                    }
                    pad.send(&rgb::custom_frame(0, &frame))?;
                    pad.send(&rgb::custom_apply())?;
                    last_frame = Some(frame);
                }
            }
            Err(e) => eprintln!("warning: temperature read failed: {e:#}"),
        }

        let mut remaining = interval * 10;
        while remaining > 0 && running.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(100));
            remaining -= 1;
        }
    }

    if args.driver_mode {
        pad.send(&rgb::device_mode(0x00))?;
    }
    println!("\nstopped (lighting left as-is; use `padctl rgb` to change it)");
    Ok(())
}
