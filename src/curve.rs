//! Automatic fan curve: poll CPU temperature, smooth it, interpolate a
//! target RPM, and push it to the pad when it changes meaningfully.
//!
//! Built to run unattended (e.g. under launchd via `padctl service`):
//! timestamps on every line, exponential smoothing against spiky loads,
//! delayed spin-down so the fans don't oscillate, and automatic reconnect
//! when the pad is unplugged/replugged or the machine sleeps.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use hidapi::HidApi;

use crate::config::CurveConfig;
use crate::device::{OpenOpts, Pad, Selector};
use crate::fan;
use crate::temp::TempReader;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OnExit {
    /// Turn the fans off when the curve stops
    Off,
    /// Leave the last speed running
    Keep,
}

#[derive(Args, Default)]
pub struct CurveArgs {
    /// Curve points as temp°C:RPM pairs; RPM 0 means fans off
    /// [default: 55:800,65:1500,75:2200,85:3200]
    #[arg(long)]
    points: Option<String>,

    /// Seconds between temperature polls [default: 5]
    #[arg(long)]
    interval: Option<u64>,

    /// What to do with the fans when the curve stops [default: off]
    #[arg(long, value_enum)]
    on_exit: Option<OnExit>,

    /// Temperature smoothing time constant in seconds; 0 disables
    /// [default: 15]
    #[arg(long)]
    smooth: Option<f64>,

    /// Seconds a lower target must persist before slowing down (spin-up is
    /// always immediate; 0 slows down immediately) [default: 30]
    #[arg(long)]
    down_delay: Option<u64>,

    /// Print decisions without sending anything to the pad
    #[arg(long)]
    dry_run: bool,

    /// Ignore ~/.config/padctl/config.toml
    #[arg(long)]
    no_config: bool,
}

/// Only push a new speed when the target moves at least this much.
const HYSTERESIS_RPM: u32 = 100;

const DEFAULT_POINTS: &str = "55:800,65:1500,75:2200,85:3200";
const DEFAULT_INTERVAL: u64 = 5;
const DEFAULT_SMOOTH: f64 = 15.0;
const DEFAULT_DOWN_DELAY: u64 = 30;

/// Curve settings after merging CLI flags, the config file, and defaults
/// (in that precedence order).
pub struct Settings {
    pub points: Vec<(f64, u32)>,
    pub points_text: String,
    pub interval: u64,
    pub on_exit: OnExit,
    pub smooth: f64,
    pub down_delay: u64,
}

pub fn resolve(args: &CurveArgs, file: Option<&CurveConfig>) -> Result<Settings> {
    let file_points = file.and_then(|f| f.points.clone());
    let points_text = args
        .points
        .clone()
        .or(file_points)
        .unwrap_or_else(|| DEFAULT_POINTS.to_string());
    let points = parse_points(&points_text)?;

    let interval = args
        .interval
        .or(file.and_then(|f| f.interval))
        .unwrap_or(DEFAULT_INTERVAL);
    if interval == 0 {
        bail!("interval must be at least 1 second");
    }

    let on_exit = match (args.on_exit, file.and_then(|f| f.on_exit.as_deref())) {
        (Some(v), _) => v,
        (None, Some(s)) => <OnExit as clap::ValueEnum>::from_str(s, true)
            .map_err(|_| anyhow!("config on_exit must be \"off\" or \"keep\", got {s:?}"))?,
        (None, None) => OnExit::Off,
    };

    let smooth = args
        .smooth
        .or(file.and_then(|f| f.smooth))
        .unwrap_or(DEFAULT_SMOOTH);
    if !smooth.is_finite() || smooth < 0.0 {
        bail!("smooth must be a non-negative number of seconds");
    }

    let down_delay = args
        .down_delay
        .or(file.and_then(|f| f.down_delay))
        .unwrap_or(DEFAULT_DOWN_DELAY);

    Ok(Settings {
        points,
        points_text,
        interval,
        on_exit,
        smooth,
        down_delay,
    })
}

fn parse_points(s: &str) -> Result<Vec<(f64, u32)>> {
    let mut points = Vec::new();
    for pair in s.split(',') {
        let (t, r) = pair
            .split_once(':')
            .with_context(|| format!("bad curve point {pair:?}, expected temp:rpm"))?;
        let temp: f64 = t
            .trim()
            .parse()
            .with_context(|| format!("bad temperature in {pair:?}"))?;
        let rpm: u32 = r
            .trim()
            .parse()
            .with_context(|| format!("bad RPM in {pair:?}"))?;
        if rpm != 0 && !(fan::MIN_RPM..=fan::MAX_RPM).contains(&rpm) {
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

/// Exponential moving average with a time constant in seconds.
struct Ema {
    tau: f64,
    value: Option<f64>,
}

impl Ema {
    fn new(tau: f64) -> Self {
        Ema { tau, value: None }
    }

    fn update(&mut self, sample: f64, dt: f64) -> f64 {
        let new = match self.value {
            _ if self.tau <= 0.0 => sample,
            None => sample,
            Some(prev) => {
                let alpha = 1.0 - (-dt / self.tau).exp();
                prev + alpha * (sample - prev)
            }
        };
        self.value = Some(new);
        new
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    /// Push the target to the pad now.
    Send,
    /// Nothing to do; target is close enough to what's running.
    Hold,
    /// Target dropped, waiting out the down-delay before slowing.
    PendingDown,
}

/// Decides when a computed target actually gets sent: hysteresis both ways,
/// immediate spin-up, delayed spin-down.
struct Governor {
    hysteresis: u32,
    down_delay: Duration,
    last: Option<u32>,
    down_since: Option<Instant>,
}

impl Governor {
    fn new(hysteresis: u32, down_delay: Duration) -> Self {
        Governor {
            hysteresis,
            down_delay,
            last: None,
            down_since: None,
        }
    }

    fn decide(&mut self, target: u32, now: Instant) -> Decision {
        let Some(last) = self.last else {
            return Decision::Send;
        };
        let on_off_change = (last == 0) != (target == 0);
        let significant = on_off_change || target.abs_diff(last) >= self.hysteresis;
        if !significant {
            self.down_since = None;
            return Decision::Hold;
        }
        if target > last {
            self.down_since = None;
            return Decision::Send;
        }
        if self.down_delay.is_zero() {
            return Decision::Send;
        }
        match self.down_since {
            None => {
                self.down_since = Some(now);
                Decision::PendingDown
            }
            Some(since) if now.duration_since(since) >= self.down_delay => Decision::Send,
            Some(_) => Decision::PendingDown,
        }
    }

    /// Record that `target` was actually applied to the device.
    fn confirm(&mut self, target: u32) {
        self.last = Some(target);
        self.down_since = None;
    }

    /// Forget device state (after a reconnect the pad may run any speed).
    fn reset(&mut self) {
        self.last = None;
        self.down_since = None;
    }
}

fn timestamp() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

fn rpm_text(rpm: u32) -> String {
    if rpm == 0 {
        "off".to_string()
    } else {
        format!("{rpm} RPM")
    }
}

pub fn run(api: &mut HidApi, selector: &Selector, opts: OpenOpts, args: CurveArgs) -> Result<()> {
    let file_config = if args.no_config {
        None
    } else {
        crate::config::load()?
    };
    let s = resolve(&args, file_config.as_ref().map(|c| &c.curve))?;

    let reader = TempReader::new()?;
    println!(
        "fan curve: {} | temp source: {} | poll {}s | smooth {}s | down-delay {}s{}{}",
        s.points
            .iter()
            .map(|(t, r)| format!("{t}°C→{r}"))
            .collect::<Vec<_>>()
            .join(", "),
        reader.source_name(),
        s.interval,
        s.smooth,
        s.down_delay,
        if file_config.is_some() {
            " | config: ~/.config/padctl/config.toml"
        } else {
            ""
        },
        if args.dry_run { " | DRY RUN" } else { "" }
    );

    // With ctrlc's `termination` feature this also catches SIGTERM/SIGHUP,
    // so `launchctl bootout`/`kill` still trigger the on-exit behavior.
    let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || r.store(false, std::sync::atomic::Ordering::SeqCst))
        .context("installing signal handler")?;
    let is_running = || running.load(std::sync::atomic::Ordering::SeqCst);

    let mut pad: Option<Pad> = None;
    let mut outage_reported = false;
    let mut ema = Ema::new(s.smooth);
    let mut governor = Governor::new(HYSTERESIS_RPM, Duration::from_secs(s.down_delay));

    while is_running() {
        // (Re)connect if needed, so the pad recovers from unplug/replug and
        // sleep/wake without restarting the process.
        if !args.dry_run && pad.is_none() {
            let _ = api.refresh_devices();
            match Pad::open(api, selector, opts) {
                Ok(p) => {
                    if outage_reported {
                        eprintln!("[{}] cooling pad reconnected", timestamp());
                    }
                    outage_reported = false;
                    pad = Some(p);
                    // The pad may have rebooted; make sure we resend.
                    governor.reset();
                }
                Err(e) => {
                    if !outage_reported {
                        eprintln!("[{}] warning: {e:#}", timestamp());
                        eprintln!("[{}] retrying every {}s", timestamp(), s.interval);
                        outage_reported = true;
                    }
                }
            }
        }

        match reader.read() {
            Ok(raw) => {
                let temp = ema.update(raw, s.interval as f64);
                let raw_target = target_rpm(&s.points, temp);
                // Below the device minimum means off.
                let target = if raw_target < fan::MIN_RPM {
                    0
                } else {
                    fan::normalize_rpm(raw_target)
                };

                let decision = governor.decide(target, Instant::now());
                let marker = match decision {
                    Decision::Send => "->",
                    Decision::Hold => "  ",
                    Decision::PendingDown => "..",
                };
                let detail = if opts.verbose && s.smooth > 0.0 {
                    format!(" (raw {raw:.1}°C)")
                } else {
                    String::new()
                };
                println!(
                    "[{}] {temp:5.1}°C {marker} {}{}{}",
                    timestamp(),
                    rpm_text(target),
                    if decision == Decision::PendingDown {
                        " (down pending)"
                    } else {
                        ""
                    },
                    detail,
                );

                if decision == Decision::Send {
                    if args.dry_run {
                        governor.confirm(target);
                    } else if let Some(p) = &pad {
                        let result = if target == 0 {
                            p.send(&fan::off())
                        } else {
                            p.send(&fan::set_rpm(target))
                        };
                        match result {
                            Ok(()) => governor.confirm(target),
                            Err(e) => {
                                eprintln!(
                                    "[{}] warning: failed to set fan: {e:#} — will reconnect",
                                    timestamp()
                                );
                                pad = None;
                            }
                        }
                    }
                    // No device right now: leave the governor unconfirmed so
                    // the send is retried as soon as we reconnect.
                }
            }
            Err(e) => eprintln!("[{}] warning: temperature read failed: {e:#}", timestamp()),
        }

        // Sleep in small slices so signals exit promptly.
        let mut remaining = s.interval * 10;
        while remaining > 0 && is_running() {
            std::thread::sleep(Duration::from_millis(100));
            remaining -= 1;
        }
    }

    match s.on_exit {
        OnExit::Off if !args.dry_run => {
            if pad.is_none() {
                let _ = api.refresh_devices();
                pad = Pad::open(api, selector, opts).ok();
            }
            match &pad {
                Some(p) => {
                    p.send(&fan::off())?;
                    println!("\nfan off");
                }
                None => eprintln!("\nwarning: pad unavailable, could not turn the fan off"),
            }
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

    #[test]
    fn resolve_precedence_cli_over_file_over_default() {
        let file = CurveConfig {
            points: Some("50:0,70:2000".into()),
            interval: Some(10),
            on_exit: Some("keep".into()),
            smooth: Some(20.0),
            down_delay: Some(60),
        };

        // Defaults only.
        let s = resolve(&CurveArgs::default(), None).unwrap();
        assert_eq!(s.points_text, DEFAULT_POINTS);
        assert_eq!(s.interval, DEFAULT_INTERVAL);
        assert_eq!(s.on_exit, OnExit::Off);

        // File overrides defaults.
        let s = resolve(&CurveArgs::default(), Some(&file)).unwrap();
        assert_eq!(s.points_text, "50:0,70:2000");
        assert_eq!(s.interval, 10);
        assert_eq!(s.on_exit, OnExit::Keep);
        assert_eq!(s.smooth, 20.0);
        assert_eq!(s.down_delay, 60);

        // CLI overrides file.
        let args = CurveArgs {
            interval: Some(3),
            on_exit: Some(OnExit::Off),
            ..CurveArgs::default()
        };
        let s = resolve(&args, Some(&file)).unwrap();
        assert_eq!(s.interval, 3);
        assert_eq!(s.on_exit, OnExit::Off);
        assert_eq!(s.smooth, 20.0); // still from file
    }

    #[test]
    fn resolve_rejects_bad_values() {
        let args = CurveArgs {
            interval: Some(0),
            ..CurveArgs::default()
        };
        assert!(resolve(&args, None).is_err());

        let file = CurveConfig {
            on_exit: Some("sideways".into()),
            ..CurveConfig::default()
        };
        assert!(resolve(&CurveArgs::default(), Some(&file)).is_err());
    }

    #[test]
    fn ema_smooths_and_converges() {
        let mut ema = Ema::new(15.0);
        assert_eq!(ema.update(50.0, 5.0), 50.0); // first sample passes through
        let stepped = ema.update(80.0, 5.0);
        assert!(stepped > 50.0 && stepped < 80.0); // moves toward the sample
        let mut v = stepped;
        for _ in 0..100 {
            v = ema.update(80.0, 5.0);
        }
        assert!((v - 80.0).abs() < 0.1); // converges
    }

    #[test]
    fn ema_zero_tau_is_passthrough() {
        let mut ema = Ema::new(0.0);
        assert_eq!(ema.update(50.0, 5.0), 50.0);
        assert_eq!(ema.update(80.0, 5.0), 80.0);
    }

    #[test]
    fn governor_first_target_sends() {
        let mut g = Governor::new(100, Duration::from_secs(30));
        let now = Instant::now();
        assert_eq!(g.decide(1500, now), Decision::Send);
    }

    #[test]
    fn governor_spin_up_is_immediate_and_small_moves_hold() {
        let mut g = Governor::new(100, Duration::from_secs(30));
        let now = Instant::now();
        g.confirm(1500);
        assert_eq!(g.decide(1550, now), Decision::Hold); // within hysteresis
        assert_eq!(g.decide(1450, now), Decision::Hold);
        assert_eq!(g.decide(2000, now), Decision::Send); // up: immediate
    }

    #[test]
    fn governor_spin_down_waits_out_the_delay() {
        let mut g = Governor::new(100, Duration::from_secs(30));
        let t0 = Instant::now();
        g.confirm(2000);
        assert_eq!(g.decide(1000, t0), Decision::PendingDown);
        assert_eq!(
            g.decide(1000, t0 + Duration::from_secs(10)),
            Decision::PendingDown
        );
        assert_eq!(g.decide(1000, t0 + Duration::from_secs(30)), Decision::Send);
    }

    #[test]
    fn governor_bounce_back_cancels_pending_down() {
        let mut g = Governor::new(100, Duration::from_secs(30));
        let t0 = Instant::now();
        g.confirm(2000);
        assert_eq!(g.decide(1000, t0), Decision::PendingDown);
        // Temperature came back: target near current speed again.
        assert_eq!(g.decide(1950, t0 + Duration::from_secs(5)), Decision::Hold);
        // A later drop starts the delay over.
        assert_eq!(
            g.decide(1000, t0 + Duration::from_secs(40)),
            Decision::PendingDown
        );
    }

    #[test]
    fn governor_off_transition_is_delayed_and_on_is_immediate() {
        let mut g = Governor::new(100, Duration::from_secs(30));
        let t0 = Instant::now();
        g.confirm(800);
        assert_eq!(g.decide(0, t0), Decision::PendingDown);
        assert_eq!(g.decide(0, t0 + Duration::from_secs(31)), Decision::Send);
        g.confirm(0);
        assert_eq!(g.decide(500, t0 + Duration::from_secs(32)), Decision::Send);
    }

    #[test]
    fn governor_zero_delay_slows_immediately() {
        let mut g = Governor::new(100, Duration::ZERO);
        let now = Instant::now();
        g.confirm(2000);
        assert_eq!(g.decide(1000, now), Decision::Send);
    }

    #[test]
    fn governor_reset_forces_resend() {
        let mut g = Governor::new(100, Duration::from_secs(30));
        let now = Instant::now();
        g.confirm(1500);
        assert_eq!(g.decide(1500, now), Decision::Hold);
        g.reset();
        assert_eq!(g.decide(1500, now), Decision::Send);
    }
}
