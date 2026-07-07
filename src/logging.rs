//! Minimal timestamped logger on top of the `log` facade, writing to stderr.
//!
//! One mechanism serves both interactive runs and the launchd service:
//! `-v` raises the level to Debug (per-poll curve decisions, raw packet
//! dumps); the default Info level keeps unattended logs quiet.

use log::{Level, LevelFilter, Log, Metadata, Record};

struct Logger;

static LOGGER: Logger = Logger;

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let ts = chrono::Local::now().format("%H:%M:%S");
        match record.level() {
            Level::Error => eprintln!("[{ts}] error: {}", record.args()),
            Level::Warn => eprintln!("[{ts}] warning: {}", record.args()),
            Level::Info => eprintln!("[{ts}] {}", record.args()),
            Level::Debug | Level::Trace => eprintln!("[{ts}] debug: {}", record.args()),
        }
    }

    fn flush(&self) {}
}

pub fn init(verbose: bool) {
    let level = if verbose {
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    };
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(level);
    }
}
