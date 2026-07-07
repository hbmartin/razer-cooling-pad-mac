#![no_main]

use libfuzzer_sys::fuzz_target;
use padctl::packet::{REPORT_LEN, Response};

// Device bytes are untrusted input too: whatever the pad (or a lookalike)
// answers must parse without panicking.
fuzz_target!(|data: &[u8]| {
    let mut report = [0u8; REPORT_LEN];
    let n = data.len().min(REPORT_LEN);
    report[..n].copy_from_slice(&data[..n]);
    let resp = Response::from_report(&report);
    let _ = padctl::rgb::serial_text(&resp);
    let _ = padctl::fan::rpm_from_report(&report);
});
