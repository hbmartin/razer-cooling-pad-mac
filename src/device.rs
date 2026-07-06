//! Discovery and I/O for the cooling pad's HID control interface.
//!
//! The pad exposes two HID interfaces, both usage page 0x0C / usage 1 on
//! macOS. Interface 0 is the Razer control interface (feature reports only,
//! MaxFeatureReportSize=90); interface 1 carries media-key input events and
//! must never be opened (it belongs to Apple's HID event driver, and opening
//! it can trip the Input Monitoring permission gate).

use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use hidapi::{DeviceInfo, HidApi, HidDevice};

use crate::packet::{Packet, REPORT_LEN, Response, Status};

pub const VID: u16 = 0x1532;
pub const PID: u16 = 0x0F43;

/// Pause between consecutive commands; Razer firmware can wedge on
/// rapid back-to-back feature reports.
const INTER_COMMAND_DELAY: Duration = Duration::from_millis(50);
/// Wait between sending a 0x8x query and reading its response.
const QUERY_DELAY: Duration = Duration::from_millis(100);
const BUSY_RETRIES: u32 = 3;

pub fn api() -> Result<HidApi> {
    HidApi::new().context("initializing hidapi")
}

fn candidates(api: &HidApi) -> Vec<&DeviceInfo> {
    let mut seen = std::collections::HashSet::new();
    api.device_list()
        .filter(|d| d.vendor_id() == VID && d.product_id() == PID)
        .filter(|d| seen.insert(d.path().to_owned()))
        .collect()
}

pub struct Pad {
    dev: HidDevice,
    pub verbose: bool,
}

impl Pad {
    /// Open the control interface: prefer bInterfaceNumber 0, and verify by
    /// probing a feature-report read (interface 1 has MaxFeatureReportSize=1
    /// and fails the 91-byte read).
    pub fn open(api: &HidApi, verbose: bool) -> Result<Pad> {
        let mut cands = candidates(api);
        if cands.is_empty() {
            bail!(
                "no Razer Laptop Cooling Pad (1532:0f43) found — is it plugged in? \
                 Check with `system_profiler SPUSBDataType`"
            );
        }
        cands.sort_by_key(|d| match d.interface_number() {
            0 => 0,
            -1 => 1,
            n => 1 + n,
        });

        let mut errors = Vec::new();
        for info in cands {
            let path = info.path().to_string_lossy().into_owned();
            match api.open_path(info.path()) {
                Ok(dev) => {
                    let mut buf = [0u8; REPORT_LEN];
                    match dev.get_feature_report(&mut buf) {
                        Ok(n) if n >= REPORT_LEN - 1 => {
                            if verbose {
                                eprintln!(
                                    "opened control interface {} ({})",
                                    info.interface_number(),
                                    path
                                );
                            }
                            return Ok(Pad { dev, verbose });
                        }
                        Ok(n) => errors.push(format!(
                            "{path}: feature report is {n} bytes, expected {REPORT_LEN}"
                        )),
                        Err(e) => errors.push(format!("{path}: feature read failed: {e}")),
                    }
                }
                Err(e) => errors.push(format!("{path}: open failed: {e}")),
            }
        }
        Err(anyhow!(
            "found the cooling pad but could not open its control interface:\n  {}\n\
             If the error mentions permissions, grant your terminal Input Monitoring in \
             System Settings → Privacy & Security. If the device seems stuck, unplug and \
             replug it, or quit Razer Synapse.",
            errors.join("\n  ")
        ))
    }

    /// Send a command packet (fire and forget).
    pub fn send(&self, packet: &Packet) -> Result<()> {
        self.send_report(&packet.to_report())
    }

    /// Send a pre-built 91-byte feature report verbatim (report id + packet).
    pub fn send_report(&self, report: &[u8; REPORT_LEN]) -> Result<()> {
        if self.verbose {
            eprintln!("-> {}", hex(&report[1..]));
        }
        self.dev
            .send_feature_report(report)
            .context("sending feature report")?;
        sleep(INTER_COMMAND_DELAY);
        Ok(())
    }

    /// Read the device's current 91-byte feature report without sending
    /// a request first (the Windows plugin reads RPM this way).
    pub fn read_report(&self) -> Result<[u8; REPORT_LEN]> {
        let mut buf = [0u8; REPORT_LEN];
        let n = self
            .dev
            .get_feature_report(&mut buf)
            .context("reading feature report")?;
        if n < REPORT_LEN - 1 {
            bail!("short feature report: {n} bytes, expected {REPORT_LEN}");
        }
        if self.verbose {
            eprintln!("<- {}", hex(&buf[1..]));
        }
        Ok(buf)
    }

    /// Send a query (0x8x command) and read back its response, retrying
    /// while the device reports busy.
    pub fn query(&self, packet: &Packet) -> Result<Response> {
        for attempt in 0..=BUSY_RETRIES {
            self.send(packet)?;
            sleep(QUERY_DELAY);
            let report = self.read_report()?;
            let resp = Response::from_report(&report);
            match resp.status {
                Status::Busy if attempt < BUSY_RETRIES => {
                    sleep(QUERY_DELAY);
                    continue;
                }
                Status::Ok | Status::New => {
                    let sent = packet.to_report();
                    if resp.class != sent[7] || resp.cmd != sent[8] {
                        bail!(
                            "response does not echo the request (got class 0x{:02x} cmd 0x{:02x}, \
                             sent class 0x{:02x} cmd 0x{:02x})",
                            resp.class,
                            resp.cmd,
                            sent[7],
                            sent[8]
                        );
                    }
                    return Ok(resp);
                }
                status => bail!("device answered: {status}"),
            }
        }
        bail!("device still busy after {BUSY_RETRIES} retries")
    }
}

/// One row per HID interface of the pad, for `padctl list`.
pub fn list(api: &HidApi) -> Vec<String> {
    candidates(api)
        .into_iter()
        .map(|d| {
            let role = if d.interface_number() == 0 {
                "control"
            } else {
                "events (unused)"
            };
            format!(
                "interface {} — usage page 0x{:04x}, usage 0x{:02x}, path {} [{}]",
                d.interface_number(),
                d.usage_page(),
                d.usage(),
                d.path().to_string_lossy(),
                role,
            )
        })
        .collect()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}
