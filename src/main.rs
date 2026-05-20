//! ODE Artwork Downloader
//!
//! A cross-platform GUI application that automatically identifies CD/DVD disc images
//! and downloads appropriate cover art for the USBODE project.

// Hide console window on Windows release builds
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;

use ode_artwork_downloader::{gui, logging};

fn main() -> eframe::Result<()> {
    // Initialize the UI logger using the configured level (defaults to "info").
    let initial_level =
        logging::ui_logger::parse_level(&ode_artwork_downloader::config::get_config().log_level);
    let log_receiver = logging::ui_logger::UiLogger::init(initial_level)
        .expect("Failed to initialize logger");

    // Store the receiver so the App can take it during initialization
    gui::set_log_receiver(log_receiver);

    // Load icon from bytes with transparency preserved
    let icon_bytes = include_bytes!("../assets/icons/icon-256.png");
    let icon_image = image::load_from_memory_with_format(icon_bytes, image::ImageFormat::Png)
        .expect("Failed to load icon");
    
    // Ensure we have RGBA with alpha channel
    let icon_rgba = icon_image.to_rgba8();
    let (icon_width, icon_height) = icon_rgba.dimensions();
    
    let icon_data = egui::IconData {
        rgba: icon_rgba.into_raw(),
        width: icon_width,
        height: icon_height,
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 720.0])
            .with_min_inner_size([720.0, 540.0])
            .with_drag_and_drop(true)
            .with_icon(icon_data),
        ..Default::default()
    };

    eframe::run_native(
        "ODE Artwork Downloader",
        options,
        Box::new(|cc| Ok(Box::new(gui::App::new(cc)))),
    )
}
