//! Text viewer widget for displaying text content

use eframe::egui;

/// Text viewer widget
pub struct TextView {
    /// Whether to wrap lines
    wrap_lines: bool,
}

impl Default for TextView {
    fn default() -> Self {
        Self { wrap_lines: true }
    }
}

impl TextView {
    /// Create a new text viewer
    pub fn new() -> Self {
        Self::default()
    }

    /// Render the text view
    pub fn show(&mut self, ui: &mut egui::Ui, content: &str) {
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.wrap_lines, "Wrap lines");
            ui.separator();
            ui.label(format!("{} characters", content.len()));
        });

        ui.separator();

        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.wrap_lines {
                    ui.label(
                        egui::RichText::new(content)
                            .font(egui::FontId::monospace(12.0)),
                    );
                } else {
                    // Use a text edit in read-only mode for horizontal scrolling
                    let mut text = content.to_string();
                    ui.add(
                        egui::TextEdit::multiline(&mut text)
                            .font(egui::FontId::monospace(12.0))
                            .code_editor()
                            .desired_width(f32::INFINITY)
                            .interactive(false),
                    );
                }
            });
    }

    /// Toggle line wrapping
    pub fn toggle_wrap(&mut self) {
        self.wrap_lines = !self.wrap_lines;
    }

    /// Check if line wrapping is enabled
    pub fn is_wrapping(&self) -> bool {
        self.wrap_lines
    }
}

/// Detect if data is likely text and what encoding it might be
pub fn detect_text_encoding(data: &[u8]) -> Option<TextEncoding> {
    if data.is_empty() {
        return None;
    }

    // Check for BOM
    if data.len() >= 3 && data[0] == 0xEF && data[1] == 0xBB && data[2] == 0xBF {
        return Some(TextEncoding::Utf8Bom);
    }
    if data.len() >= 2 && data[0] == 0xFE && data[1] == 0xFF {
        return Some(TextEncoding::Utf16Be);
    }
    if data.len() >= 2 && data[0] == 0xFF && data[1] == 0xFE {
        return Some(TextEncoding::Utf16Le);
    }

    // Check if valid UTF-8
    if std::str::from_utf8(data).is_ok() {
        // Check if it looks like text (mostly printable characters)
        let printable_ratio = data
            .iter()
            .filter(|&&b| b.is_ascii_graphic() || b.is_ascii_whitespace())
            .count() as f64
            / data.len() as f64;

        if printable_ratio > 0.8 {
            return Some(TextEncoding::Utf8);
        }
    }

    // Check if it's ASCII (subset of UTF-8)
    let ascii_ratio = data
        .iter()
        .filter(|&&b| b.is_ascii() && (b.is_ascii_graphic() || b.is_ascii_whitespace()))
        .count() as f64
        / data.len() as f64;

    if ascii_ratio > 0.85 {
        return Some(TextEncoding::Ascii);
    }

    // Check for Latin-1 / ISO-8859-1 (text with high bytes but no invalid sequences)
    let printable_latin1 = data
        .iter()
        .filter(|&&b| {
            (b >= 0x20 && b <= 0x7E) // ASCII printable
            || (b >= 0x80 && b <= 0xFF) // Latin-1 extended
            || b == 0x09 // Tab
            || b == 0x0A // LF
            || b == 0x0D // CR
        })
        .count() as f64
        / data.len() as f64;

    if printable_latin1 > 0.85 {
        return Some(TextEncoding::Latin1);
    }

    None
}

/// Detected text encoding
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextEncoding {
    Ascii,
    Utf8,
    Utf8Bom,
    Utf16Be,
    Utf16Le,
    Latin1,
}

impl TextEncoding {
    /// Decode bytes to string using this encoding
    pub fn decode(&self, data: &[u8]) -> String {
        match self {
            TextEncoding::Ascii | TextEncoding::Utf8 => {
                String::from_utf8_lossy(data).to_string()
            }
            TextEncoding::Utf8Bom => {
                if data.len() >= 3 {
                    String::from_utf8_lossy(&data[3..]).to_string()
                } else {
                    String::new()
                }
            }
            TextEncoding::Utf16Be => {
                let skip = if data.len() >= 2 && data[0] == 0xFE && data[1] == 0xFF {
                    2
                } else {
                    0
                };
                let chars: Vec<u16> = data[skip..]
                    .chunks(2)
                    .filter_map(|chunk| {
                        if chunk.len() == 2 {
                            Some(u16::from_be_bytes([chunk[0], chunk[1]]))
                        } else {
                            None
                        }
                    })
                    .collect();
                String::from_utf16_lossy(&chars)
            }
            TextEncoding::Utf16Le => {
                let skip = if data.len() >= 2 && data[0] == 0xFF && data[1] == 0xFE {
                    2
                } else {
                    0
                };
                let chars: Vec<u16> = data[skip..]
                    .chunks(2)
                    .filter_map(|chunk| {
                        if chunk.len() == 2 {
                            Some(u16::from_le_bytes([chunk[0], chunk[1]]))
                        } else {
                            None
                        }
                    })
                    .collect();
                String::from_utf16_lossy(&chars)
            }
            TextEncoding::Latin1 => {
                // ISO-8859-1 to UTF-8
                data.iter().map(|&b| b as char).collect()
            }
        }
    }

    /// Get display name
    pub fn display_name(&self) -> &'static str {
        match self {
            TextEncoding::Ascii => "ASCII",
            TextEncoding::Utf8 => "UTF-8",
            TextEncoding::Utf8Bom => "UTF-8 (BOM)",
            TextEncoding::Utf16Be => "UTF-16 BE",
            TextEncoding::Utf16Le => "UTF-16 LE",
            TextEncoding::Latin1 => "Latin-1",
        }
    }
}
