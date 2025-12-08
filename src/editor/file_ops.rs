use crate::buffer::Buffer;
use crate::syntax::SyntaxHighlighter;
use crate::kernel::{self, Kernel};
use crate::direct_kernel::DirectKernel;
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

        // Check for shebang and auto-select kernel if appropriate
        self.detect_and_set_kernel_from_shebang();

        Ok(())
    }

    /// Detect shebang in the first few lines and automatically select an appropriate kernel
    fn detect_and_set_kernel_from_shebang(&mut self) {
        // Check first 3 lines for shebang
        let shebang = self.detect_shebang();

        if let Some(interpreter) = shebang {
            // Try to find and set an appropriate kernel based on the shebang
            if let Some(kernel) = self.find_kernel_for_interpreter(&interpreter) {
                self.set_kernel(kernel);
                self.enable_repl_mode();

                // Connect to the kernel
                if let Err(e) = self.connect_kernel() {
                    self.status_message = Some((
                        format!("Auto-detected kernel but failed to connect: {}", e),
                        true
                    ));
                } else {
                    // Set a status message to let the user know the kernel was auto-detected
                    if let Some(kernel_name) = self.get_kernel_info() {
                        self.status_message = Some((
                            format!("Auto-detected and connected to: {}", kernel_name),
                            false
                        ));
                    }
                }
            }
        }
    }

    /// Detect shebang line in the first few lines of the file
    /// Returns the interpreter path/name if found
    fn detect_shebang(&self) -> Option<String> {
        // Check first 3 lines
        for line_idx in 0..3.min(self.buffer.len_lines()) {
            let line = self.buffer.line(line_idx);
            let line = line.trim();

            // Check if line starts with shebang
            if line.starts_with("#!") {
                let shebang = line[2..].trim();

                // Parse different shebang formats
                // Format 1: #!/usr/bin/env python3
                if shebang.starts_with("/usr/bin/env ") || shebang.starts_with("/bin/env ") {
                    let parts: Vec<&str> = shebang.split_whitespace().collect();
                    if parts.len() >= 2 {
                        return Some(parts[1].to_string());
                    }
                }
                // Format 2: #!/usr/bin/python3 or #!/home/user/venv/bin/python
                else if shebang.contains('/') {
                    // For full paths, return the entire path (useful for venv detection)
                    let path = shebang.split_whitespace().next().unwrap_or(shebang);
                    return Some(path.to_string());
                }
                // Format 3: #!python3 (rare but possible)
                else {
                    let interpreter = shebang.split_whitespace().next().unwrap_or(shebang);
                    return Some(interpreter.to_string());
                }
            }
        }

        None
    }

    /// Find an appropriate kernel for the given interpreter
    fn find_kernel_for_interpreter(&self, interpreter: &str) -> Option<Box<dyn Kernel>> {
        // Check if it's a full path to a Python executable
        if interpreter.contains('/') {
            // It's a full path - use it directly
            let display_name = format!("Python ({})", interpreter);
            return Some(Box::new(DirectKernel::new(
                interpreter.to_string(),
                interpreter.to_string(),
                display_name
            )));
        }

        // Normalize interpreter name
        let interpreter_lower = interpreter.to_lowercase();

        // Check if it's a Python interpreter
        if interpreter_lower.starts_with("python") {
            // Try to find Python kernels
            let kernels = kernel::discover_kernels();

            if !kernels.is_empty() {
                // Look for a kernel that matches the interpreter
                // Priority: exact match > python3 > python > any python kernel

                // Try exact match first
                if let Some(kernel_info) = kernels.iter().find(|k| {
                    k.display_name.to_lowercase().contains(&interpreter_lower) ||
                    k.name.to_lowercase().contains(&interpreter_lower)
                }) {
                    return Some(self.create_kernel_from_info(kernel_info));
                }

                // Try python3
                if interpreter_lower.contains('3') {
                    if let Some(kernel_info) = kernels.iter().find(|k| {
                        k.display_name.to_lowercase().contains("python 3") ||
                        k.display_name.to_lowercase().contains("python3") ||
                        k.name.to_lowercase().contains("python3")
                    }) {
                        return Some(self.create_kernel_from_info(kernel_info));
                    }
                }

                // Try any python kernel
                if let Some(kernel_info) = kernels.iter().find(|k| {
                    k.display_name.to_lowercase().contains("python") ||
                    k.name.to_lowercase().contains("python")
                }) {
                    return Some(self.create_kernel_from_info(kernel_info));
                }
            }

            // No kernels found, try direct kernel
            return self.try_direct_kernel_for_interpreter(&interpreter_lower);
        }

        None
    }

    /// Create a kernel from KernelInfo
    fn create_kernel_from_info(&self, kernel_info: &kernel::KernelInfo) -> Box<dyn Kernel> {
        // For now, always use DirectKernel regardless of type
        // TODO: Implement proper Jupyter kernel support
        Box::new(DirectKernel::new(
            kernel_info.python_path.clone(),
            kernel_info.name.clone(),
            kernel_info.display_name.clone()
        ))
    }

    /// Try to create a direct kernel for the interpreter
    fn try_direct_kernel_for_interpreter(&self, interpreter: &str) -> Option<Box<dyn Kernel>> {
        // Map common interpreter names to executable names
        let executable = match interpreter {
            "python" | "python2" | "python3" => interpreter.to_string(),
            name if name.starts_with("python") => {
                // Try the exact name first, fall back to python3
                if std::process::Command::new(name).arg("--version").output().is_ok() {
                    name.to_string()
                } else {
                    "python3".to_string()
                }
            }
            _ => return None,
        };

        // Try to create a direct kernel
        // DirectKernel::new takes (python_path, name, display_name)
        let display_name = format!("Python ({})", executable);
        Some(Box::new(DirectKernel::new(
            executable.clone(),
            executable.clone(),
            display_name
        )))
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
