//! `padctl doctor`: automated diagnostics for the situations the README's
//! troubleshooting section covers by hand — device present, control
//! interface openable, protocol responding, brightness stuck at 0, Razer
//! Synapse fighting over the device, temperature source quality, config
//! validity, and launchd service health.
//!
//! Every check runs even when earlier ones fail, so one run paints the
//! whole picture. Warnings don't affect the exit code; failures do.

use std::process::Command;

use anyhow::{Result, bail};

use crate::config;
use crate::curve;
use crate::device::{OpenOpts, Pad, Selector, api, list};
use crate::lighting;
use crate::rgb;
use crate::service;
use crate::temp::TempReader;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pass,
    Warn,
    Fail,
    Skip,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Pass => "ok",
            Status::Warn => "warn",
            Status::Fail => "FAIL",
            Status::Skip => "skip",
        }
    }
}

#[derive(Debug)]
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
    /// A concrete next step shown under the check when it needs attention.
    pub fix: Option<String>,
}

impl Check {
    fn pass(name: &'static str, detail: impl Into<String>) -> Self {
        Check {
            name,
            status: Status::Pass,
            detail: detail.into(),
            fix: None,
        }
    }

    fn warn(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Check {
            name,
            status: Status::Warn,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }

    fn fail(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Check {
            name,
            status: Status::Fail,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }

    fn skip(name: &'static str, detail: impl Into<String>) -> Self {
        Check {
            name,
            status: Status::Skip,
            detail: detail.into(),
            fix: None,
        }
    }
}

pub fn run(selector: &Selector, json: bool) -> Result<()> {
    let checks = collect(selector);
    let failed = checks.iter().filter(|c| c.status == Status::Fail).count();
    if json {
        println!("{}", serde_json::to_string_pretty(&json_report(&checks))?);
    } else {
        print!("{}", render(&checks));
    }
    if failed > 0 {
        bail!("{failed} of {} checks failed", checks.len());
    }
    Ok(())
}

fn collect(selector: &Selector) -> Vec<Check> {
    let mut checks = Vec::new();
    checks.extend(device_checks(selector));
    checks.push(synapse_check());
    checks.push(temperature_check());
    checks.push(config_check());
    checks.push(service_check());
    checks
}

/// Device-side checks: presence, open, protocol probe, brightness.
/// Later checks degrade to Skip when an earlier one fails.
fn device_checks(selector: &Selector) -> Vec<Check> {
    let permission_fix = if cfg!(target_os = "macos") {
        "grant your terminal app Input Monitoring in System Settings → \
         Privacy & Security, then retry"
    } else {
        "add a udev rule granting your user access to the pad's hidraw node"
    };

    let api = match api() {
        Ok(api) => api,
        Err(e) => {
            let mut out = vec![Check::fail(
                "hidapi",
                format!("could not initialize HID access: {e:#}"),
                permission_fix,
            )];
            out.extend(skips(
                &["device", "open", "protocol", "brightness"],
                "HID access unavailable",
            ));
            return out;
        }
    };

    let interfaces = list(&api, selector);
    if interfaces.is_empty() {
        let mut out = vec![Check::fail(
            "device",
            "no Razer Laptop Cooling Pad (1532:0f43) found",
            if cfg!(target_os = "macos") {
                "check the USB cable/hub; `system_profiler SPUSBDataType` should list \
                 \"Razer Laptop Cooling Pad\""
            } else {
                "check the USB cable/hub; `lsusb` should list 1532:0f43"
            },
        )];
        out.extend(skips(&["open", "protocol", "brightness"], "no device"));
        return out;
    }
    let mut out = vec![Check::pass(
        "device",
        format!(
            "found ({} HID interface{})",
            interfaces.len(),
            if interfaces.len() == 1 { "" } else { "s" }
        ),
    )];

    let pad = match Pad::open(&api, selector, OpenOpts::default()) {
        Ok(pad) => pad,
        Err(e) => {
            out.push(Check::fail("open", format!("{e:#}"), permission_fix));
            out.extend(skips(
                &["protocol", "brightness"],
                "control interface not open",
            ));
            return out;
        }
    };
    out.push(Check::pass("open", "control interface opened"));

    match pad
        .query(&rgb::firmware_version())
        .and_then(|fw| Ok((fw, pad.query(&rgb::serial())?)))
    {
        Ok((fw, serial)) => out.push(Check::pass(
            "protocol",
            format!(
                "device responding (firmware v{}.{}, serial {})",
                fw.args[0],
                fw.args[1],
                rgb::serial_text(&serial)
            ),
        )),
        Err(e) => out.push(Check::fail(
            "protocol",
            format!("device did not answer a firmware query: {e:#}"),
            "unplug and replug the pad; if Razer Synapse is running, quit it first",
        )),
    }

    match pad.query(&rgb::brightness_get()) {
        Ok(resp) => {
            let pct = resp.args[2] as u16 * 100 / 255;
            if pct == 0 {
                out.push(Check::warn(
                    "brightness",
                    "persisted at 0% — lighting commands will show nothing",
                    "run `padctl rgb brightness 100`",
                ));
            } else {
                out.push(Check::pass("brightness", format!("{pct}%")));
            }
        }
        Err(e) => out.push(Check::warn(
            "brightness",
            format!("could not read brightness: {e:#}"),
            "unplug and replug the pad if lighting commands also fail",
        )),
    }

    out
}

fn skips(names: &[&'static str], reason: &str) -> Vec<Check> {
    names
        .iter()
        .map(|&name| Check::skip(name, reason.to_string()))
        .collect()
}

/// Razer Synapse's engine holds the device open and can silently override
/// or reject commands sent by anyone else.
fn synapse_check() -> Check {
    if !cfg!(target_os = "macos") {
        return Check::skip("synapse", "macOS only");
    }
    match Command::new("pgrep")
        .args(["-x", "RazerAppEngine"])
        .output()
    {
        Ok(out) if out.status.success() => Check::warn(
            "synapse",
            "RazerAppEngine is running and may fight over the device",
            "quit Razer Synapse if commands are accepted but nothing happens",
        ),
        Ok(_) => Check::pass("synapse", "RazerAppEngine not running"),
        Err(e) => Check::skip("synapse", format!("could not run pgrep: {e}")),
    }
}

fn temperature_check() -> Check {
    match TempReader::new().and_then(|r| Ok((r.read()?, r))) {
        Ok((celsius, reader)) => {
            if reader.is_fallback() {
                Check::warn(
                    "temperature",
                    format!(
                        "{celsius:.1}°C via {} — coarse estimates only",
                        reader.source_name()
                    ),
                    "the fan curve still works but reacts in big steps; real \
                     sensors were not readable on this machine",
                )
            } else {
                Check::pass(
                    "temperature",
                    format!("{celsius:.1}°C via {}", reader.source_name()),
                )
            }
        }
        Err(e) => Check::warn(
            "temperature",
            format!("no temperature source: {e:#}"),
            "`padctl curve` cannot run without one; fan/rgb commands still work",
        ),
    }
}

/// The config file must parse AND resolve: a curve typo or invalid
/// [lighting] section makes the launchd service crash-loop at login.
fn config_check() -> Check {
    let path = config::path();
    match config::load() {
        Ok(None) => Check::pass("config", format!("{} absent (defaults)", path.display())),
        Ok(Some(cfg)) => {
            let validated = curve::resolve(&curve::CurveArgs::default(), Some(&cfg.curve))
                .map(|_| ())
                .and_then(|()| lighting::plan(&cfg.lighting).map(|_| ()));
            match validated {
                Ok(()) => Check::pass("config", format!("{} valid", path.display())),
                Err(e) => Check::fail(
                    "config",
                    format!("{} has invalid settings: {e:#}", path.display()),
                    "fix the value or start over with `padctl config init --force`",
                ),
            }
        }
        Err(e) => Check::fail(
            "config",
            format!("{e:#}"),
            "fix the syntax or start over with `padctl config init --force`",
        ),
    }
}

/// The launchd agent points at an absolute binary path; a moved or deleted
/// binary leaves a service that silently never starts.
fn service_check() -> Check {
    if !cfg!(target_os = "macos") {
        return Check::skip("service", "launchd service is macOS only");
    }
    let plist = match service::plist_path() {
        Ok(p) => p,
        Err(e) => return Check::skip("service", format!("{e:#}")),
    };
    if !plist.exists() {
        return Check::pass("service", "not installed");
    }
    let text = match std::fs::read_to_string(&plist) {
        Ok(t) => t,
        Err(e) => {
            return Check::fail(
                "service",
                format!("could not read {}: {e}", plist.display()),
                "reinstall with `padctl service install`",
            );
        }
    };
    let Some(program) = service::program_from_plist(&text) else {
        return Check::fail(
            "service",
            format!("{} has no ProgramArguments entry", plist.display()),
            "reinstall with `padctl service install`",
        );
    };
    if !std::path::Path::new(&program).exists() {
        return Check::fail(
            "service",
            format!("installed agent points at a missing binary: {program}"),
            "reinstall from the binary's new location with `padctl service install`",
        );
    }
    let loaded = service::agent_runtime().unwrap_or(None);
    let state = match &loaded {
        Some(detail) => format!("installed, {detail}"),
        None => "installed but not loaded".to_string(),
    };
    if program.contains("/target/") {
        return Check::warn(
            "service",
            format!("{state}; runs a build-tree binary ({program})"),
            "copy padctl somewhere stable (e.g. /usr/local/bin) and reinstall",
        );
    }
    if loaded.is_none() {
        return Check::warn(
            "service",
            state,
            "load it with `padctl service install` (or `launchctl bootstrap`)",
        );
    }
    Check::pass("service", state)
}

fn render(checks: &[Check]) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    for c in checks {
        writeln!(out, "  {:<5} {:<12} {}", c.status.label(), c.name, c.detail).unwrap();
        if let Some(fix) = &c.fix {
            writeln!(out, "        {:<12} fix: {fix}", "").unwrap();
        }
    }
    let count = |s: Status| checks.iter().filter(|c| c.status == s).count();
    writeln!(
        out,
        "\n{} ok, {} warning(s), {} failed, {} skipped",
        count(Status::Pass),
        count(Status::Warn),
        count(Status::Fail),
        count(Status::Skip)
    )
    .unwrap();
    out
}

fn json_report(checks: &[Check]) -> serde_json::Value {
    let count = |s: Status| checks.iter().filter(|c| c.status == s).count();
    serde_json::json!({
        "checks": checks.iter().map(|c| serde_json::json!({
            "name": c.name,
            "status": c.status.label(),
            "detail": c.detail,
            "fix": c.fix,
        })).collect::<Vec<_>>(),
        "ok": count(Status::Pass),
        "warnings": count(Status::Warn),
        "failed": count(Status::Fail),
        "skipped": count(Status::Skip),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Check> {
        vec![
            Check::pass("device", "found (2 HID interfaces)"),
            Check::warn("brightness", "persisted at 0%", "padctl rgb brightness 100"),
            Check::fail("config", "bad TOML", "padctl config init --force"),
            Check::skip("service", "macOS only"),
        ]
    }

    #[test]
    fn render_shows_status_detail_and_fixes() {
        let text = render(&sample());
        assert!(text.contains("ok    device"));
        assert!(text.contains("warn  brightness"));
        assert!(text.contains("FAIL  config"));
        assert!(text.contains("skip  service"));
        assert!(text.contains("fix: padctl rgb brightness 100"));
        assert!(text.contains("1 ok, 1 warning(s), 1 failed, 1 skipped"));
    }

    #[test]
    fn json_report_counts_statuses() {
        let v = json_report(&sample());
        assert_eq!(v["ok"], 1);
        assert_eq!(v["warnings"], 1);
        assert_eq!(v["failed"], 1);
        assert_eq!(v["skipped"], 1);
        assert_eq!(v["checks"].as_array().unwrap().len(), 4);
        assert_eq!(v["checks"][0]["name"], "device");
        assert_eq!(v["checks"][0]["fix"], serde_json::Value::Null);
        assert_eq!(v["checks"][2]["status"], "FAIL");
    }

    #[test]
    fn skips_marks_every_named_check() {
        let out = skips(&["open", "protocol"], "no device");
        assert_eq!(
            out.iter().map(|c| c.name).collect::<Vec<_>>(),
            vec!["open", "protocol"]
        );
        assert!(out.iter().all(|c| c.status == Status::Skip));
        assert!(out.iter().all(|c| c.detail == "no device"));
    }
}
