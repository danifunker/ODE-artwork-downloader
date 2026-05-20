use log::{Log, Metadata, Record, SetLoggerError, LevelFilter};
use std::str::FromStr;
use std::sync::mpsc::{self, Receiver, Sender};

/// Parse a `LevelFilter` from a config string, falling back to `Info` for
/// anything we don't recognize.
pub fn parse_level(s: &str) -> LevelFilter {
    LevelFilter::from_str(s).unwrap_or(LevelFilter::Info)
}

/// Simple logger that forwards formatted log lines into an mpsc channel.
/// The active filter is controlled by `log::set_max_level`, so it can be
/// updated at runtime without reinstalling the logger.
pub struct UiLogger {
    sender: Sender<String>,
}

impl UiLogger {
    /// Install the UI logger with the given initial max level and return the
    /// receiver to read log lines from.
    pub fn init(initial_level: LevelFilter) -> Result<Receiver<String>, SetLoggerError> {
        let (tx, rx) = mpsc::channel();
        let logger = UiLogger { sender: tx };
        log::set_boxed_logger(Box::new(logger))?;
        log::set_max_level(initial_level);
        Ok(rx)
    }
}

impl Log for UiLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let mut msg = format!("[{}] {}: {}", record.level(), record.target(), record.args());
            if let (Some(file), Some(line)) = (record.file(), record.line()) {
                msg.push_str(&format!(" ({}:{})", file, line));
            }
            let _ = self.sender.send(msg);
        }
    }

    fn flush(&self) {}
}
