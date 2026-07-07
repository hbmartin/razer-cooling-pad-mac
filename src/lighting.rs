//! Turn the `[lighting]` config section into device packets, shared by
//! `padctl rgb apply` and the fan-curve startup/reconnect path so the
//! launchd service can restore the preferred lighting at login.

use anyhow::{Result, bail};

use crate::config::LightingConfig;
use crate::packet::Packet;
use crate::parse;
use crate::rgb;

/// A validated lighting configuration, ready to send.
pub struct Plan {
    pub packets: Vec<Packet>,
    /// Human-readable description, e.g. `brightness 80%, static #ff6600`.
    pub summary: String,
}

/// Validate `cfg` and build the packets it describes. `Ok(None)` means the
/// section is empty and the lighting should be left alone.
pub fn plan(cfg: &LightingConfig) -> Result<Option<Plan>> {
    let mut packets = Vec::new();
    let mut parts = Vec::new();

    if let Some(pct) = cfg.brightness {
        if pct > 100 {
            bail!("lighting brightness must be 0-100, got {pct}");
        }
        packets.push(rgb::brightness_set((pct as u16 * 255 / 100) as u8));
        parts.push(format!("brightness {pct}%"));
    }

    let colors: Vec<rgb::Rgb> = cfg
        .colors
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|c| parse::parse_color(c))
        .collect::<Result<_>>()?;
    let color_text = |c: rgb::Rgb| format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2);

    let effect = cfg.effect.as_deref();
    let is_frame_effect = matches!(effect, Some("gradient" | "custom"));
    if !matches!(effect, Some("wave")) && (cfg.wave_dir.is_some() || cfg.wave_speed.is_some()) {
        bail!("lighting wave_dir/wave_speed only apply to effect = \"wave\"");
    }
    if !is_frame_effect && cfg.driver_mode.is_some() {
        bail!("lighting driver_mode only applies to effect = \"gradient\" or \"custom\"");
    }

    match effect {
        None => {
            if !colors.is_empty() {
                bail!("lighting colors need an effect (e.g. effect = \"static\")");
            }
        }
        Some("off") => {
            expect_colors(&colors, 0, "off")?;
            packets.push(rgb::off());
            parts.push("off".into());
        }
        Some("static") => {
            expect_colors(&colors, 1, "static")?;
            let (r, g, b) = colors[0];
            packets.push(rgb::static_color(r, g, b));
            parts.push(format!("static {}", color_text(colors[0])));
        }
        Some("spectrum") => {
            expect_colors(&colors, 0, "spectrum")?;
            packets.push(rgb::spectrum());
            parts.push("spectrum".into());
        }
        Some("wave") => {
            expect_colors(&colors, 0, "wave")?;
            let dir = match cfg.wave_dir.as_deref() {
                None | Some("right") => rgb::WaveDirection::Right,
                Some("left") => rgb::WaveDirection::Left,
                Some(other) => {
                    bail!("lighting wave_dir must be \"left\" or \"right\", got {other:?}")
                }
            };
            let speed = cfg.wave_speed.unwrap_or(rgb::DEFAULT_WAVE_SPEED);
            packets.push(rgb::wave(dir, speed));
            parts.push(format!("wave ({dir:?}, speed {speed})"));
        }
        Some("breath") => {
            let packet = match colors.len() {
                0 => rgb::breath_random(),
                1 => rgb::breath_single(colors[0].0, colors[0].1, colors[0].2),
                2 => rgb::breath_dual(colors[0], colors[1]),
                n => bail!("breath takes 0, 1 or 2 colors, got {n}"),
            };
            packets.push(packet);
            parts.push("breath".into());
        }
        Some("gradient") => {
            expect_colors(&colors, 2, "gradient")?;
            let frame = rgb::gradient(colors[0], colors[1], rgb::NUM_LEDS);
            push_frame(&mut packets, cfg, &frame);
            parts.push(format!(
                "gradient {}→{}",
                color_text(colors[0]),
                color_text(colors[1])
            ));
        }
        Some("custom") => {
            if colors.is_empty() || colors.len() > rgb::NUM_LEDS {
                bail!(
                    "custom takes 1-{} colors, got {}",
                    rgb::NUM_LEDS,
                    colors.len()
                );
            }
            let frame = rgb::stretch(&colors, rgb::NUM_LEDS);
            push_frame(&mut packets, cfg, &frame);
            parts.push(format!("custom frame ({} colors)", colors.len()));
        }
        Some(other) => bail!(
            "unknown lighting effect {other:?}: use off, static, spectrum, wave, \
             breath, gradient or custom"
        ),
    }

    if packets.is_empty() {
        return Ok(None);
    }
    Ok(Some(Plan {
        packets,
        summary: parts.join(", "),
    }))
}

fn expect_colors(colors: &[rgb::Rgb], want: usize, effect: &str) -> Result<()> {
    if colors.len() != want {
        bail!(
            "{effect} takes {} color{}, got {}",
            if want == 0 {
                "no".to_string()
            } else {
                want.to_string()
            },
            if want == 1 { "" } else { "s" },
            colors.len()
        );
    }
    Ok(())
}

fn push_frame(packets: &mut Vec<Packet>, cfg: &LightingConfig, frame: &[rgb::Rgb]) {
    if cfg.driver_mode == Some(true) {
        packets.push(rgb::device_mode(0x03));
    }
    packets.push(rgb::custom_frame(0, frame));
    packets.push(rgb::custom_apply());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> LightingConfig {
        LightingConfig::default()
    }

    #[test]
    fn empty_section_is_none() {
        assert!(plan(&cfg()).unwrap().is_none());
    }

    #[test]
    fn brightness_only() {
        let p = plan(&LightingConfig {
            brightness: Some(80),
            ..cfg()
        })
        .unwrap()
        .unwrap();
        assert_eq!(p.packets.len(), 1);
        assert_eq!(p.packets[0], rgb::brightness_set(204));
        assert_eq!(p.summary, "brightness 80%");
    }

    #[test]
    fn static_with_brightness_orders_brightness_first() {
        let p = plan(&LightingConfig {
            effect: Some("static".into()),
            colors: Some(vec!["ff6600".into()]),
            brightness: Some(100),
            ..cfg()
        })
        .unwrap()
        .unwrap();
        assert_eq!(p.packets.len(), 2);
        assert_eq!(p.packets[0], rgb::brightness_set(255));
        assert_eq!(p.packets[1], rgb::static_color(0xFF, 0x66, 0x00));
        assert_eq!(p.summary, "brightness 100%, static #ff6600");
    }

    #[test]
    fn wave_defaults_and_options() {
        let p = plan(&LightingConfig {
            effect: Some("wave".into()),
            ..cfg()
        })
        .unwrap()
        .unwrap();
        assert_eq!(
            p.packets,
            vec![rgb::wave(
                rgb::WaveDirection::Right,
                rgb::DEFAULT_WAVE_SPEED
            )]
        );

        let p = plan(&LightingConfig {
            effect: Some("wave".into()),
            wave_dir: Some("left".into()),
            wave_speed: Some(16),
            ..cfg()
        })
        .unwrap()
        .unwrap();
        assert_eq!(p.packets, vec![rgb::wave(rgb::WaveDirection::Left, 16)]);
    }

    #[test]
    fn breath_color_counts() {
        for (colors, expect) in [
            (vec![], rgb::breath_random()),
            (vec!["0000ff".into()], rgb::breath_single(0, 0, 0xFF)),
            (
                vec!["0000ff".into(), "ff0000".into()],
                rgb::breath_dual((0, 0, 0xFF), (0xFF, 0, 0)),
            ),
        ] {
            let p = plan(&LightingConfig {
                effect: Some("breath".into()),
                colors: Some(colors),
                ..cfg()
            })
            .unwrap()
            .unwrap();
            assert_eq!(p.packets, vec![expect]);
        }
    }

    #[test]
    fn gradient_builds_frame_with_optional_driver_mode() {
        let base = LightingConfig {
            effect: Some("gradient".into()),
            colors: Some(vec!["000000".into(), "ffffff".into()]),
            ..cfg()
        };
        let p = plan(&base).unwrap().unwrap();
        assert_eq!(p.packets.len(), 2); // frame + apply

        let p = plan(&LightingConfig {
            driver_mode: Some(true),
            ..base
        })
        .unwrap()
        .unwrap();
        assert_eq!(p.packets.len(), 3);
        assert_eq!(p.packets[0], rgb::device_mode(0x03));
    }

    #[test]
    fn rejects_invalid_configs() {
        // Wrong color counts.
        assert!(
            plan(&LightingConfig {
                effect: Some("static".into()),
                ..cfg()
            })
            .is_err()
        );
        assert!(
            plan(&LightingConfig {
                effect: Some("spectrum".into()),
                colors: Some(vec!["ff0000".into()]),
                ..cfg()
            })
            .is_err()
        );
        // Colors without an effect.
        assert!(
            plan(&LightingConfig {
                colors: Some(vec!["ff0000".into()]),
                ..cfg()
            })
            .is_err()
        );
        // Unknown effect / bad values.
        assert!(
            plan(&LightingConfig {
                effect: Some("disco".into()),
                ..cfg()
            })
            .is_err()
        );
        assert!(
            plan(&LightingConfig {
                brightness: Some(101),
                ..cfg()
            })
            .is_err()
        );
        // Options that don't apply to the chosen effect.
        assert!(
            plan(&LightingConfig {
                effect: Some("static".into()),
                colors: Some(vec!["ff0000".into()]),
                wave_speed: Some(40),
                ..cfg()
            })
            .is_err()
        );
        assert!(
            plan(&LightingConfig {
                effect: Some("spectrum".into()),
                driver_mode: Some(true),
                ..cfg()
            })
            .is_err()
        );
    }
}
