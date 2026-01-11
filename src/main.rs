//! ODE Artwork Downloader
//!
//! A cross-platform GUI application that automatically identifies CD/DVD disc images
//! and downloads appropriate cover art for the USBODE project.

use eframe::egui;

mod api;
mod disc;
mod export;
mod gui;
mod search;

fn main() -> eframe::Result<()> {
    env_logger::init();

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
            .with_inner_size([800.0, 600.0])
            .with_min_inner_size([640.0, 480.0])
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
