mod curve;
mod device;
mod fan;
mod packet;
mod rgb;
mod temp;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use crate::device::Pad;
use crate::packet::{PACKET_LEN, REPORT_LEN};

#[derive(Parser)]
#[command(
    name = "padctl",
    about = "Control the fans and lights of a Razer Laptop Cooling Pad (1532:0f43)",
    version
)]
struct Cli {
    /// Print raw packets sent/received
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    cmd: Cmd,
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
    Info,
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
}

#[derive(Subcommand)]
enum FanCmd {
    /// Set fan speed: RPM (500-3200, step 50) or percentage like 60%
    Set { speed: String },
    /// Turn the fans off
    Off,
    /// Read the current fan speed
    Get,
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
    },
    /// Breathing: no color = random, one color = single, two = dual
    Breath { colors: Vec<String> },
    /// Brightness 0-100 (no value: read current)
    Brightness { percent: Option<u8> },
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let api = device::api()?;

    match cli.cmd {
        Cmd::List => {
            let rows = device::list(&api);
            if rows.is_empty() {
                bail!("no Razer Laptop Cooling Pad (1532:0f43) found");
            }
            for row in rows {
                println!("{row}");
            }
        }
        Cmd::Fan { cmd } => {
            let pad = Pad::open(&api, cli.verbose)?;
            match cmd {
                FanCmd::Set { speed } => {
                    let rpm = parse_speed(&speed)?;
                    pad.send(&fan::set_rpm(rpm))?;
                    println!("fan set to {} RPM", fan::normalize_rpm(rpm));
                }
                FanCmd::Off => {
                    pad.send(&fan::off())?;
                    println!("fan off");
                }
                FanCmd::Get => {
                    let report = pad.read_report()?;
                    println!("{} RPM", fan::rpm_from_report(&report));
                }
            }
        }
        Cmd::Rgb { cmd } => {
            let pad = Pad::open(&api, cli.verbose)?;
            match cmd {
                RgbCmd::Off => {
                    pad.send(&rgb::off())?;
                    println!("lighting off");
                }
                RgbCmd::Static { color } => {
                    let (r, g, b) = parse_color(&color)?;
                    pad.send(&rgb::static_color(r, g, b))?;
                    println!("lighting set to static #{}", color.trim_start_matches('#'));
                }
                RgbCmd::Spectrum => {
                    pad.send(&rgb::spectrum())?;
                    println!("lighting set to spectrum");
                }
                RgbCmd::Wave { dir } => {
                    pad.send(&rgb::wave(dir))?;
                    println!("lighting set to wave ({dir:?})");
                }
                RgbCmd::Breath { colors } => {
                    let packet = match colors.len() {
                        0 => rgb::breath_random(),
                        1 => {
                            let c = parse_color(&colors[0])?;
                            rgb::breath_single(c.0, c.1, c.2)
                        }
                        2 => {
                            let c1 = parse_color(&colors[0])?;
                            let c2 = parse_color(&colors[1])?;
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
            }
        }
        Cmd::Info => {
            let pad = Pad::open(&api, cli.verbose)?;
            let fw = pad.query(&rgb::firmware_version())?;
            println!("firmware: v{}.{}", fw.args[0], fw.args[1]);
            let serial = pad.query(&rgb::serial())?;
            let text: String = serial.args[..22]
                .iter()
                .take_while(|&&b| b != 0)
                .map(|&b| b as char)
                .filter(|c| c.is_ascii_graphic())
                .collect();
            println!("serial:   {text}");
        }
        Cmd::Raw { hex, auto_crc, read } => {
            let pad = Pad::open(&api, cli.verbose)?;
            let cleaned: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
            let raw = hex_decode(&cleaned)?;
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
            let pad = Pad::open(&api, cli.verbose)?;
            curve::run(pad, args)?;
        }
    }
    Ok(())
}

fn parse_speed(s: &str) -> Result<u32> {
    if let Some(pct) = s.strip_suffix('%') {
        let pct: u32 = pct.trim().parse().context("invalid percentage")?;
        if pct > 100 {
            bail!("percentage must be 0-100");
        }
        Ok(fan::percent_to_rpm(pct))
    } else {
        let rpm: u32 = s.trim().parse().context("invalid RPM value")?;
        if rpm < fan::MIN_RPM || rpm > fan::MAX_RPM {
            bail!(
                "RPM must be {}-{} (or use a percentage like 60%)",
                fan::MIN_RPM,
                fan::MAX_RPM
            );
        }
        Ok(rpm)
    }
}

fn parse_color(s: &str) -> Result<(u8, u8, u8)> {
    let s = s.trim_start_matches('#');
    if s.len() != 6 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("color must be 6 hex digits like ff6600");
    }
    let v = u32::from_str_radix(s, 16).unwrap();
    Ok(((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

fn hex_decode(s: &str) -> Result<Vec<u8>> {
    if s.is_empty() || s.len() % 2 != 0 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("expected an even number of hex digits");
    }
    Ok((0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect())
}
