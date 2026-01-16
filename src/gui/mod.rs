//! GUI module using egui/eframe
//!
//! Provides the graphical user interface for the ODE Artwork Downloader.

mod app;
pub mod browse_view;
pub mod hex_view;
pub mod text_view;

pub use app::App;
pub use browse_view::BrowseView;

use std::sync::mpsc::Receiver;
use std::sync::Mutex;

/// Global storage for the log receiver, used to transfer from main() to App::new()
static LOG_RECEIVER: Mutex<Option<Receiver<String>>> = Mutex::new(None);

/// Store the log receiver for the App to take during initialization
pub fn set_log_receiver(receiver: Receiver<String>) {
    if let Ok(mut guard) = LOG_RECEIVER.lock() {
        *guard = Some(receiver);
    }
}

/// Take the log receiver (called once by App::new)
pub(crate) fn take_log_receiver() -> Option<Receiver<String>> {
    LOG_RECEIVER.lock().ok().and_then(|mut guard| guard.take())
}
