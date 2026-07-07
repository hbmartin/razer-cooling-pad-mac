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

fn plist_path() -> Result<PathBuf> {
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

fn plist_content(exe: &str, home: &str, log: &str) -> String {
    let exe = xml_escape(exe);
    let home = xml_escape(home);
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
    <key>EnvironmentVariables</key>
    <dict>
        <key>HOME</key>
        <string>{home}</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ThrottleInterval</key>
    <integer>30</integer>
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
    let home = home()?;
    std::fs::create_dir_all(plist.parent().unwrap())
        .with_context(|| format!("creating {}", plist.parent().unwrap().display()))?;
    std::fs::create_dir_all(log.parent().unwrap())
        .with_context(|| format!("creating {}", log.parent().unwrap().display()))?;
    std::fs::write(
        &plist,
        plist_content(&exe_str, &home.to_string_lossy(), &log.to_string_lossy()),
    )
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

fn status() -> Result<()> {
    let plist = plist_path()?;
    println!(
        "plist: {} ({})",
        plist.display(),
        if plist.exists() { "present" } else { "missing" }
    );
    let uid = uid()?;
    let out = launchctl(&["print", &format!("gui/{uid}/{LABEL}")])?;
    if out.status.success() {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with("state =") || line.starts_with("pid =") {
                println!("{line}");
            }
        }
        println!("loaded: yes");
    } else {
        println!("loaded: no");
    }
    println!("logs:  {}", log_path()?.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_is_well_formed_and_escaped() {
        let p = plist_content(
            "/Users/x&y/bin/padctl",
            "/Users/x&y",
            "/tmp/padctl <log>.log",
        );
        assert!(p.contains("<string>/Users/x&amp;y/bin/padctl</string>"));
        assert!(p.contains("<key>EnvironmentVariables</key>"));
        assert!(p.contains("<key>HOME</key>"));
        assert!(p.contains("<string>/Users/x&amp;y</string>"));
        assert!(p.contains("<string>/tmp/padctl &lt;log&gt;.log</string>"));
        assert!(p.contains("<string>curve</string>"));
        assert!(p.contains("<key>RunAtLoad</key>\n    <true/>"));
        assert!(p.contains("<key>KeepAlive</key>"));
        assert!(p.contains("<key>SuccessfulExit</key>\n        <false/>"));
        assert!(p.contains("<key>ThrottleInterval</key>\n    <integer>30</integer>"));
        assert!(p.contains("<key>ProcessType</key>\n    <string>Background</string>"));
        assert!(p.contains(LABEL));
        assert!(p.starts_with("<?xml"));
    }
}
