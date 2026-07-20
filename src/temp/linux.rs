//! Linux CPU temperature reading via sysfs thermal zones
//! (`/sys/class/thermal/thermal_zone*/temp`, millidegrees Celsius).
//!
//! macOS is the primary target; this exists so the whole crate builds,
//! lints, and tests on Linux, and it makes the curve usable there too.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::Reading;

pub struct Reader {
    zones: Vec<Zone>,
}

struct Zone {
    name: String,
    temp_path: PathBuf,
}

/// Zone types that look like CPU/package sensors rather than ambient,
/// battery, or radio sensors.
fn is_cpu_zone(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["cpu", "pkg", "core", "soc", "x86", "tctl", "tdie"]
        .iter()
        .any(|k| name.contains(k))
}

fn parse_millideg(s: &str) -> Result<f64> {
    let raw: i64 = s.trim().parse().context("parsing thermal zone value")?;
    Ok(raw as f64 / 1000.0)
}

fn plausible_temp(celsius: f64) -> bool {
    (-40.0..=150.0).contains(&celsius)
}

fn read_zone_temp(path: &Path) -> Option<f64> {
    let celsius = parse_millideg(&fs::read_to_string(path).ok()?).ok()?;
    plausible_temp(celsius).then_some(celsius)
}

impl Reader {
    pub fn new() -> Result<Self> {
        let mut zones = Vec::new();
        let base = PathBuf::from("/sys/class/thermal");
        let entries = fs::read_dir(&base)
            .with_context(|| format!("reading {} (no thermal zones?)", base.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("thermal_zone"))
            {
                continue;
            }
            let temp_path = path.join("temp");
            let name = fs::read_to_string(path.join("type"))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| entry.file_name().to_string_lossy().into_owned());
            // Only keep zones that are actually readable right now.
            if read_zone_temp(&temp_path).is_some() {
                zones.push(Zone { name, temp_path });
            }
        }
        if zones.is_empty() {
            bail!("no readable thermal zones under /sys/class/thermal");
        }
        zones.sort_by(|a, b| a.temp_path.cmp(&b.temp_path));
        Ok(Reader { zones })
    }

    pub fn read(&self) -> Result<f64> {
        let read_zone = |z: &Zone| read_zone_temp(&z.temp_path);
        let cpu: Vec<f64> = self
            .zones
            .iter()
            .filter(|z| is_cpu_zone(&z.name))
            .filter_map(read_zone)
            .collect();
        let temps: Vec<f64> = if cpu.is_empty() {
            self.zones.iter().filter_map(read_zone).collect()
        } else {
            cpu
        };
        if temps.is_empty() {
            bail!("all thermal zones became unreadable");
        }
        Ok(temps.iter().sum::<f64>() / temps.len() as f64)
    }

    pub fn source_name(&self) -> &'static str {
        "sysfs thermal zones"
    }

    pub fn is_fallback(&self) -> bool {
        false
    }

    pub fn sensors(&self) -> Result<Vec<Reading>> {
        let readings: Vec<Reading> = self
            .zones
            .iter()
            .filter_map(|z| {
                let celsius = read_zone_temp(&z.temp_path)?;
                Some(Reading {
                    name: z.name.clone(),
                    celsius,
                })
            })
            .collect();
        if readings.is_empty() {
            bail!("all thermal zones became unreadable");
        }
        Ok(readings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn millideg_parsing() {
        assert_eq!(parse_millideg("45000\n").unwrap(), 45.0);
        assert_eq!(parse_millideg("-5000").unwrap(), -5.0);
        assert!(parse_millideg("hot").is_err());
    }

    #[test]
    fn plausibility_bounds() {
        assert!(plausible_temp(-40.0));
        assert!(plausible_temp(80.0));
        assert!(plausible_temp(150.0));
        assert!(!plausible_temp(-40.1));
        assert!(!plausible_temp(150.1));
        assert!(!plausible_temp(f64::NAN));
    }

    #[test]
    fn cpu_zone_detection() {
        assert!(is_cpu_zone("x86_pkg_temp"));
        assert!(is_cpu_zone("cpu-thermal"));
        assert!(is_cpu_zone("TCPU"));
        assert!(is_cpu_zone("Tctl"));
        assert!(!is_cpu_zone("acpitz"));
        assert!(!is_cpu_zone("iwlwifi_1"));
        assert!(!is_cpu_zone("battery"));
    }
}
