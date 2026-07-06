//! CLI argument parsing helpers, kept out of main.rs so they can be tested.

use anyhow::{Context, Result, bail};

use crate::fan;

/// A parsed fan speed argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speed {
    /// Fans off (`off`, `0`, or `0%`).
    Off,
    /// A concrete RPM within the device's supported range.
    Rpm(u32),
}

/// Parse `off`, an RPM value (500-3200), or a percentage like `60%`.
/// `0` and `0%` mean off, matching how the curve treats sub-minimum targets.
pub fn parse_speed(s: &str) -> Result<Speed> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("off") {
        return Ok(Speed::Off);
    }
    if let Some(pct) = s.strip_suffix('%') {
        let pct: u32 = pct.trim().parse().context("invalid percentage")?;
        if pct > 100 {
            bail!("percentage must be 0-100");
        }
        if pct == 0 {
            return Ok(Speed::Off);
        }
        return Ok(Speed::Rpm(fan::percent_to_rpm(pct)));
    }
    let rpm: u32 = s.parse().context("invalid RPM value")?;
    if rpm == 0 {
        return Ok(Speed::Off);
    }
    if !(fan::MIN_RPM..=fan::MAX_RPM).contains(&rpm) {
        bail!(
            "RPM must be 0 (off) or {}-{} (or use a percentage like 60%)",
            fan::MIN_RPM,
            fan::MAX_RPM
        );
    }
    Ok(Speed::Rpm(rpm))
}

/// Parse a 6-hex-digit color like `ff6600` (leading `#` allowed).
pub fn parse_color(s: &str) -> Result<(u8, u8, u8)> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("color must be 6 hex digits like ff6600");
    }
    let v = u32::from_str_radix(s, 16).unwrap();
    Ok(((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// Decode a hex string (whitespace already stripped) into bytes.
pub fn hex_decode(s: &str) -> Result<Vec<u8>> {
    if s.is_empty() || !s.len().is_multiple_of(2) || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("expected an even number of hex digits");
    }
    Ok((0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_off_forms() {
        assert_eq!(parse_speed("off").unwrap(), Speed::Off);
        assert_eq!(parse_speed("OFF").unwrap(), Speed::Off);
        assert_eq!(parse_speed("0").unwrap(), Speed::Off);
        assert_eq!(parse_speed("0%").unwrap(), Speed::Off);
    }

    #[test]
    fn speed_rpm_and_percent() {
        assert_eq!(parse_speed("1500").unwrap(), Speed::Rpm(1500));
        assert_eq!(parse_speed(" 3200 ").unwrap(), Speed::Rpm(3200));
        assert_eq!(parse_speed("100%").unwrap(), Speed::Rpm(fan::MAX_RPM));
        // 50% maps onto the middle of the supported range.
        assert_eq!(parse_speed("50%").unwrap(), Speed::Rpm(1850));
    }

    #[test]
    fn speed_rejects_out_of_range() {
        assert!(parse_speed("400").is_err());
        assert!(parse_speed("3300").is_err());
        assert!(parse_speed("101%").is_err());
        assert!(parse_speed("fast").is_err());
        assert!(parse_speed("").is_err());
    }

    #[test]
    fn color_parses_with_and_without_hash() {
        assert_eq!(parse_color("ff6600").unwrap(), (0xFF, 0x66, 0x00));
        assert_eq!(parse_color("#00ff00").unwrap(), (0x00, 0xFF, 0x00));
        assert!(parse_color("ff660").is_err());
        assert!(parse_color("gg6600").is_err());
        assert!(parse_color("").is_err());
    }

    #[test]
    fn hex_decode_bytes() {
        assert_eq!(hex_decode("001f").unwrap(), vec![0x00, 0x1F]);
        assert!(hex_decode("0").is_err());
        assert!(hex_decode("").is_err());
        assert!(hex_decode("zz").is_err());
    }
}
