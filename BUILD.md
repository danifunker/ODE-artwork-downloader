This is only for linux

# Update package list
sudo apt-get update

# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env

# Install build essentials
sudo apt-get install -y build-essential pkg-config

# Install GUI/GTK dependencies
sudo apt-get install -y libgtk-3-dev

# Install OpenSSL development libraries
sudo apt-get install -y libssl-dev

# Install Snapcraft (for snap builds)
sudo snap install snapcraft --classic

# Install AppImage tools (for AppImage builds)
wget https://github.com/AppImage/AppImageKit/releases/download/continuous/appimagetool-aarch64.AppImage
chmod +x appimagetool-aarch64.AppImage
sudo mv appimagetool-aarch64.AppImage /usr/local/bin/appimagetool

# Install other utilities
sudo apt-get install -y wget curl git libfuse-dev

# Install flatpak and flatpak-builder
sudo apt-get update
sudo apt-get install -y flatpak flatpak-builder python3

# Add Flathub repository
flatpak remote-add --if-not-exists flathub https://flathub.org/repo/flathub.flatpakrepo --user

# Install the Freedesktop runtime and SDK (arm64 versions)
flatpak install -y --user flathub org.freedesktop.Platform//23.08 org.freedesktop.Sdk//23.08 org.freedesktop.Sdk.Extension.rust-stable//23.08