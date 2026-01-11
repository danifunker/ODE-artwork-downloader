// Build script to embed resources into the executable

#[cfg(windows)]
fn main() {
    let mut res = winres::WindowsResource::new();
    res.set_icon("assets/icons/icon.ico");
    res.compile().unwrap();
}

#[cfg(not(windows))]
fn main() {
    // Nothing to do on other platforms
}
