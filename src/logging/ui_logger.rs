use log::{Log, Metadata, Record, SetLoggerError, LevelFilter};
use std::sync::mpsc::{self, Sender, Receiver};
use std::str::FromStr;

/// Simple logger that forwards formatted log lines into an mpsc channel.
pub struct UiLogger {
    sender: Sender<String>,
    max_level: LevelFilter,
}

impl UiLogger {
    /// Install the UI logger and return the receiver to read log lines from.
    /// If `RUST_LOG` is not set this will set it to `debug` so the UI shows verbose logs.
    pub fn init() -> Result<Receiver<String>, SetLoggerError> {
        if std::env::var("RUST_LOG").is_err() {
            std::env::set_var("RUST_LOG", "debug");
        }

        let max_level = std::env::var("RUST_LOG")
            .ok()
            .and_then(|s| LevelFilter::from_str(&s).ok())
            .unwrap_or(LevelFilter::Debug);

        let (tx, rx) = mpsc::channel();
        let logger = UiLogger {
            sender: tx,
            max_level,
        };

        log::set_boxed_logger(Box::new(logger))?;
        log::set_max_level(max_level);
        Ok(rx)
    }
}

impl Log for UiLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.max_level
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
