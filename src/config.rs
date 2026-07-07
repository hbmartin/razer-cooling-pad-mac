//! Optional TOML configuration, shared by interactive runs and the launchd
//! service: `~/.config/padctl/config.toml` (respects `XDG_CONFIG_HOME`).
//! CLI flags always take precedence over config values.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub curve: CurveConfig,
    #[serde(default)]
    pub lighting: LightingConfig,
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

/// Lighting applied at `padctl curve` startup (and on every reconnect), so
/// the launchd service restores the preferred look at login. Also applied
/// on demand with `padctl rgb apply`. See [`crate::lighting`].
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LightingConfig {
    /// "off", "static", "spectrum", "wave", "breath", "gradient" or "custom".
    pub effect: Option<String>,
    /// Colors as 6-hex-digit strings; how many depends on the effect.
    pub colors: Option<Vec<String>>,
    /// Brightness 0-100, applied before the effect.
    pub brightness: Option<u8>,
    /// Wave direction: "left" or "right" (wave effect only).
    pub wave_dir: Option<String>,
    /// Wave speed byte (wave effect only; device default 40).
    pub wave_speed: Option<u8>,
    /// Switch to driver mode before sending custom frames
    /// (gradient/custom effects only).
    pub driver_mode: Option<bool>,
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

# Lighting applied when the fan curve starts (so the launchd service restores
# it at login) and on `padctl rgb apply`. Leave everything commented out to
# leave the lighting alone.
[lighting]
# Effect: "off", "static", "spectrum", "wave", "breath", "gradient", "custom".
#effect = "static"

# Colors as 6-hex-digit strings. static: 1 color; breath: 0-2 colors;
# gradient: 2 colors; custom: 1-18 colors (stretched across the strip).
#colors = ["ff6600"]

# Brightness 0-100, applied before the effect.
#brightness = 80

# Wave options (effect = "wave" only).
#wave_dir = "right"
#wave_speed = 40

# Gradient/custom frames are experimental on this device; enable driver mode
# first if the lighting does not change (normal mode is NOT restored).
#driver_mode = false
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

/// Persist live-tuned curve values into the config file, keeping existing
/// comments and unrelated sections (e.g. `[lighting]`) intact. A missing
/// file is created from the commented template first.
pub fn save_curve_tuning(points_text: &str, smooth: f64, down_delay: u64) -> Result<PathBuf> {
    let path = path();
    save_curve_tuning_to(&path, points_text, smooth, down_delay)?;
    Ok(path)
}

// Path-parameterized so tests can target a scratch directory.
fn save_curve_tuning_to(
    path: &Path,
    points_text: &str,
    smooth: f64,
    down_delay: u64,
) -> Result<()> {
    let text = if path.exists() {
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?
    } else {
        TEMPLATE.to_string()
    };
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("parsing {}", path.display()))?;
    doc["curve"]["points"] = toml_edit::value(points_text);
    doc["curve"]["smooth"] = toml_edit::value(smooth);
    doc["curve"]["down_delay"] = toml_edit::value(down_delay as i64);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(path, doc.to_string()).with_context(|| format!("writing {}", path.display()))
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
    fn parses_lighting_section() {
        let c: Config = toml::from_str(
            r#"
            [lighting]
            effect = "wave"
            brightness = 80
            wave_dir = "left"
            wave_speed = 40
            "#,
        )
        .unwrap();
        assert_eq!(c.lighting.effect.as_deref(), Some("wave"));
        assert_eq!(c.lighting.brightness, Some(80));
        assert_eq!(c.lighting.wave_dir.as_deref(), Some("left"));
        assert_eq!(c.lighting.wave_speed, Some(40));
        assert_eq!(c.lighting.driver_mode, None);
    }

    /// A scratch file path that is cleaned up when dropped.
    struct ScratchFile(PathBuf);

    impl ScratchFile {
        fn new(test: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("padctl-test-{}-{test}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            ScratchFile(dir.join("config.toml"))
        }
    }

    impl Drop for ScratchFile {
        fn drop(&mut self) {
            if let Some(dir) = self.0.parent() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }

    #[test]
    fn save_creates_file_from_template_and_reloads() {
        let scratch = ScratchFile::new("save-creates");
        save_curve_tuning_to(&scratch.0, "50:800,80:3000", 12.0, 45).unwrap();

        let text = std::fs::read_to_string(&scratch.0).unwrap();
        // Template comments survive as documentation.
        assert!(text.contains("# padctl configuration."));
        let c: Config = toml::from_str(&text).unwrap();
        assert_eq!(c.curve.points.as_deref(), Some("50:800,80:3000"));
        assert_eq!(c.curve.smooth, Some(12.0));
        assert_eq!(c.curve.down_delay, Some(45));
    }

    #[test]
    fn save_preserves_existing_lighting_and_comments() {
        let scratch = ScratchFile::new("save-preserves");
        std::fs::write(
            &scratch.0,
            "# my precious comment\n[curve]\ninterval = 10\n\n[lighting]\neffect = \"wave\"\n",
        )
        .unwrap();
        save_curve_tuning_to(&scratch.0, "60:1000", 0.0, 0).unwrap();

        let text = std::fs::read_to_string(&scratch.0).unwrap();
        assert!(text.contains("# my precious comment"));
        let c: Config = toml::from_str(&text).unwrap();
        assert_eq!(c.curve.points.as_deref(), Some("60:1000"));
        assert_eq!(c.curve.smooth, Some(0.0));
        assert_eq!(c.curve.down_delay, Some(0));
        assert_eq!(c.curve.interval, Some(10)); // untouched key survives
        assert_eq!(c.lighting.effect.as_deref(), Some("wave"));
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(toml::from_str::<Config>("[curve]\nspeed = 3\n").is_err());
        assert!(toml::from_str::<Config>("[fan]\n").is_err());
        assert!(toml::from_str::<Config>("[lighting]\ncolour = \"red\"\n").is_err());
    }
}
