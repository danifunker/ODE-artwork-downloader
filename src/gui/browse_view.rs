//! Browse view for navigating disc filesystem contents

use eframe::egui;
use std::collections::HashSet;
use std::sync::mpsc::Receiver;

use crate::disc::browse::{FileEntry, EntryType, open_filesystem};
use crate::disc::DiscInfo;

use super::hex_view::HexView;
use super::text_view::{TextView, TextEncoding, detect_text_encoding};

/// Maximum file size to load into memory for viewing
const MAX_VIEW_SIZE: u64 = 10 * 1024 * 1024; // 10 MB

/// View mode for file content
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Hex,
    Text,
    Auto,
}

impl ViewMode {
    pub fn display_name(&self) -> &'static str {
        match self {
            ViewMode::Hex => "Hex",
            ViewMode::Text => "Text",
            ViewMode::Auto => "Auto",
        }
    }
}

/// Content loaded for viewing
pub enum FileContent {
    Binary(Vec<u8>),
    Text(String, TextEncoding),
}

/// Browse view state
pub struct BrowseView {
    /// Root file entry
    root: Option<FileEntry>,
    /// Set of expanded directory paths
    expanded_paths: HashSet<String>,
    /// Cache of directory listings
    directory_cache: std::collections::HashMap<String, Vec<FileEntry>>,
    /// Currently selected file path
    selected_path: Option<String>,
    /// Currently selected entry (for reading)
    selected_entry: Option<FileEntry>,
    /// Content of selected file
    content: Option<FileContent>,
    /// View mode
    view_mode: ViewMode,
    /// Hex viewer
    hex_view: HexView,
    /// Text viewer
    text_view: TextView,
    /// Is content loading?
    loading: bool,
    /// Error message
    error: Option<String>,
    /// Receiver for directory listing results
    dir_receiver: Option<Receiver<Result<Vec<FileEntry>, String>>>,
    /// Receiver for file content results
    content_receiver: Option<Receiver<Result<Vec<u8>, String>>>,
    /// Path being loaded
    loading_path: Option<String>,
}

impl Default for BrowseView {
    fn default() -> Self {
        Self {
            root: None,
            expanded_paths: HashSet::new(),
            directory_cache: std::collections::HashMap::new(),
            selected_path: None,
            selected_entry: None,
            content: None,
            view_mode: ViewMode::Auto,
            hex_view: HexView::new(),
            text_view: TextView::new(),
            loading: false,
            error: None,
            dir_receiver: None,
            content_receiver: None,
            loading_path: None,
        }
    }
}

impl BrowseView {
    /// Create a new browse view
    pub fn new() -> Self {
        Self::default()
    }

    /// Initialize the browse view with a disc info
    pub fn initialize(&mut self, disc_info: &DiscInfo) -> Result<(), String> {
        // Try to open the filesystem
        let mut fs = open_filesystem(disc_info)
            .map_err(|e| format!("Failed to open filesystem: {}", e))?;

        // Get root directory
        let root = fs.root()
            .map_err(|e| format!("Failed to get root: {}", e))?;

        // Get initial listing for root
        let root_entries = fs.list_directory(&root)
            .map_err(|e| format!("Failed to list root: {}", e))?;

        self.root = Some(root.clone());
        self.directory_cache.insert("/".to_string(), root_entries);
        self.expanded_paths.insert("/".to_string());
        self.error = None;

        Ok(())
    }

    /// Poll for async results
    pub fn poll(&mut self) {
        // Check for directory listing results
        if let Some(ref receiver) = self.dir_receiver {
            if let Ok(result) = receiver.try_recv() {
                self.loading = false;
                match result {
                    Ok(entries) => {
                        if let Some(path) = self.loading_path.take() {
                            self.directory_cache.insert(path.clone(), entries);
                            self.expanded_paths.insert(path);
                        }
                    }
                    Err(e) => {
                        self.error = Some(e);
                    }
                }
                self.dir_receiver = None;
            }
        }

        // Check for file content results
        if let Some(ref receiver) = self.content_receiver {
            if let Ok(result) = receiver.try_recv() {
                self.loading = false;
                match result {
                    Ok(data) => {
                        // Determine content type
                        let content = if let Some(encoding) = detect_text_encoding(&data) {
                            let text = encoding.decode(&data);
                            FileContent::Text(text, encoding)
                        } else {
                            FileContent::Binary(data)
                        };
                        self.content = Some(content);
                    }
                    Err(e) => {
                        self.error = Some(e);
                    }
                }
                self.content_receiver = None;
            }
        }
    }

    /// Check if the view is active (has been initialized)
    pub fn is_active(&self) -> bool {
        self.root.is_some()
    }

    /// Render the browse view
    pub fn show(&mut self, ui: &mut egui::Ui, disc_info: &DiscInfo) {
        self.poll();

        // Two-column layout: tree on left, content on right
        ui.horizontal(|ui| {
            // Left panel: file tree
            ui.vertical(|ui| {
                ui.set_min_width(300.0);
                ui.set_max_width(400.0);

                ui.heading("Files");
                ui.separator();

                egui::ScrollArea::vertical()
                    .id_salt("file_tree")
                    .auto_shrink([false, false])
                    .max_height(ui.available_height() - 50.0)
                    .show(ui, |ui| {
                        if let Some(root) = self.root.clone() {
                            self.render_tree_entry(ui, &root, disc_info);
                        }
                    });
            });

            ui.separator();

            // Right panel: content viewer
            ui.vertical(|ui| {
                ui.heading("Content");

                // View mode selector
                ui.horizontal(|ui| {
                    ui.label("View:");
                    ui.selectable_value(&mut self.view_mode, ViewMode::Auto, "Auto");
                    ui.selectable_value(&mut self.view_mode, ViewMode::Hex, "Hex");
                    ui.selectable_value(&mut self.view_mode, ViewMode::Text, "Text");

                    ui.separator();

                    // Export button
                    let export_entry = if let Some(ref entry) = self.selected_entry {
                        if entry.is_file() && ui.button("Export...").clicked() {
                            Some(entry.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    if let Some(entry) = export_entry {
                        self.export_file(&entry, disc_info);
                    }
                });

                ui.separator();

                // Content area
                if self.loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Loading...");
                    });
                } else if let Some(ref error) = self.error {
                    ui.colored_label(egui::Color32::RED, format!("Error: {}", error));
                } else if let Some(ref content) = self.content {
                    match content {
                        FileContent::Binary(data) => {
                            match self.view_mode {
                                ViewMode::Text => {
                                    // Force text view even for binary
                                    let text = String::from_utf8_lossy(data);
                                    self.text_view.show(ui, &text);
                                }
                                _ => {
                                    self.hex_view.show(ui, data, 0);
                                }
                            }
                        }
                        FileContent::Text(text, encoding) => {
                            match self.view_mode {
                                ViewMode::Hex => {
                                    // Show original bytes in hex
                                    self.hex_view.show(ui, text.as_bytes(), 0);
                                }
                                _ => {
                                    ui.label(format!("Encoding: {}", encoding.display_name()));
                                    self.text_view.show(ui, text);
                                }
                            }
                        }
                    }
                } else if let Some(ref entry) = self.selected_entry {
                    if entry.is_directory() {
                        ui.label("Select a file to view its contents");
                    } else if entry.size > MAX_VIEW_SIZE {
                        ui.label(format!(
                            "File is too large to view ({} > {} MB)",
                            entry.size_string(),
                            MAX_VIEW_SIZE / 1024 / 1024
                        ));
                        ui.label("Use Export to save the file");
                    } else {
                        ui.label("Click on a file to load its contents");
                    }
                } else {
                    ui.label("Select a file from the tree to view its contents");
                }
            });
        });
    }

    /// Render a tree entry recursively
    fn render_tree_entry(&mut self, ui: &mut egui::Ui, entry: &FileEntry, disc_info: &DiscInfo) {
        let path = entry.path.clone();

        match entry.entry_type {
            EntryType::Directory => {
                let is_expanded = self.expanded_paths.contains(&path);
                let has_children = self.directory_cache.contains_key(&path);

                let header = egui::CollapsingHeader::new(&entry.name)
                    .id_salt(&path)
                    .default_open(path == "/")
                    .open(if is_expanded { Some(true) } else { None })
                    .show(ui, |ui| {
                        if let Some(children) = self.directory_cache.get(&path).cloned() {
                            for child in children {
                                self.render_tree_entry(ui, &child, disc_info);
                            }
                        } else if !self.loading {
                            ui.label("Loading...");
                        }
                    });

                // Load children when expanded
                if header.fully_open() && !has_children && !self.loading {
                    self.load_directory(entry.clone(), disc_info);
                }
            }
            EntryType::File => {
                let is_selected = self.selected_path.as_ref() == Some(&path);
                let display = format!("{} ({})", entry.name, entry.size_string());

                if ui.selectable_label(is_selected, display).clicked() {
                    self.select_file(entry.clone(), disc_info);
                }
            }
        }
    }

    /// Load a directory's contents asynchronously
    fn load_directory(&mut self, entry: FileEntry, disc_info: &DiscInfo) {
        // For simplicity, load synchronously in this version
        // A full async implementation would use channels
        if let Ok(mut fs) = open_filesystem(disc_info) {
            if let Ok(children) = fs.list_directory(&entry) {
                self.directory_cache.insert(entry.path.clone(), children);
                self.expanded_paths.insert(entry.path);
            }
        }
    }

    /// Select a file and load its content
    fn select_file(&mut self, entry: FileEntry, disc_info: &DiscInfo) {
        self.selected_path = Some(entry.path.clone());
        self.selected_entry = Some(entry.clone());
        self.content = None;
        self.error = None;

        if entry.size > MAX_VIEW_SIZE {
            return; // Don't auto-load large files
        }

        // Load file content synchronously
        self.loading = true;
        if let Ok(mut fs) = open_filesystem(disc_info) {
            match fs.read_file(&entry) {
                Ok(data) => {
                    let content = if let Some(encoding) = detect_text_encoding(&data) {
                        let text = encoding.decode(&data);
                        FileContent::Text(text, encoding)
                    } else {
                        FileContent::Binary(data)
                    };
                    self.content = Some(content);
                }
                Err(e) => {
                    self.error = Some(format!("Failed to read file: {}", e));
                }
            }
        }
        self.loading = false;
    }

    /// Export a file to disk
    fn export_file(&mut self, entry: &FileEntry, disc_info: &DiscInfo) {
        // Use file picker to get save location
        if let Some(path) = rfd::FileDialog::new()
            .set_file_name(&entry.name)
            .save_file()
        {
            // Read and save the file
            if let Ok(mut fs) = open_filesystem(disc_info) {
                match fs.read_file(entry) {
                    Ok(data) => {
                        if let Err(e) = std::fs::write(&path, &data) {
                            self.error = Some(format!("Failed to write file: {}", e));
                        }
                    }
                    Err(e) => {
                        self.error = Some(format!("Failed to read file: {}", e));
                    }
                }
            }
        }
    }

    /// Get the currently selected file content as bytes (for external export)
    pub fn get_content_bytes(&self) -> Option<&[u8]> {
        match &self.content {
            Some(FileContent::Binary(data)) => Some(data),
            Some(FileContent::Text(text, _)) => Some(text.as_bytes()),
            None => None,
        }
    }

    /// Clear the view state
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}
