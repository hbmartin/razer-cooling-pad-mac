mod config;
mod curve;
mod device;
mod fan;
mod lighting;
mod logging;
mod packet;
mod parse;
mod reactive;
mod rgb;
mod service;
mod temp;
mod watch;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!("padctl supports macOS (its primary target) and Linux (protocol work, CI) only");

use std::io::Write;

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser, Subcommand};

use crate::device::{OpenOpts, Pad, Selector};
use crate::packet::{PACKET_LEN, REPORT_LEN, Response};
use crate::parse::Speed;

#[derive(Parser)]
#[command(
    name = "padctl",
    about = "Control the fans and lights of a Razer Laptop Cooling Pad (1532:0f43)",
    version
)]
struct Cli {
    /// Debug logging: raw packets sent/received, every curve poll
    #[arg(short, long, global = true)]
    verbose: bool,

    /// After each command, read back the device status and fail if it was
    /// rejected (best effort)
    #[arg(long, global = true)]
    verify: bool,

    /// Select a specific pad by USB serial number (see `padctl list`)
    #[arg(long, global = true)]
    serial: Option<String>,

    /// Select a specific pad by HID path (see `padctl list`)
    #[arg(long, global = true, value_name = "HID_PATH")]
    path: Option<String>,

    #[command(subcommand)]
    cmd: Cmd,
}

impl Cli {
    fn selector(&self) -> Selector {
        Selector {
            serial: self.serial.clone(),
            path: self.path.clone(),
        }
    }

    fn open_opts(&self) -> OpenOpts {
        OpenOpts {
            verify: self.verify,
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// List the pad's HID interfaces
    List,
    /// Fan control
    Fan {
        #[command(subcommand)]
        cmd: FanCmd,
    },
    /// RGB lighting control
    Rgb {
        #[command(subcommand)]
        cmd: RgbCmd,
    },
    /// Show firmware version and serial number
    Info {
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// One-shot overview: fan, brightness, firmware, serial, CPU temperature
    Status {
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Print the current CPU temperature reading the fan curve would use
    Temp {
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// List the temperature sensors visible to padctl
    Sensors {
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Send a raw 90-byte packet (hex); advanced/protocol exploration
    Raw {
        /// Hex bytes of the packet, spaces optional (up to 90 bytes; zero-padded)
        hex: String,
        /// Compute and fill in the XOR checksum at byte 88
        #[arg(long)]
        auto_crc: bool,
        /// Read and print the feature report after sending
        #[arg(long)]
        read: bool,
    },
    /// Run an automatic fan curve from CPU temperature (Ctrl-C to stop)
    Curve(curve::CurveArgs),
    /// Live dashboard for the fan curve with in-place tuning (q to quit)
    Watch(curve::CurveArgs),
    /// Manage a launchd agent that runs the fan curve at login (macOS)
    Service {
        #[command(subcommand)]
        cmd: service::ServiceCmd,
    },
    /// Manage ~/.config/padctl/config.toml
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// Generate shell completions (e.g. `padctl completions zsh`)
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Generate a man page (roff) on stdout
    Manpage,
}

#[derive(Subcommand)]
enum FanCmd {
    /// Set fan speed: RPM (500-3200, step 50), a percentage like 60%, or off
    Set { speed: String },
    /// Turn the fans off
    Off,
    /// Read the current fan speed
    Get {
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum RgbCmd {
    /// Turn lighting off
    Off,
    /// Static color, e.g. ff6600
    Static { color: String },
    /// Spectrum cycling
    Spectrum,
    /// Wave effect
    Wave {
        #[arg(long, value_enum, default_value = "right")]
        dir: rgb::WaveDirection,
        /// Wave speed byte (device default 40)
        #[arg(long, default_value_t = rgb::DEFAULT_WAVE_SPEED)]
        speed: u8,
    },
    /// Breathing: no color = random, one color = single, two = dual
    Breath { colors: Vec<String> },
    /// Brightness 0-100 (no value: read current)
    Brightness { percent: Option<u8> },
    /// Per-LED colors via a custom frame (1-18 colors, stretched to fit;
    /// experimental on this device)
    Custom {
        /// Colors like ff6600 (1-18); fewer than 18 are stretched in blocks
        #[arg(required = true)]
        colors: Vec<String>,
        /// Put the device in driver mode first (try this if nothing changes)
        #[arg(long)]
        driver_mode: bool,
    },
    /// Linear gradient across the strip (experimental on this device)
    Gradient {
        from: String,
        to: String,
        /// Put the device in driver mode first (try this if nothing changes)
        #[arg(long)]
        driver_mode: bool,
    },
    /// Map CPU temperature onto the strip, updating live (Ctrl-C to stop;
    /// experimental on this device)
    Thermal(reactive::ThermalArgs),
    /// Apply the [lighting] section of ~/.config/padctl/config.toml
    Apply,
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Write a commented template config file
    Init {
        /// Overwrite an existing config file
        #[arg(long)]
        force: bool,
    },
    /// Print the config file path
    Path,
    /// Show the effective curve settings (config merged with defaults)
    Show,
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    // The watch TUI owns the terminal: a stderr logger would draw over the
    // alternate screen, so leave the log facade uninitialized (no-op) and
    // let the dashboard record events itself.
    if !matches!(cli.cmd, Cmd::Watch(_)) {
        logging::init(cli.verbose);
    }

    // Commands that don't touch the device (or manage it themselves).
    match &cli.cmd {
        Cmd::Completions { shell } => {
            clap_complete::generate(
                *shell,
                &mut Cli::command(),
                "padctl",
                &mut std::io::stdout(),
            );
            return Ok(());
        }
        Cmd::Manpage => {
            let man = clap_mangen::Man::new(Cli::command());
            let mut out = Vec::new();
            man.render(&mut out).context("rendering man page")?;
            std::io::stdout()
                .write_all(&out)
                .context("writing man page")?;
            return Ok(());
        }
        Cmd::Temp { json } => {
            let reader = temp::TempReader::new()?;
            let celsius = reader.read()?;
            if *json {
                print_json(&serde_json::json!({
                    "celsius": celsius,
                    "source": reader.source_name(),
                }))?;
            } else {
                println!("{celsius:.1}°C ({})", reader.source_name());
            }
            return Ok(());
        }
        Cmd::Sensors { json } => {
            let reader = temp::TempReader::new()?;
            let sensors = reader.sensors()?;
            if *json {
                let rows: Vec<serde_json::Value> = sensors
                    .iter()
                    .map(|r| serde_json::json!({ "name": r.name, "celsius": r.celsius }))
                    .collect();
                print_json(&serde_json::json!({
                    "source": reader.source_name(),
                    "sensors": rows,
                }))?;
            } else {
                println!("source: {}", reader.source_name());
                for r in sensors {
                    println!("{:6.1}°C  {}", r.celsius, r.name);
                }
            }
            return Ok(());
        }
        _ => {}
    }

    match cli.cmd {
        Cmd::Config { ref cmd } => {
            match cmd {
                ConfigCmd::Init { force } => {
                    let path = config::init(*force)?;
                    println!("wrote {}", path.display());
                }
                ConfigCmd::Path => println!("{}", config::path().display()),
                ConfigCmd::Show => {
                    let file = config::load()?;
                    let path = config::path();
                    println!(
                        "config file: {} ({})",
                        path.display(),
                        if file.is_some() {
                            "present"
                        } else {
                            "absent, using defaults"
                        }
                    );
                    let s = curve::resolve(
                        &curve::CurveArgs::default(),
                        file.as_ref().map(|c| &c.curve),
                    )?;
                    println!("curve points: {}", s.points_text);
                    println!("interval:     {}s", s.interval);
                    println!("on exit:      {:?}", s.on_exit);
                    println!("smooth:       {}s", s.smooth);
                    println!("down delay:   {}s", s.down_delay);
                    let lighting = match &file {
                        Some(c) => lighting::plan(&c.lighting)?,
                        None => None,
                    };
                    println!(
                        "lighting:     {}",
                        lighting
                            .map(|p| p.summary)
                            .unwrap_or_else(|| "not configured".into())
                    );
                }
            }
            return Ok(());
        }
        Cmd::Service { cmd } => return service::run(cmd),
        _ => {}
    }

    let mut api = device::api()?;
    let selector = cli.selector();
    let opts = cli.open_opts();

    match cli.cmd {
        Cmd::List => {
            let rows = device::list(&api, &selector);
            if rows.is_empty() {
                bail!("no Razer Laptop Cooling Pad (1532:0f43) found");
            }
            for row in rows {
                println!("{row}");
            }
        }
        Cmd::Fan { cmd } => {
            let pad = Pad::open(&api, &selector, opts)?;
            match cmd {
                FanCmd::Set { speed } => match parse::parse_speed(&speed)? {
                    Speed::Off => {
                        pad.send(&fan::off())?;
                        println!("fan off");
                    }
                    Speed::Rpm(rpm) => {
                        pad.send(&fan::set_rpm(rpm))?;
                        println!("fan set to {} RPM", fan::normalize_rpm(rpm));
                    }
                },
                FanCmd::Off => {
                    pad.send(&fan::off())?;
                    println!("fan off");
                }
                FanCmd::Get { json } => {
                    let report = pad.read_report()?;
                    let rpm = fan::rpm_from_report(&report);
                    if json {
                        print_json(&serde_json::json!({ "rpm": rpm, "off": rpm == 0 }))?;
                    } else {
                        println!("{rpm} RPM");
                    }
                }
            }
        }
        Cmd::Rgb { cmd } => {
            let pad = Pad::open(&api, &selector, opts)?;
            match cmd {
                RgbCmd::Off => {
                    pad.send(&rgb::off())?;
                    println!("lighting off");
                }
                RgbCmd::Static { color } => {
                    let (r, g, b) = parse::parse_color(&color)?;
                    pad.send(&rgb::static_color(r, g, b))?;
                    println!("lighting set to static #{}", color.trim_start_matches('#'));
                }
                RgbCmd::Spectrum => {
                    pad.send(&rgb::spectrum())?;
                    println!("lighting set to spectrum");
                }
                RgbCmd::Wave { dir, speed } => {
                    pad.send(&rgb::wave(dir, speed))?;
                    println!("lighting set to wave ({dir:?}, speed {speed})");
                }
                RgbCmd::Breath { colors } => {
                    let packet = match colors.len() {
                        0 => rgb::breath_random(),
                        1 => {
                            let c = parse::parse_color(&colors[0])?;
                            rgb::breath_single(c.0, c.1, c.2)
                        }
                        2 => {
                            let c1 = parse::parse_color(&colors[0])?;
                            let c2 = parse::parse_color(&colors[1])?;
                            rgb::breath_dual(c1, c2)
                        }
                        n => bail!("breath takes 0, 1 or 2 colors, got {n}"),
                    };
                    pad.send(&packet)?;
                    println!("lighting set to breath");
                }
                RgbCmd::Brightness { percent: Some(pct) } => {
                    if pct > 100 {
                        bail!("brightness must be 0-100");
                    }
                    let raw = (pct as u16 * 255 / 100) as u8;
                    pad.send(&rgb::brightness_set(raw))?;
                    println!("brightness set to {pct}%");
                }
                RgbCmd::Brightness { percent: None } => {
                    let resp = pad.query(&rgb::brightness_get())?;
                    println!("brightness: {}%", resp.args[2] as u16 * 100 / 255);
                }
                RgbCmd::Custom {
                    colors,
                    driver_mode,
                } => {
                    if colors.len() > rgb::NUM_LEDS {
                        bail!("at most {} colors fit on the strip", rgb::NUM_LEDS);
                    }
                    let parsed: Vec<rgb::Rgb> = colors
                        .iter()
                        .map(|c| parse::parse_color(c))
                        .collect::<Result<_>>()?;
                    let frame = rgb::stretch(&parsed, rgb::NUM_LEDS);
                    send_custom_frame(&pad, &frame, driver_mode)?;
                    println!("lighting set to custom frame ({} colors)", colors.len());
                }
                RgbCmd::Gradient {
                    from,
                    to,
                    driver_mode,
                } => {
                    let from = parse::parse_color(&from)?;
                    let to = parse::parse_color(&to)?;
                    let frame = rgb::gradient(from, to, rgb::NUM_LEDS);
                    send_custom_frame(&pad, &frame, driver_mode)?;
                    println!("lighting set to gradient");
                }
                RgbCmd::Thermal(args) => reactive::run(pad, args)?,
                RgbCmd::Apply => {
                    let file = config::load()?.with_context(|| {
                        format!(
                            "no config file at {} (run `padctl config init`)",
                            config::path().display()
                        )
                    })?;
                    let plan = lighting::plan(&file.lighting)?
                        .context("the config has no [lighting] settings to apply")?;
                    for packet in &plan.packets {
                        pad.send(packet)?;
                    }
                    println!("lighting applied from config: {}", plan.summary);
                }
            }
        }
        Cmd::Info { json } => {
            let pad = Pad::open(&api, &selector, opts)?;
            let fw = pad.query(&rgb::firmware_version())?;
            let serial = pad.query(&rgb::serial())?;
            if json {
                print_json(&serde_json::json!({
                    "firmware": format!("{}.{}", fw.args[0], fw.args[1]),
                    "serial": serial_text(&serial),
                }))?;
            } else {
                println!("firmware: v{}.{}", fw.args[0], fw.args[1]);
                println!("serial:   {}", serial_text(&serial));
            }
        }
        Cmd::Status { json } => {
            let pad = Pad::open(&api, &selector, opts)?;
            // Read the fan report first: queries overwrite the report buffer.
            let report = pad.read_report()?;
            let rpm = fan::rpm_from_report(&report);
            let brightness = pad.query(&rgb::brightness_get())?;
            let brightness_pct = brightness.args[2] as u16 * 100 / 255;
            let fw = pad.query(&rgb::firmware_version())?;
            let serial = pad.query(&rgb::serial())?;
            let temp = temp::TempReader::new().and_then(|r| Ok((r.read()?, r.source_name())));
            if json {
                let (celsius, source) = match &temp {
                    Ok((t, s)) => (Some(*t), Some(*s)),
                    Err(_) => (None, None),
                };
                print_json(&serde_json::json!({
                    "fan_rpm": rpm,
                    "fan_off": rpm == 0,
                    "fan_percent": if rpm == 0 { None } else { Some(fan::rpm_to_percent(rpm)) },
                    "brightness_percent": brightness_pct,
                    "firmware": format!("{}.{}", fw.args[0], fw.args[1]),
                    "serial": serial_text(&serial),
                    "cpu_temp_celsius": celsius,
                    "temp_source": source,
                }))?;
            } else {
                if rpm == 0 {
                    println!("fan:        off");
                } else {
                    println!("fan:        {rpm} RPM (~{}%)", fan::rpm_to_percent(rpm));
                }
                println!("brightness: {brightness_pct}%");
                println!("firmware:   v{}.{}", fw.args[0], fw.args[1]);
                println!("serial:     {}", serial_text(&serial));
                match temp {
                    Ok((t, source)) => println!("cpu temp:   {t:.1}°C ({source})"),
                    Err(e) => println!("cpu temp:   unavailable ({e:#})"),
                }
            }
        }
        Cmd::Raw {
            hex,
            auto_crc,
            read,
        } => {
            let pad = Pad::open(&api, &selector, opts)?;
            let cleaned: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
            let raw = parse::hex_decode(&cleaned)?;
            if raw.len() > PACKET_LEN {
                bail!("packet is {} bytes, max {PACKET_LEN}", raw.len());
            }
            let mut report = [0u8; REPORT_LEN];
            report[1..1 + raw.len()].copy_from_slice(&raw);
            if auto_crc {
                report[1 + packet::CRC_OFFSET] = packet::crc(&report[1..]);
            }
            pad.send_report(&report)?;
            println!("sent: {}", device::hex(&report[1..]));
            if read {
                std::thread::sleep(std::time::Duration::from_millis(100));
                let resp = pad.read_report()?;
                println!("read: {}", device::hex(&resp[1..]));
            }
        }
        Cmd::Curve(args) => {
            curve::run(&mut api, &selector, opts, args)?;
        }
        Cmd::Watch(args) => {
            watch::run(&mut api, &selector, opts, args, cli.verbose)?;
        }
        // Handled before the device was opened.
        Cmd::Config { .. }
        | Cmd::Service { .. }
        | Cmd::Completions { .. }
        | Cmd::Manpage
        | Cmd::Temp { .. }
        | Cmd::Sensors { .. } => unreachable!(),
    }
    Ok(())
}

fn print_json(value: &serde_json::Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// Store a full-strip custom frame and switch the pad to display it.
fn send_custom_frame(pad: &Pad, frame: &[rgb::Rgb], driver_mode: bool) -> Result<()> {
    if driver_mode {
        pad.send(&rgb::device_mode(0x03))?;
    }
    pad.send(&rgb::custom_frame(0, frame))?;
    pad.send(&rgb::custom_apply())?;
    Ok(())
}

/// Decode the printable ASCII serial from a serial-query response.
fn serial_text(resp: &Response) -> String {
    resp.args[..22]
        .iter()
        .take_while(|&&b| b != 0)
        .map(|&b| b as char)
        .filter(|c| c.is_ascii_graphic())
        .collect()
}
