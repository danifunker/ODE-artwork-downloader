//! "Disc N" overlay badge for multi-disc artwork.
//!
//! Real jewel-case sets traditionally share box art across discs but stamp
//! the disc face with a number. We mirror that here: composite a small
//! semi-transparent badge in the bottom-right corner of the saved cover so
//! the ODE picker can visually disambiguate discs in the same game.

use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use image::{DynamicImage, Rgba, RgbaImage};

/// Bundled font. ~127 KB, Apache-2.0. Keeps the badge code self-contained
/// rather than depending on a system font that varies per OS.
const FONT_BYTES: &[u8] = include_bytes!("../../assets/fonts/RobotoMono-Bold.ttf");

/// Format "Disc N" or "Disc N/M" depending on whether the set total is known.
pub fn format_label(n: u32, total: Option<u32>) -> String {
    match total {
        Some(t) if t > 1 => format!("Disc {n}/{t}"),
        _ => format!("Disc {n}"),
    }
}

/// Draw a disc-number badge onto `img`. Operates in-place on an RGBA buffer
/// converted from the input. The output is returned as an RGB-ready
/// `DynamicImage` so the JPEG encoder downstream doesn't need to know about
/// the alpha channel we used for the overlay.
pub fn apply_disc_badge(img: DynamicImage, n: u32, total: Option<u32>) -> DynamicImage {
    let label = format_label(n, total);
    apply_badge_text(img, &label)
}

fn apply_badge_text(img: DynamicImage, text: &str) -> DynamicImage {
    let font = match FontVec::try_from_vec(FONT_BYTES.to_vec()) {
        Ok(f) => f,
        Err(_) => return img, // Font load failed: leave the image untouched.
    };

    let mut rgba: RgbaImage = img.to_rgba8();
    let (w, h) = (rgba.width() as i32, rgba.height() as i32);

    // Scale the badge so it's readable but not dominant. Use 8% of the
    // shorter dimension as the cap height; this lands around 40px on a
    // 500px artwork, which is roughly the proportion a real Disc N stamp
    // had on a CD label.
    let short = w.min(h) as f32;
    let cap_h = (short * 0.08).round().max(14.0);
    let scale = PxScale::from(cap_h * 1.35); // PxScale ~= line height in px.
    let scaled = font.as_scaled(scale);

    // Pre-measure the glyph run to size the background pill.
    let mut run_w = 0.0f32;
    for ch in text.chars() {
        let g = scaled.scaled_glyph(ch);
        run_w += scaled.h_advance(g.id);
    }
    let pad_x = (cap_h * 0.55).round();
    let pad_y = (cap_h * 0.30).round();
    let pill_w = (run_w + pad_x * 2.0).round() as i32;
    let pill_h = (cap_h + pad_y * 2.0).round() as i32;
    let margin = (short * 0.025).round() as i32;
    let pill_x = w - pill_w - margin;
    let pill_y = h - pill_h - margin;

    // Backing pill: semi-transparent black so the label stays legible over
    // both light and dark cover art. Rounded corners drawn by skipping a
    // quarter-circle in each corner.
    let corner = (cap_h * 0.30).round() as i32;
    let bg = Rgba([0u8, 0, 0, 200]);
    for y in pill_y..pill_y + pill_h {
        for x in pill_x..pill_x + pill_w {
            if x < 0 || y < 0 || x >= w || y >= h {
                continue;
            }
            if in_rounded_rect(x, y, pill_x, pill_y, pill_w, pill_h, corner) {
                blend_pixel(&mut rgba, x as u32, y as u32, bg);
            }
        }
    }

    // Text: white, drawn glyph-by-glyph onto the pill.
    let text_x = pill_x + pad_x as i32;
    let baseline = pill_y + pad_y as i32 + scaled.ascent() as i32;
    imageproc::drawing::draw_text_mut(
        &mut rgba,
        Rgba([255u8, 255, 255, 255]),
        text_x,
        baseline - scaled.ascent() as i32,
        scale,
        &font,
        text,
    );

    DynamicImage::ImageRgba8(rgba)
}

fn in_rounded_rect(x: i32, y: i32, rx: i32, ry: i32, w: i32, h: i32, r: i32) -> bool {
    // Quick interior check.
    let cx = if x < rx + r {
        rx + r
    } else if x >= rx + w - r {
        rx + w - 1 - r
    } else {
        return true;
    };
    let cy = if y < ry + r {
        ry + r
    } else if y >= ry + h - r {
        ry + h - 1 - r
    } else {
        return true;
    };
    let dx = (x - cx) as i64;
    let dy = (y - cy) as i64;
    dx * dx + dy * dy <= (r as i64) * (r as i64)
}

fn blend_pixel(img: &mut RgbaImage, x: u32, y: u32, src: Rgba<u8>) {
    let dst = img.get_pixel_mut(x, y);
    let a = src[3] as u16;
    let inv = 255 - a;
    for c in 0..3 {
        let new = (src[c] as u16 * a + dst[c] as u16 * inv) / 255;
        dst[c] = new as u8;
    }
    dst[3] = 255;
}
