//! Hex viewer widget for displaying binary data

use eframe::egui;

/// Hex viewer widget
pub struct HexView {
    /// Number of bytes per line
    bytes_per_line: usize,
}

impl Default for HexView {
    fn default() -> Self {
        Self { bytes_per_line: 16 }
    }
}

impl HexView {
    /// Create a new hex viewer
    pub fn new() -> Self {
        Self::default()
    }

    /// Render the hex view for the given data
    pub fn show(&self, ui: &mut egui::Ui, data: &[u8], offset: u64) {
        let font_id = egui::FontId::monospace(12.0);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Limit to reasonable number of lines for performance
                let max_lines = (data.len() + self.bytes_per_line - 1) / self.bytes_per_line;
                let lines_to_show = max_lines.min(10000);

                for line_idx in 0..lines_to_show {
                    let line_start = line_idx * self.bytes_per_line;
                    let line_end = (line_start + self.bytes_per_line).min(data.len());

                    if line_start >= data.len() {
                        break;
                    }

                    let line_data = &data[line_start..line_end];
                    let line_offset = offset + line_start as u64;

                    ui.horizontal(|ui| {
                        // Offset column
                        ui.label(
                            egui::RichText::new(format!("{:08X}", line_offset))
                                .font(font_id.clone())
                                .color(egui::Color32::from_rgb(128, 128, 128)),
                        );

                        ui.add_space(12.0);

                        // Hex bytes with gap at 8 bytes
                        let mut hex_str = String::with_capacity(self.bytes_per_line * 3 + 2);
                        for (i, byte) in line_data.iter().enumerate() {
                            if i == 8 {
                                hex_str.push(' ');
                            }
                            hex_str.push_str(&format!("{:02X} ", byte));
                        }
                        // Pad if line is incomplete
                        let missing = self.bytes_per_line - line_data.len();
                        for i in 0..missing {
                            if line_data.len() + i == 8 {
                                hex_str.push(' ');
                            }
                            hex_str.push_str("   ");
                        }

                        ui.label(egui::RichText::new(hex_str).font(font_id.clone()));

                        ui.add_space(8.0);

                        // ASCII representation
                        let ascii: String = line_data
                            .iter()
                            .map(|&b| {
                                if b.is_ascii_graphic() || b == b' ' {
                                    b as char
                                } else {
                                    '.'
                                }
                            })
                            .collect();

                        ui.label(
                            egui::RichText::new(ascii)
                                .font(font_id.clone())
                                .color(egui::Color32::from_rgb(100, 149, 237)), // Cornflower blue
                        );
                    });
                }

                if max_lines > lines_to_show {
                    ui.label(
                        egui::RichText::new(format!(
                            "... {} more lines (showing first {} KB)",
                            max_lines - lines_to_show,
                            lines_to_show * self.bytes_per_line / 1024
                        ))
                        .color(egui::Color32::GRAY),
                    );
                }
            });
    }

    /// Get the number of bytes per line
    pub fn bytes_per_line(&self) -> usize {
        self.bytes_per_line
    }

    /// Set the number of bytes per line
    pub fn set_bytes_per_line(&mut self, count: usize) {
        self.bytes_per_line = count.clamp(8, 32);
    }
}
