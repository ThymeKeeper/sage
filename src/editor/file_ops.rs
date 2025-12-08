use crate::buffer::Buffer;
use crate::syntax::SyntaxHighlighter;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::Editor;

impl Editor {
    /// Normalize text by removing invisible characters and converting line endings/tabs
    pub(super) fn normalize_text(text: String) -> String {
        text.chars()
            .filter_map(|c| match c {
                // Convert tabs to 4 spaces
                '\t' => Some("    ".to_string()),
                // Remove carriage returns (handled separately for CRLF)
                '\r' => None,
                // Remove zero-width and invisible characters
                '\u{200B}' | // Zero-width space
                '\u{200C}' | // Zero-width non-joiner
                '\u{200D}' | // Zero-width joiner
                '\u{200E}' | // Left-to-right mark
                '\u{200F}' | // Right-to-left mark
                '\u{202A}' | // Left-to-right embedding
                '\u{202B}' | // Right-to-left embedding
                '\u{202C}' | // Pop directional formatting
                '\u{202D}' | // Left-to-right override
                '\u{202E}' | // Right-to-left override
                '\u{2060}' | // Word joiner
                '\u{2061}' | // Function application
                '\u{2062}' | // Invisible times
                '\u{2063}' | // Invisible separator
                '\u{2064}' | // Invisible plus
                '\u{2066}' | // Left-to-right isolate
                '\u{2067}' | // Right-to-left isolate
                '\u{2068}' | // First strong isolate
                '\u{2069}' | // Pop directional isolate
                '\u{206A}' | // Inhibit symmetric swapping
                '\u{206B}' | // Activate symmetric swapping
                '\u{206C}' | // Inhibit Arabic form shaping
                '\u{206D}' | // Activate Arabic form shaping
                '\u{206E}' | // National digit shapes
                '\u{206F}' | // Nominal digit shapes
                '\u{FEFF}' | // Zero-width no-break space (BOM)
                '\u{FFF9}' | // Interlinear annotation anchor
                '\u{FFFA}' | // Interlinear annotation separator
                '\u{FFFB}' | // Interlinear annotation terminator
                '\u{00AD}' | // Soft hyphen
                '\u{034F}' | // Combining grapheme joiner
                '\u{061C}' | // Arabic letter mark
                '\u{115F}' | // Hangul choseong filler
                '\u{1160}' | // Hangul jungseong filler
                '\u{17B4}' | // Khmer vowel inherent AQ
                '\u{17B5}' | // Khmer vowel inherent AA
                '\u{180E}' | // Mongolian vowel separator
                '\u{3164}' | // Hangul filler
                '\u{FFA0}' | // Halfwidth hangul filler
                '\u{FE00}'..='\u{FE0F}' | // Variation selectors
                '\u{E0100}'..='\u{E01EF}' => None, // Variation selectors supplement
                // Keep normal characters
                _ => Some(c.to_string()),
            })
            .collect::<String>()
            // Handle CRLF -> LF conversion after filtering
            .replace("\r\n", "\n")
    }

    pub fn load_file(&mut self, path: &str) -> io::Result<()> {
        let content = fs::read_to_string(path)?;
        // Normalize: CRLF → LF, tabs → spaces, remove invisible characters
        let content = Self::normalize_text(content);
        self.buffer = Buffer::from_string(content);
        self.file_path = Some(PathBuf::from(path));
        self.cursor = 0;
        self.selection_start = None;
        self.modified = false;
        self.viewport_offset = (0, 0);
        self.last_saved_undo_len = 0;
        self.mouse_selecting = false;
        self.preferred_column = None;

        // Check if file is read-only
        self.read_only = self.is_file_read_only(path);

        // Initialize syntax highlighting
        self.syntax = SyntaxHighlighter::new();

        // Set language based on file extension
        self.syntax.set_language_from_path(path);

        let line_count = self.buffer.len_lines();

        // For large files, use viewport mode; otherwise init all lines
        if line_count <= 50_000 {
            self.syntax.init_all_lines(line_count);
            self.syntax.process_dirty_lines(|line_index| {
                if line_index < self.buffer.len_lines() {
                    Some(self.buffer.line(line_index).to_string())
                } else {
                    None
                }
            });
        }
        // Large files will initialize viewport on first render

        Ok(())
    }

    pub(super) fn is_file_read_only(&self, path: &str) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            if let Ok(metadata) = fs::metadata(path) {
                let permissions = metadata.permissions();
                // Check if file is read-only
                permissions.readonly() || (permissions.mode() & 0o200) == 0
            } else {
                false // If we can't get metadata, assume it's writable (will fail on save anyway)
            }
        }

        #[cfg(not(unix))]
        {
            if let Ok(metadata) = fs::metadata(path) {
                let permissions = metadata.permissions();
                // On Windows, just check the readonly flag
                permissions.readonly()
            } else {
                false // If we can't get metadata, assume it's writable (will fail on save anyway)
            }
        }
    }

    pub fn save(&mut self) -> io::Result<()> {
        if self.read_only {
            self.status_message = Some(("Cannot save: File is read-only".to_string(), true));
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "File is read-only"));
        }

        if let Some(ref path) = self.file_path {
            // Create parent directories if they don't exist
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            match fs::write(path, self.buffer.to_string()) {
                Ok(_) => {
                    self.modified = false;
                    self.last_saved_undo_len = 0; // Reset save point
                    self.status_message = None; // Clear any error messages
                    Ok(())
                }
                Err(e) => {
                    self.status_message = Some((format!("Save failed: {}", e), true));
                    Err(e)
                }
            }
        } else {
            Err(io::Error::new(io::ErrorKind::Other, "No file path set"))
        }
    }

    pub fn save_as(&mut self, path: PathBuf) -> io::Result<()> {
        // Check if the new path would be read-only
        let new_read_only = self.is_file_read_only(path.to_str().unwrap_or(""));
        if new_read_only {
            self.status_message = Some(("Cannot save: Target location is read-only".to_string(), true));
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Target location is read-only"));
        }

        // Create parent directories if they don't exist
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        match fs::write(&path, self.buffer.to_string()) {
            Ok(_) => {
                self.file_path = Some(path.clone());
                self.modified = false;
                self.last_saved_undo_len = 0; // Reset save point
                self.read_only = new_read_only;
                self.status_message = None; // Clear any error messages
                Ok(())
            }
            Err(e) => {
                self.status_message = Some((format!("Save as failed: {}", e), true));
                Err(e)
            }
        }
    }

    pub fn file_path(&self) -> Option<&Path> {
        self.file_path.as_deref()
    }

    pub fn file_name(&self) -> &str {
        self.file_path
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("[No Name]")
    }

    pub fn set_file_path(&mut self, path: &str) {
        self.file_path = Some(PathBuf::from(path));
        self.syntax.set_language_from_path(path);
    }
}
