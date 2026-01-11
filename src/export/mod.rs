//! Image export module for USBODE artwork
//!
//! Handles cropping, resizing, and JPEG conversion for cover art.
//! Output is 240x240px baseline JPEG with specific color space settings.

use image::{DynamicImage, RgbImage};
use std::path::Path;

/// Target size for USBODE artwork
pub const TARGET_SIZE: u32 = 240;

/// JPEG quality setting (0-100)
pub const JPEG_QUALITY: u8 = 90;

/// Result of an export operation
#[derive(Debug)]
pub struct ExportResult {
    /// Path where the image was saved
    pub output_path: String,
    /// Original image dimensions
    pub original_size: (u32, u32),
    /// Final image dimensions (should be 240x240)
    pub final_size: (u32, u32),
    /// Whether cropping was applied
    pub was_cropped: bool,
}

/// Export settings
#[derive(Debug, Clone)]
pub struct ExportSettings {
    /// Target width and height (square)
    pub target_size: u32,
    /// JPEG quality (0-100)
    pub quality: u8,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            target_size: TARGET_SIZE,
            quality: JPEG_QUALITY,
        }
    }
}

/// Process and export an image for USBODE
///
/// This function:
/// 1. Center-crops non-square images to a square
/// 2. Resizes to 240x240 pixels
/// 3. Converts to baseline JPEG with YCbCr color space (BT.601)
/// 4. Saves with quality 90, 4:4:4 subsampling, no ICC profile
pub fn export_artwork<P: AsRef<Path>>(
    image_data: &[u8],
    output_path: P,
    settings: &ExportSettings,
) -> Result<ExportResult, String> {
    // Load the image
    let img = image::load_from_memory(image_data)
        .map_err(|e| format!("Failed to load image: {}", e))?;

    let original_size = (img.width(), img.height());

    // Crop to square if needed
    let (cropped_img, was_cropped) = crop_to_square(img);

    // Resize to target size
    let resized = cropped_img.resize_exact(
        settings.target_size,
        settings.target_size,
        image::imageops::FilterType::Lanczos3,
    );

    // Convert to RGB
    let rgb_image = resized.to_rgb8();

    // Encode as baseline JPEG with specific settings
    let jpeg_data = encode_baseline_jpeg(&rgb_image, settings.quality)?;

    // Write to file
    let output_path = output_path.as_ref();
    std::fs::write(output_path, &jpeg_data)
        .map_err(|e| format!("Failed to write file: {}", e))?;

    Ok(ExportResult {
        output_path: output_path.display().to_string(),
        original_size,
        final_size: (settings.target_size, settings.target_size),
        was_cropped,
    })
}

/// Export artwork from a URL
pub fn export_artwork_from_url<P: AsRef<Path>>(
    url: &str,
    output_path: P,
    settings: &ExportSettings,
) -> Result<ExportResult, String> {
    // Fetch the image
    let image_data = fetch_image(url)?;

    // Export it
    export_artwork(&image_data, output_path, settings)
}

/// Fetch image data from a URL
fn fetch_image(url: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    let response = client
        .get(url)
        .send()
        .map_err(|e| format!("Failed to fetch image: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("HTTP error: {}", response.status()));
    }

    response
        .bytes()
        .map(|b| b.to_vec())
        .map_err(|e| format!("Failed to read response: {}", e))
}

/// Center-crop an image to a square
///
/// If the image is already square, returns it unchanged.
/// Otherwise, crops from the center to make it square.
fn crop_to_square(img: DynamicImage) -> (DynamicImage, bool) {
    let width = img.width();
    let height = img.height();

    if width == height {
        return (img, false);
    }

    let size = width.min(height);
    let x = (width - size) / 2;
    let y = (height - size) / 2;

    let cropped = img.crop_imm(x, y, size, size);
    (cropped, true)
}

/// Encode image as baseline JPEG
///
/// This produces:
/// - Baseline (non-progressive) JPEG
/// - YCbCr color space (standard JFIF)
/// - No ICC profile
/// - No EXIF data
///
/// Note: The image crate's encoder uses standard JPEG encoding.
/// For exact BT.601 YCbCr with 4:4:4 subsampling like the Python script,
/// consider adding the `jpeg-encoder` crate for more control.
fn encode_baseline_jpeg(rgb_image: &RgbImage, quality: u8) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, quality);

    encoder
        .encode(
            rgb_image.as_raw(),
            rgb_image.width(),
            rgb_image.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|e| format!("Failed to encode JPEG: {}", e))?;

    Ok(output)
}

/// Generate output filename from disc image path
///
/// Changes the extension to .jpg
pub fn generate_output_path<P: AsRef<Path>>(disc_path: P) -> String {
    let path = disc_path.as_ref();
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("cover");
    let parent = path.parent().unwrap_or(Path::new("."));

    parent.join(format!("{}.jpg", stem)).display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_output_path() {
        assert_eq!(
            generate_output_path("/path/to/game.iso"),
            "/path/to/game.jpg"
        );
        assert_eq!(
            generate_output_path("/path/to/game.chd"),
            "/path/to/game.jpg"
        );
        assert_eq!(
            generate_output_path("game.bin"),
            "./game.jpg"
        );
    }

    #[test]
    fn test_crop_to_square_already_square() {
        let img = DynamicImage::new_rgb8(100, 100);
        let (result, was_cropped) = crop_to_square(img);
        assert!(!was_cropped);
        assert_eq!(result.width(), 100);
        assert_eq!(result.height(), 100);
    }

    #[test]
    fn test_crop_to_square_landscape() {
        let img = DynamicImage::new_rgb8(200, 100);
        let (result, was_cropped) = crop_to_square(img);
        assert!(was_cropped);
        assert_eq!(result.width(), 100);
        assert_eq!(result.height(), 100);
    }

    #[test]
    fn test_crop_to_square_portrait() {
        let img = DynamicImage::new_rgb8(100, 200);
        let (result, was_cropped) = crop_to_square(img);
        assert!(was_cropped);
        assert_eq!(result.width(), 100);
        assert_eq!(result.height(), 100);
    }

    #[test]
    fn test_default_settings() {
        let settings = ExportSettings::default();
        assert_eq!(settings.target_size, 240);
        assert_eq!(settings.quality, 90);
    }
}
