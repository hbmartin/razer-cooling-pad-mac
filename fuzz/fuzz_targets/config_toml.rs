#![no_main]

use libfuzzer_sys::fuzz_target;

// The whole config pipeline: TOML text -> Config -> resolved curve
// settings + lighting plan, exactly as `padctl curve` consumes it.
fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data)
        && let Ok(cfg) = padctl::config::parse(s)
    {
        let _ = padctl::curve::resolve(&padctl::curve::CurveArgs::default(), Some(&cfg.curve));
        let _ = padctl::lighting::plan(&cfg.lighting);
    }
});
