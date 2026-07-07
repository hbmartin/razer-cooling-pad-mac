//! Run the fan curve as a macOS LaunchAgent so it starts at login and keeps
//! running in the background. `install` writes a plist pointing at the
//! current padctl binary running `padctl curve` (curve settings come from
//! ~/.config/padctl/config.toml, see `padctl config`).

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};

pub const LABEL: &str = "io.github.hbmartin.padctl";

#[derive(clap::Subcommand)]
pub enum ServiceCmd {
    /// Install and start the launchd agent (runs `padctl curve` at login)
    Install,
    /// Stop and remove the launchd agent
    Uninstall,
    /// Show whether the agent is loaded and running
    Status,
}

pub fn run(cmd: ServiceCmd) -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("padctl service manages a launchd agent and is only supported on macOS");
    }
    match cmd {
        ServiceCmd::Install => install(),
        ServiceCmd::Uninstall => uninstall(),
        ServiceCmd::Status => status(),
    }
}

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

/// Path of the installed LaunchAgent plist (also used by `padctl doctor`).
pub fn plist_path() -> Result<PathBuf> {
    Ok(home()?
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

fn log_path() -> Result<PathBuf> {
    Ok(home()?.join("Library/Logs/padctl.log"))
}

fn uid() -> Result<String> {
    let out = Command::new("id")
        .arg("-u")
        .output()
        .context("running id -u")?;
    if !out.status.success() {
        bail!("id -u failed");
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Extract the program path from an agent plist we wrote: the first
/// `<string>` under `ProgramArguments`. Used by `padctl doctor` to notice
/// a service pointing at a moved or deleted binary.
pub fn program_from_plist(plist: &str) -> Option<String> {
    let after_key = plist.split("<key>ProgramArguments</key>").nth(1)?;
    let value = after_key
        .split("<string>")
        .nth(1)?
        .split("</string>")
        .next()?;
    Some(xml_unescape(value))
}

fn plist_content(exe: &str, log: &str) -> String {
    let exe = xml_escape(exe);
    let log = xml_escape(log);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>curve</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ProcessType</key>
    <string>Background</string>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#
    )
}

fn launchctl(args: &[&str]) -> Result<std::process::Output> {
    Command::new("launchctl")
        .args(args)
        .output()
        .context("running launchctl")
}

fn install() -> Result<()> {
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .context("locating the padctl binary")?;
    let exe_str = exe.to_string_lossy().into_owned();
    if exe_str.contains("/target/") {
        eprintln!(
            "warning: installing service pointing at a build-tree binary ({exe_str}).\n\
             Consider copying padctl somewhere stable (e.g. /usr/local/bin) first."
        );
    }

    let plist = plist_path()?;
    let log = log_path()?;
    std::fs::create_dir_all(plist.parent().unwrap())
        .with_context(|| format!("creating {}", plist.parent().unwrap().display()))?;
    std::fs::create_dir_all(log.parent().unwrap())
        .with_context(|| format!("creating {}", log.parent().unwrap().display()))?;
    std::fs::write(&plist, plist_content(&exe_str, &log.to_string_lossy()))
        .with_context(|| format!("writing {}", plist.display()))?;

    let uid = uid()?;
    // Reload cleanly if a previous version is running.
    let _ = launchctl(&["bootout", &format!("gui/{uid}/{LABEL}")]);
    let boot = launchctl(&["bootstrap", &format!("gui/{uid}"), &plist.to_string_lossy()])?;
    if !boot.status.success() {
        // Older macOS fallback.
        let load = launchctl(&["load", "-w", &plist.to_string_lossy()])?;
        if !load.status.success() {
            bail!(
                "launchctl could not load the agent:\n{}{}",
                String::from_utf8_lossy(&boot.stderr),
                String::from_utf8_lossy(&load.stderr)
            );
        }
    }

    println!("installed launch agent {LABEL}");
    println!("  plist: {}", plist.display());
    println!("  logs:  {} (tail -f to watch)", log.display());
    println!("  curve settings come from ~/.config/padctl/config.toml (padctl config init)");
    Ok(())
}

fn uninstall() -> Result<()> {
    let uid = uid()?;
    let _ = launchctl(&["bootout", &format!("gui/{uid}/{LABEL}")]);
    let plist = plist_path()?;
    if plist.exists() {
        std::fs::remove_file(&plist).with_context(|| format!("removing {}", plist.display()))?;
        println!("removed {}", plist.display());
    } else {
        println!("no plist at {}", plist.display());
    }
    println!("service {LABEL} unloaded");
    Ok(())
}

/// Whether the launch agent is currently loaded in launchd; `Ok(Some(_))`
/// carries its state/pid detail when available. Also used by `padctl doctor`.
pub fn agent_runtime() -> Result<Option<String>> {
    let uid = uid()?;
    let out = launchctl(&["print", &format!("gui/{uid}/{LABEL}")])?;
    if !out.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let parts: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("state =") || l.starts_with("pid ="))
        .map(str::to_string)
        .collect();
    Ok(Some(if parts.is_empty() {
        "loaded".to_string()
    } else {
        parts.join(", ")
    }))
}

fn status() -> Result<()> {
    let plist = plist_path()?;
    println!(
        "plist: {} ({})",
        plist.display(),
        if plist.exists() { "present" } else { "missing" }
    );
    match agent_runtime()? {
        Some(detail) => {
            if detail != "loaded" {
                println!("{detail}");
            }
            println!("loaded: yes");
        }
        None => println!("loaded: no"),
    }
    println!("logs:  {}", log_path()?.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_is_well_formed_and_escaped() {
        let p = plist_content("/Users/x&y/bin/padctl", "/tmp/padctl <log>.log");
        assert!(p.contains("<string>/Users/x&amp;y/bin/padctl</string>"));
        assert!(p.contains("<string>/tmp/padctl &lt;log&gt;.log</string>"));
        assert!(p.contains("<string>curve</string>"));
        assert!(p.contains(LABEL));
        assert!(p.starts_with("<?xml"));
    }

    #[test]
    fn program_round_trips_through_plist() {
        let p = plist_content("/Users/x&y/bin/padctl", "/tmp/padctl.log");
        assert_eq!(
            program_from_plist(&p).as_deref(),
            Some("/Users/x&y/bin/padctl")
        );
        assert_eq!(program_from_plist("not a plist"), None);
        assert_eq!(program_from_plist(""), None);
    }
}
