//! ODE Artwork Downloader
//!
//! A cross-platform GUI application that automatically identifies CD/DVD disc images
//! and downloads appropriate cover art for the USBODE project.

use eframe::egui;

mod disc;
mod gui;

fn main() -> eframe::Result<()> {
    env_logger::init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 600.0])
            .with_min_inner_size([640.0, 480.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };

    eframe::run_native(
        "ODE Artwork Downloader",
        options,
        Box::new(|cc| Ok(Box::new(gui::App::new(cc)))),
    )
}
