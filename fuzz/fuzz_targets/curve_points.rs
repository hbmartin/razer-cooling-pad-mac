#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data)
        && let Ok(points) = padctl::curve::parse_points(s)
    {
        // Anything that parses must also interpolate without panicking.
        for temp in [-40.0, 0.0, 55.5, 85.0, 200.0] {
            let _ = padctl::curve::target_rpm(&points, temp);
        }
    }
});
