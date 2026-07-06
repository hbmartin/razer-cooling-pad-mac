//! Optional TOML configuration, shared by interactive runs and the launchd
//! service: `~/.config/padctl/config.toml` (respects `XDG_CONFIG_HOME`).
//! CLI flags always take precedence over config values.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub curve: CurveConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurveConfig {
    /// Curve points as "temp:rpm,temp:rpm,..." (same format as --points).
    pub points: Option<String>,
    /// Seconds between temperature polls.
    pub interval: Option<u64>,
    /// "off" or "keep".
    pub on_exit: Option<String>,
    /// Temperature smoothing time constant in seconds (0 disables).
    pub smooth: Option<f64>,
    /// Seconds a lower target must persist before the fans slow down.
    pub down_delay: Option<u64>,
}

pub const TEMPLATE: &str = r#"# padctl configuration.
# CLI flags override these values; commented-out lines use built-in defaults.

[curve]
# Curve points as "temp°C:RPM" pairs. RPM 0 means fans off.
#points = "55:800,65:1500,75:2200,85:3200"

# Seconds between temperature polls.
#interval = 5

# What to do with the fans when the curve stops: "off" or "keep".
#on_exit = "off"

# Exponential smoothing time constant for temperature readings, in seconds.
# Larger values react more slowly to spikes; 0 disables smoothing.
#smooth = 15

# How long (seconds) a lower fan target must persist before slowing down.
# Spin-up is always immediate; this only delays spin-down. 0 disables it.
#down_delay = 30
"#;

pub fn dir() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(xdg).join("padctl");
    }
    let home = std::env::var_os("HOME").unwrap_or_else(|| ".".into());
    PathBuf::from(home).join(".config").join("padctl")
}

pub fn path() -> PathBuf {
    dir().join("config.toml")
}

/// Load the config file if it exists. A file that exists but does not parse
/// is a hard error — silently ignoring it would be worse.
pub fn load() -> Result<Option<Config>> {
    let path = path();
    if !path.exists() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let config: Config =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(config))
}

/// Write the commented template config, refusing to clobber an existing file
/// unless `force` is set.
pub fn init(force: bool) -> Result<PathBuf> {
    let path = path();
    if path.exists() && !force {
        bail!(
            "{} already exists (use --force to overwrite)",
            path.display()
        );
    }
    std::fs::create_dir_all(dir()).with_context(|| format!("creating {}", dir().display()))?;
    std::fs::write(&path, TEMPLATE).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses_to_empty_config() {
        let c: Config = toml::from_str(TEMPLATE).unwrap();
        assert!(c.curve.points.is_none());
        assert!(c.curve.interval.is_none());
    }

    #[test]
    fn parses_populated_config() {
        let c: Config = toml::from_str(
            r#"
            [curve]
            points = "50:0,70:2000"
            interval = 10
            on_exit = "keep"
            smooth = 20
            down_delay = 60
            "#,
        )
        .unwrap();
        assert_eq!(c.curve.points.as_deref(), Some("50:0,70:2000"));
        assert_eq!(c.curve.interval, Some(10));
        assert_eq!(c.curve.on_exit.as_deref(), Some("keep"));
        assert_eq!(c.curve.smooth, Some(20.0));
        assert_eq!(c.curve.down_delay, Some(60));
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(toml::from_str::<Config>("[curve]\nspeed = 3\n").is_err());
        assert!(toml::from_str::<Config>("[fan]\n").is_err());
    }
}
