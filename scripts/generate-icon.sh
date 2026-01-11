#!/bin/bash
# scripts/generate-icon.sh
# Converts icon-original.png to all required formats for cross-platform releases

set -e

ORIGINAL="assets/icon-original.png"
ASSETS_DIR="assets/icons"

# Check if ImageMagick is installed
if command -v magick &> /dev/null; then
    MAGICK_CMD="magick"
elif command -v convert &> /dev/null; then
    MAGICK_CMD="convert"
else
    echo "Error: ImageMagick is not installed."
    echo "Install it with:"
    echo "  macOS: brew install imagemagick"
    echo "  Linux: sudo apt-get install imagemagick"
    exit 1
fi

# Check if original icon exists
if [ ! -f "$ORIGINAL" ]; then
    echo "Error: $ORIGINAL not found"
    exit 1
fi

# Create assets directory
mkdir -p "$ASSETS_DIR"

echo "Converting $ORIGINAL to multiple formats..."

# Generate PNG icons at various sizes
SIZES=(16 32 48 64 128 256 512 1024)
for size in "${SIZES[@]}"; do
    echo "Creating ${size}x${size} PNG..."
    $MAGICK_CMD "$ORIGINAL" -resize ${size}x${size} "$ASSETS_DIR/icon-${size}.png"
done

# Generate Windows ICO file (multi-size)
echo "Creating Windows ICO file..."
$MAGICK_CMD "$ORIGINAL" \
    \( -clone 0 -resize 16x16 \) \
    \( -clone 0 -resize 32x32 \) \
    \( -clone 0 -resize 48x48 \) \
    \( -clone 0 -resize 64x64 \) \
    \( -clone 0 -resize 128x128 \) \
    \( -clone 0 -resize 256x256 \) \
    -delete 0 "$ASSETS_DIR/icon.ico"

# Generate macOS ICNS file (requires additional tools on macOS)
if [[ "$OSTYPE" == "darwin"* ]]; then
    echo "Creating macOS ICNS file..."
    
    # Create iconset directory
    ICONSET="$ASSETS_DIR/icon.iconset"
    mkdir -p "$ICONSET"
    
    # Generate all required sizes for ICNS
    $MAGICK_CMD "$ORIGINAL" -resize 16x16 "$ICONSET/icon_16x16.png"
    $MAGICK_CMD "$ORIGINAL" -resize 32x32 "$ICONSET/icon_16x16@2x.png"
    $MAGICK_CMD "$ORIGINAL" -resize 32x32 "$ICONSET/icon_32x32.png"
    $MAGICK_CMD "$ORIGINAL" -resize 64x64 "$ICONSET/icon_32x32@2x.png"
    $MAGICK_CMD "$ORIGINAL" -resize 128x128 "$ICONSET/icon_128x128.png"
    $MAGICK_CMD "$ORIGINAL" -resize 256x256 "$ICONSET/icon_128x128@2x.png"
    $MAGICK_CMD "$ORIGINAL" -resize 256x256 "$ICONSET/icon_256x256.png"
    $MAGICK_CMD "$ORIGINAL" -resize 512x512 "$ICONSET/icon_256x256@2x.png"
    $MAGICK_CMD "$ORIGINAL" -resize 512x512 "$ICONSET/icon_512x512.png"
    $MAGICK_CMD "$ORIGINAL" -resize 1024x1024 "$ICONSET/icon_512x512@2x.png"
    
    # Convert to ICNS
    iconutil -c icns "$ICONSET"
    
    # Clean up
    rm -rf "$ICONSET"
else
    echo "Skipping ICNS generation (macOS only)"
    echo "Note: The workflow will use PNG icons for macOS"
fi

# Create a simple AppImage-ready icon structure
echo "Creating AppImage icon structure..."
mkdir -p "$ASSETS_DIR/hicolor/256x256/apps"
cp "$ASSETS_DIR/icon-256.png" "$ASSETS_DIR/hicolor/256x256/apps/ode-artwork-downloader.png"

# Copy main icon for easy reference
cp "$ASSETS_DIR/icon-256.png" "$ASSETS_DIR/icon.png"

echo ""
echo "✓ Icon conversion complete!"
echo ""
echo "Generated files:"
ls -lh "$ASSETS_DIR"
echo ""
echo "Icon files are ready for:"
echo "  • Windows: $ASSETS_DIR/icon.ico"
echo "  • macOS: $ASSETS_DIR/icon.png (or icon.icns if on macOS)"
echo "  • Linux AppImage: $ASSETS_DIR/hicolor/256x256/apps/ode-artwork-downloader.png"
echo "  • Snap: $ASSETS_DIR/icon.png"