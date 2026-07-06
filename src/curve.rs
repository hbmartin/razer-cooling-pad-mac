//! Automatic fan curve: poll CPU temperature, interpolate a target RPM,
//! and push it to the pad when it changes meaningfully.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Args;

use crate::device::Pad;
use crate::fan;
use crate::temp::{TempReader, source_name};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OnExit {
    /// Turn the fans off when the curve stops
    Off,
    /// Leave the last speed running
    Keep,
}

#[derive(Args)]
pub struct CurveArgs {
    /// Curve points as temp°C:RPM pairs; RPM 0 means fans off
    #[arg(long, default_value = "55:800,65:1500,75:2200,85:3200")]
    points: String,

    /// Seconds between temperature polls
    #[arg(long, default_value_t = 5)]
    interval: u64,

    /// What to do with the fans on Ctrl-C
    #[arg(long, value_enum, default_value = "off")]
    on_exit: OnExit,

    /// Print decisions without sending anything to the pad
    #[arg(long)]
    dry_run: bool,
}

/// Only push a new speed when the target moves at least this much.
const HYSTERESIS_RPM: i64 = 100;

fn parse_points(s: &str) -> Result<Vec<(f64, u32)>> {
    let mut points = Vec::new();
    for pair in s.split(',') {
        let (t, r) = pair
            .split_once(':')
            .with_context(|| format!("bad curve point {pair:?}, expected temp:rpm"))?;
        let temp: f64 = t.trim().parse().with_context(|| format!("bad temperature in {pair:?}"))?;
        let rpm: u32 = r.trim().parse().with_context(|| format!("bad RPM in {pair:?}"))?;
        if rpm != 0 && (rpm < fan::MIN_RPM || rpm > fan::MAX_RPM) {
            bail!(
                "RPM {rpm} out of range: use 0 (off) or {}-{}",
                fan::MIN_RPM,
                fan::MAX_RPM
            );
        }
        points.push((temp, rpm));
    }
    points.sort_by(|a, b| a.0.total_cmp(&b.0));
    if points.is_empty() {
        bail!("curve needs at least one point");
    }
    Ok(points)
}

/// Piecewise-linear interpolation, clamped to the end points.
fn target_rpm(points: &[(f64, u32)], temp: f64) -> u32 {
    let first = points.first().unwrap();
    let last = points.last().unwrap();
    if temp <= first.0 {
        return first.1;
    }
    if temp >= last.0 {
        return last.1;
    }
    for w in points.windows(2) {
        let (t0, r0) = w[0];
        let (t1, r1) = w[1];
        if temp <= t1 {
            let frac = (temp - t0) / (t1 - t0);
            return (r0 as f64 + frac * (r1 as f64 - r0 as f64)).round() as u32;
        }
    }
    last.1
}

pub fn run(pad: Pad, args: CurveArgs) -> Result<()> {
    let points = parse_points(&args.points)?;
    let reader = TempReader::new()?;
    println!(
        "fan curve: {} | temp source: {} | poll every {}s{}",
        points
            .iter()
            .map(|(t, r)| format!("{t}°C→{r}"))
            .collect::<Vec<_>>()
            .join(", "),
        source_name(&reader.source),
        args.interval,
        if args.dry_run { " | DRY RUN" } else { "" }
    );

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || r.store(false, Ordering::SeqCst))
        .context("installing Ctrl-C handler")?;

    let mut current: Option<u32> = None; // last speed we sent (0 = off)
    while running.load(Ordering::SeqCst) {
        match reader.read() {
            Ok(temp) => {
                let raw_target = target_rpm(&points, temp);
                // Below the device minimum means off.
                let target = if raw_target < fan::MIN_RPM {
                    0
                } else {
                    fan::normalize_rpm(raw_target)
                };
                let should_send = match current {
                    None => true,
                    Some(cur) => {
                        (cur == 0) != (target == 0)
                            || (cur as i64 - target as i64).abs() >= HYSTERESIS_RPM
                    }
                };
                let action = if should_send { "->" } else { "  " };
                println!(
                    "{:6.1}°C {action} {}",
                    temp,
                    if target == 0 { "off".to_string() } else { format!("{target} RPM") }
                );
                if should_send && !args.dry_run {
                    let result = if target == 0 {
                        pad.send(&fan::off())
                    } else {
                        pad.send(&fan::set_rpm(target))
                    };
                    if let Err(e) = result {
                        eprintln!("warning: failed to set fan: {e:#}");
                    } else {
                        current = Some(target);
                    }
                } else if should_send {
                    current = Some(target);
                }
            }
            Err(e) => eprintln!("warning: temperature read failed: {e:#}"),
        }

        // Sleep in small slices so Ctrl-C exits promptly.
        let mut remaining = args.interval.max(1) * 10;
        while remaining > 0 && running.load(Ordering::SeqCst) {
            sleep(Duration::from_millis(100));
            remaining -= 1;
        }
    }

    match args.on_exit {
        OnExit::Off if !args.dry_run => {
            pad.send(&fan::off())?;
            println!("\nfan off");
        }
        _ => println!("\nleaving fan as-is"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates_and_clamps() {
        let points = parse_points("55:800,65:1500,75:2200,85:3200").unwrap();
        assert_eq!(target_rpm(&points, 40.0), 800); // clamp low
        assert_eq!(target_rpm(&points, 55.0), 800);
        assert_eq!(target_rpm(&points, 60.0), 1150); // midpoint 800..1500
        assert_eq!(target_rpm(&points, 85.0), 3200);
        assert_eq!(target_rpm(&points, 100.0), 3200); // clamp high
    }

    #[test]
    fn zero_rpm_points_mean_off() {
        let points = parse_points("45:0,60:1000").unwrap();
        assert_eq!(target_rpm(&points, 40.0), 0);
        // interpolated values below MIN_RPM are treated as off by the loop
        assert!(target_rpm(&points, 50.0) < fan::MIN_RPM);
    }

    #[test]
    fn rejects_bad_points() {
        assert!(parse_points("55:400").is_err()); // below min, not 0
        assert!(parse_points("55-800").is_err());
        assert!(parse_points("").is_err());
    }
}
