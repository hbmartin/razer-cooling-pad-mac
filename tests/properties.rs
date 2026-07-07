//! Property tests for the parsing surfaces: anything that consumes
//! user-typed strings, config text, or raw device bytes must never panic,
//! and its outputs must hold a few structural invariants. The cargo-fuzz
//! targets under `fuzz/` hammer the same functions with coverage-guided
//! input; these run on every `cargo test`.

use padctl::packet::{Packet, REPORT_LEN, Response, crc};
use padctl::{config, curve, fan, lighting, parse, rgb};
use proptest::prelude::*;

proptest! {
    // ---- parse::parse_speed ----

    #[test]
    fn parse_speed_never_panics(s in "\\PC*") {
        let _ = parse::parse_speed(&s);
    }

    #[test]
    fn parse_speed_accepts_every_valid_rpm(rpm in fan::MIN_RPM..=fan::MAX_RPM) {
        prop_assert_eq!(
            parse::parse_speed(&rpm.to_string()).unwrap(),
            parse::Speed::Rpm(rpm)
        );
    }

    #[test]
    fn parse_speed_percent_maps_into_device_range(pct in 1u32..=100) {
        match parse::parse_speed(&format!("{pct}%")).unwrap() {
            parse::Speed::Rpm(rpm) => {
                prop_assert!((fan::MIN_RPM..=fan::MAX_RPM).contains(&rpm));
                prop_assert_eq!(rpm % fan::RPM_STEP, 0);
            }
            parse::Speed::Off => prop_assert!(false, "non-zero percent parsed as off"),
        }
    }

    // ---- parse::parse_color ----

    #[test]
    fn parse_color_never_panics(s in "\\PC*") {
        let _ = parse::parse_color(&s);
    }

    #[test]
    fn parse_color_round_trips(r: u8, g: u8, b: u8) {
        let plain = format!("{r:02x}{g:02x}{b:02x}");
        prop_assert_eq!(parse::parse_color(&plain).unwrap(), (r, g, b));
        prop_assert_eq!(parse::parse_color(&format!("#{plain}")).unwrap(), (r, g, b));
        prop_assert_eq!(parse::parse_color(&plain.to_uppercase()).unwrap(), (r, g, b));
    }

    // ---- parse::hex_decode ----

    #[test]
    fn hex_decode_never_panics(s in "\\PC*") {
        let _ = parse::hex_decode(&s);
    }

    #[test]
    fn hex_decode_round_trips(bytes in proptest::collection::vec(any::<u8>(), 1..90)) {
        let text: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        prop_assert_eq!(parse::hex_decode(&text).unwrap(), bytes);
    }

    // ---- curve::parse_points / target_rpm ----

    #[test]
    fn parse_points_never_panics(s in "\\PC*") {
        let _ = curve::parse_points(&s);
    }

    #[test]
    fn valid_points_parse_and_sort(
        pts in proptest::collection::vec(
            (0.0f64..120.0, prop_oneof![Just(0u32), fan::MIN_RPM..=fan::MAX_RPM]),
            1..8,
        )
    ) {
        let text = pts
            .iter()
            .map(|(t, r)| format!("{t}:{r}"))
            .collect::<Vec<_>>()
            .join(",");
        let parsed = curve::parse_points(&text).unwrap();
        prop_assert_eq!(parsed.len(), pts.len());
        prop_assert!(parsed.windows(2).all(|w| w[0].0 <= w[1].0), "points not sorted");
    }

    #[test]
    fn target_rpm_stays_within_curve_bounds(
        pts in proptest::collection::vec(
            (0.0f64..120.0, prop_oneof![Just(0u32), fan::MIN_RPM..=fan::MAX_RPM]),
            1..8,
        ),
        temp in -50.0f64..200.0,
    ) {
        let text = pts
            .iter()
            .map(|(t, r)| format!("{t}:{r}"))
            .collect::<Vec<_>>()
            .join(",");
        let parsed = curve::parse_points(&text).unwrap();
        let target = curve::target_rpm(&parsed, temp);
        let lo = parsed.iter().map(|p| p.1).min().unwrap();
        let hi = parsed.iter().map(|p| p.1).max().unwrap();
        prop_assert!((lo..=hi).contains(&target), "{target} outside {lo}..={hi}");
    }

    // ---- packet framing ----

    #[test]
    fn packet_report_layout_and_crc(
        tid: u8,
        class: u8,
        cmd: u8,
        data_size: u8,
        args in proptest::collection::vec(any::<u8>(), 0..=80),
    ) {
        let report = Packet::new(tid, class, cmd, data_size, &args).to_report();
        prop_assert_eq!(report[0], 0, "report id must be 0");
        prop_assert_eq!(report[2], tid);
        prop_assert_eq!(report[7], class);
        prop_assert_eq!(report[8], cmd);
        prop_assert_eq!(report[89], crc(&report[1..]), "stored crc must match");
        prop_assert_eq!(report[90], 0, "trailing byte must be 0");
    }

    #[test]
    fn response_parses_any_report(bytes in proptest::collection::vec(any::<u8>(), REPORT_LEN)) {
        let report: [u8; REPORT_LEN] = bytes.try_into().unwrap();
        let resp = Response::from_report(&report);
        prop_assert_eq!(resp.transaction_id, report[2]);
        prop_assert_eq!(resp.data_size, report[6]);
        prop_assert_eq!(resp.class, report[7]);
        prop_assert_eq!(resp.cmd, report[8]);
        prop_assert_eq!(&resp.args[..], &report[9..89]);
        // Derived views of the same bytes must not panic either.
        let serial = rgb::serial_text(&resp);
        prop_assert!(serial.chars().all(|c| c.is_ascii_graphic()));
        let _ = fan::rpm_from_report(&report);
    }

    // ---- rgb frame helpers ----

    #[test]
    fn stretch_fills_strip_from_input_colors(
        colors in proptest::collection::vec(any::<(u8, u8, u8)>(), 1..=rgb::NUM_LEDS)
    ) {
        let frame = rgb::stretch(&colors, rgb::NUM_LEDS);
        prop_assert_eq!(frame.len(), rgb::NUM_LEDS);
        prop_assert!(frame.iter().all(|c| colors.contains(c)));
        // Full frames pass through unchanged.
        if colors.len() == rgb::NUM_LEDS {
            prop_assert_eq!(frame, colors);
        }
    }

    #[test]
    fn gradient_hits_both_endpoints(
        from in any::<(u8, u8, u8)>(),
        to in any::<(u8, u8, u8)>(),
        n in 1usize..=64,
    ) {
        let g = rgb::gradient(from, to, n);
        prop_assert_eq!(g.len(), n);
        prop_assert_eq!(g[0], from);
        if n > 1 {
            prop_assert_eq!(g[n - 1], to);
        }
    }

    // ---- config / lighting ----

    #[test]
    fn config_parse_never_panics(s in "\\PC{0,400}") {
        let _ = config::parse(&s);
    }

    #[test]
    fn lighting_plan_never_panics(
        effect in proptest::option::of("\\PC{0,12}"),
        colors in proptest::option::of(proptest::collection::vec("\\PC{0,8}", 0..4)),
        brightness in proptest::option::of(any::<u8>()),
        wave_dir in proptest::option::of("\\PC{0,8}"),
        wave_speed in proptest::option::of(any::<u8>()),
        driver_mode in proptest::option::of(any::<bool>()),
    ) {
        let cfg = config::LightingConfig {
            effect,
            colors,
            brightness,
            wave_dir,
            wave_speed,
            driver_mode,
        };
        if let Ok(Some(plan)) = lighting::plan(&cfg) {
            prop_assert!(!plan.packets.is_empty());
            prop_assert!(!plan.summary.is_empty());
        }
    }
}
