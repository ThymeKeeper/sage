use crate::buffer::Buffer;
use crate::syntax::SyntaxHighlighter;
use crate::kernel::{self, Kernel};
use crate::direct_kernel::DirectKernel;
use crate::spreadsheet::Spreadsheet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::Editor;

fn is_spreadsheet_ext(path: &Path) -> Option<u8> {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("csv") => Some(b','),
        Some(ext) if ext.eq_ignore_ascii_case("tsv") => Some(b'\t'),
        _ => None,
    }
}

impl Editor {
    /// Normalize text under the WYSIWYG input policy (drop invisibles, fold
    /// visible confusables to ASCII, tabs to spaces, CRLF to LF). Delegates to
    /// [`crate::normalize`] so typing, paste, and file load all share one policy.
    pub(super) fn normalize_text(text: String) -> String {
        crate::normalize::normalize_text(&text)
    }

    pub fn load_file(&mut self, path: &str) -> io::Result<()> {
        let path_obj = Path::new(path);

        // Spreadsheet mode for CSV/TSV files
        self.grid_text_view = false;
        if is_spreadsheet_ext(path_obj).is_some() {
            let ss = Spreadsheet::from_file(path_obj)?;
            self.spreadsheet = Some(ss);
            self.buffer = Buffer::new();
            self.file_path = Some(PathBuf::from(path));
            self.cursor = 0;
            self.selection_start = None;
            self.modified = false;
            self.viewport_offset = (0, 0);
            self.viewport_top_seg = 0;
            self.last_saved_undo_len = 0;
            self.read_only = self.is_file_read_only(path);
            self.syntax = SyntaxHighlighter::new();
            self.syntax.set_language_from_path(path);
            return Ok(());
        }

        self.spreadsheet = None;
        let content = fs::read_to_string(path)?;
        // Buffer::from_string applies the WYSIWYG input policy (CRLF → LF,
        // tabs → spaces, drop invisibles, fold confusables to ASCII).
        self.buffer = Buffer::from_string(content);
        self.file_path = Some(PathBuf::from(path));
        self.cursor = 0;
        self.selection_start = None;
        self.modified = false;
        self.viewport_offset = (0, 0);
        self.viewport_top_seg = 0;
        self.last_saved_undo_len = 0;
        self.mouse_selecting = false;
        self.word_select_mode = false;
        self.line_select_mode = false;
        self.word_select_anchor = None;
        self.line_select_anchor = None;
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

        // Enable REPL mode and auto-connect a kernel based on the file extension.
        // .sql -> Snowflake (if configured); .py/.pyw -> Python interpreter.
        if let Some(ext) = Path::new(path).extension() {
            if ext == "sql" {
                self.enable_repl_mode();
                if !self.is_kernel_connected() {
                    self.try_connect_snowflake_kernel();
                }
            } else if ext == "py" || ext == "pyw" {
                self.enable_repl_mode();
                // Try shebang-based kernel detection first, then fall back to discovery
                if !self.is_kernel_connected() {
                    self.detect_and_set_kernel_from_shebang();
                }
                if !self.is_kernel_connected() {
                    self.auto_connect_kernel();
                }
            }
        }

        Ok(())
    }

    /// Try to build and connect a SnowflakeKernel from the user's config
    /// (`~/.config/sage/snowflake.toml`). Silently no-ops if no config exists
    /// or if connect fails — the user can fix config and re-open, or pick a
    /// different kernel via Ctrl+K.
    fn try_connect_snowflake_kernel(&mut self) {
        let info = crate::kernel::KernelInfo {
            name: crate::kernel::SNOWFLAKE_KERNEL_NAME.to_string(),
            display_name: "Snowflake".to_string(),
            python_path: String::new(),
        };
        match crate::kernel::build_from_info(&info) {
            Ok(mut kernel) => {
                if let Err(e) = kernel.connect() {
                    self.status_message = Some((
                        format!("Snowflake auto-connect failed: {} (press Ctrl+K to pick another kernel)", e),
                        true,
                    ));
                    return;
                }
                let display = kernel.info().display_name.clone();
                self.set_kernel(kernel);
                self.status_message =
                    Some((format!("Auto-connected to {}", display), false));
            }
            Err(e) => {
                self.status_message = Some((
                    format!("Snowflake config not loaded: {} (add C:\\.dotfile\\snowflake.toml)", e),
                    true,
                ));
            }
        }
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

        let path = match self.file_path.clone() {
            Some(p) => p,
            None => return Err(io::Error::new(io::ErrorKind::Other, "No file path set")),
        };

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        if let Some(ss) = self.spreadsheet.as_mut() {
            return match ss.save(&path) {
                Ok(()) => {
                    self.status_message = None;
                    self.refresh_grid_text_view();
                    Ok(())
                }
                Err(e) => {
                    self.status_message = Some((format!("Save failed: {}", e), true));
                    Err(e)
                }
            };
        }

        match fs::write(&path, self.buffer.to_string()) {
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

        if let Some(ss) = self.spreadsheet.as_mut() {
            return match ss.save(&path) {
                Ok(()) => {
                    self.file_path = Some(path.clone());
                    self.read_only = new_read_only;
                    if let Some(path_str) = path.to_str() {
                        self.syntax.set_language_from_path(path_str);
                    }
                    self.status_message = None;
                    self.refresh_grid_text_view();
                    Ok(())
                }
                Err(e) => {
                    self.status_message = Some((format!("Save as failed: {}", e), true));
                    Err(e)
                }
            };
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
        let path_obj = Path::new(path);
        if let Some(delim) = is_spreadsheet_ext(path_obj) {
            self.spreadsheet = Some(Spreadsheet::new_empty(delim));
        } else {
            self.spreadsheet = None;
        }
        self.grid_text_view = false;
        self.file_path = Some(PathBuf::from(path));
        self.syntax.set_language_from_path(path);
    }

    /// Ctrl+Y → Spreadsheet (CSV / TSV): show this text as a grid. Reads the
    /// file on disk when there are no unsaved edits (its exact bytes: the text
    /// editor turns tabs into spaces and folds curly quotes, which would change
    /// the data), else the text as shown. Refuses, with the reason, text that
    /// doesn't read as delimited data (see `Spreadsheet::from_text`).
    pub fn enter_grid_mode(&mut self, delimiter: u8) -> Result<(), String> {
        if self.spreadsheet.is_some() {
            // Already a grid (perhaps showing its text view): back to the grid.
            if self.grid_text_view {
                self.toggle_grid_text_view();
            }
            return Ok(());
        }
        let on_disk = self.file_path.clone().filter(|p| !self.modified && p.exists());
        // (An empty buffer has no tabs to lose: it becomes an empty grid.)
        if on_disk.is_none() && delimiter == b'\t' && !self.buffer.to_string().trim().is_empty() {
            // The text editor turned any tabs into spaces as they were typed,
            // pasted or loaded, so unsaved text can't hold TSV. Saving first
            // would write the spaces over the file's tabs, so say so.
            return Err(if self.file_path.as_ref().map_or(false, |p| p.exists()) {
                "the text editor turns tabs into spaces, so text with unsaved edits can't be read as TSV. \
                 Undo the edits or reopen the file without saving (saving would write the spaces), then try again"
                    .to_string()
            } else {
                "the text editor turns tabs into spaces, so text typed or pasted here can't be read as TSV. \
                 Open the data from a .tsv file instead"
                    .to_string()
            });
        }
        let text = match &on_disk {
            Some(path) => fs::read_to_string(path).map_err(|e| format!("the file couldn't be read ({})", e))?,
            None => self.buffer.to_string(),
        };
        let mut ss = Spreadsheet::from_text(&text, delimiter, true)?;
        if on_disk.is_none() && self.modified {
            ss.mark_unsaved(); // the text had unsaved edits; so does the grid
        }
        self.spreadsheet = Some(ss);
        self.grid_text_view = false;
        self.buffer = Buffer::new(); // the grid is the document now
        self.cursor = 0;
        self.selection_start = None;
        self.modified = false;
        self.viewport_offset = (0, 0);
        self.viewport_top_seg = 0;
        self.find_matches.clear();
        self.current_find_match = None;
        self.syntax = SyntaxHighlighter::new();
        self.syntax.set_language(if delimiter == b'\t' {
            crate::syntax::Language::Tsv
        } else {
            crate::syntax::Language::Csv
        });
        Ok(())
    }

    /// Ctrl+T on a CSV/TSV: switch between the grid and a read-only plain-text
    /// view of the file as it is on disk. The grid is kept, untouched, so
    /// switching back restores it exactly, unsaved edits and cursor included.
    pub fn toggle_grid_text_view(&mut self) {
        let Some(ss) = self.spreadsheet.as_mut() else {
            self.status_message = Some((
                "Ctrl+T switches a CSV/TSV file between grid and text view".to_string(),
                false,
            ));
            return;
        };
        if self.grid_text_view {
            self.grid_text_view = false;
            self.buffer = Buffer::new(); // the grid is the document; drop the copy
            self.selection_start = None;
            self.status_message = None;
            return;
        }
        if ss.is_editing() {
            ss.commit_edit();
        }
        let unsaved = ss.is_modified();
        self.grid_text_view = true;
        let loaded = self.load_grid_text_view();
        self.status_message = Some((
            match (loaded, unsaved) {
                (false, _) => "Text view: nothing saved on disk yet. Ctrl+T returns to the grid.",
                (true, true) => "Text view of the file on disk (read-only); unsaved grid edits are not shown, Ctrl+S saves them. Ctrl+T returns to the grid.",
                (true, false) => "Text view (read-only). Ctrl+T returns to the grid.",
            }
            .to_string(),
            false,
        ));
    }

    /// After the grid is saved from the text view, re-read the file so the view
    /// shows what was written, keeping the caret's line and the scroll position.
    fn refresh_grid_text_view(&mut self) {
        if !self.grid_text_view {
            return;
        }
        let line = self.buffer.byte_to_line(self.cursor);
        let viewport = self.viewport_offset;
        self.load_grid_text_view();
        let last_line = self.buffer.len_lines().saturating_sub(1);
        self.cursor = self.buffer.line_to_byte(line.min(last_line));
        self.viewport_offset = viewport;
    }

    /// Fill the buffer with the CSV/TSV file's text from disk for the text view.
    /// Returns false (and leaves the view empty) when there is no file to read.
    fn load_grid_text_view(&mut self) -> bool {
        let content = self
            .file_path
            .as_ref()
            .and_then(|p| fs::read_to_string(p).ok());
        let loaded = content.is_some();
        self.buffer = Buffer::from_string(content.unwrap_or_default());
        self.cursor = 0;
        self.selection_start = None;
        self.modified = false;
        self.viewport_offset = (0, 0);
        self.viewport_top_seg = 0;
        self.preferred_column = None;
        self.find_matches.clear();
        self.current_find_match = None;
        self.syntax = SyntaxHighlighter::new();
        if let Some(path) = self.file_path.as_ref().and_then(|p| p.to_str()) {
            self.syntax.set_language_from_path(path);
        }
        let line_count = self.buffer.len_lines();
        if line_count <= 50_000 {
            self.syntax.init_all_lines(line_count);
        }
        loaded
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::Command;
    use std::io::Write;

    fn csv_editor(contents: &str) -> (Editor, tempfile::NamedTempFile) {
        let mut tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        tmp.write_all(contents.as_bytes()).unwrap();
        tmp.flush().unwrap();
        let mut editor = Editor::new();
        editor.load_file(tmp.path().to_str().unwrap()).unwrap();
        assert!(editor.is_spreadsheet_mode());
        (editor, tmp)
    }

    #[test]
    fn text_view_shows_the_file_read_only_and_returns_to_the_same_grid() {
        let (mut editor, _tmp) = csv_editor("a,b\n1,2\n");
        {
            let ss = editor.spreadsheet_mut().unwrap();
            ss.rows[1][0] = "9".to_string(); // an unsaved grid edit
            ss.modified = true;
        }

        editor.toggle_grid_text_view();
        assert!(editor.is_grid_text_view());
        assert!(!editor.is_spreadsheet_mode());
        assert_eq!(editor.buffer().to_string(), "a,b\n1,2\n"); // the file on disk
        assert!(editor.is_modified()); // the grid edit is still pending

        // Every edit path is refused.
        editor.execute(Command::InsertChar('x')).unwrap();
        editor.execute(Command::Backspace).unwrap();
        editor.paste_text("zz".to_string());
        editor.replace_at(0, 1, "q");
        assert_eq!(editor.buffer().to_string(), "a,b\n1,2\n");
        assert!(editor.status_message.as_ref().unwrap().0.contains("read-only"));

        editor.toggle_grid_text_view();
        assert!(editor.is_spreadsheet_mode());
        let ss = editor.spreadsheet().unwrap();
        assert_eq!(ss.cell(1, 0), "9");
        assert!(ss.is_modified());
    }

    #[test]
    fn saving_from_the_text_view_writes_the_grid_and_refreshes_the_view() {
        let (mut editor, tmp) = csv_editor("a,b\n1,2\n");
        {
            let ss = editor.spreadsheet_mut().unwrap();
            ss.rows[1][0] = "9".to_string();
            ss.modified = true;
        }
        editor.toggle_grid_text_view();
        editor.save().unwrap();
        assert_eq!(std::fs::read_to_string(tmp.path()).unwrap(), "a,b\n9,2\n");
        assert_eq!(editor.buffer().to_string(), "a,b\n9,2\n");
        assert!(editor.is_grid_text_view());
        assert!(!editor.is_modified());
    }

    fn text_editor(suffix: &str, contents: &str) -> (Editor, tempfile::NamedTempFile) {
        let mut tmp = tempfile::Builder::new().suffix(suffix).tempfile().unwrap();
        tmp.write_all(contents.as_bytes()).unwrap();
        tmp.flush().unwrap();
        let mut editor = Editor::new();
        editor.load_file(tmp.path().to_str().unwrap()).unwrap();
        assert!(!editor.is_spreadsheet_mode());
        (editor, tmp)
    }

    #[test]
    fn a_text_file_switches_to_a_grid_from_its_exact_bytes() {
        // Tabs survive because the grid reads the file, not the text editor's
        // copy (which turned them into spaces).
        let (mut editor, tmp) = text_editor(".txt", "id\tname\n1\tAda\n");
        assert!(!editor.buffer().to_string().contains('\t'));
        editor.enter_grid_mode(b'\t').unwrap();
        assert!(editor.is_spreadsheet_mode());
        assert!(!editor.is_modified());
        assert_eq!(editor.spreadsheet().unwrap().cell(1, 1), "Ada");
        // Save writes it back as TSV.
        editor.save().unwrap();
        assert_eq!(std::fs::read_to_string(tmp.path()).unwrap(), "id\tname\n1\tAda\n");
    }

    #[test]
    fn unsaved_text_becomes_an_unsaved_grid() {
        let (mut editor, _tmp) = text_editor(".txt", "a,b\n");
        editor.execute(Command::MoveEnd).unwrap();
        editor.paste_text("\n1,2".to_string());
        assert!(editor.is_modified());
        editor.enter_grid_mode(b',').unwrap();
        let ss = editor.spreadsheet().unwrap();
        assert_eq!(ss.cell(1, 1), "2");
        assert!(editor.is_modified());
    }

    #[test]
    fn an_empty_buffer_switches_to_an_empty_grid_as_csv_or_tsv() {
        for delim in [b',', b'\t'] {
            let mut editor = Editor::new(); // no file, nothing typed
            editor.enter_grid_mode(delim).unwrap();
            assert!(editor.is_spreadsheet_mode());
            assert_eq!(editor.spreadsheet().unwrap().num_cols(), 0);
        }
        let (mut editor, _tmp) = text_editor(".txt", "");
        editor.enter_grid_mode(b'\t').unwrap(); // an empty file on disk too
        assert_eq!(editor.spreadsheet().unwrap().num_cols(), 0);
    }

    #[test]
    fn unsaved_text_is_refused_as_tsv_with_the_real_reason() {
        let (mut editor, _tmp) = text_editor(".txt", "id\tname\n1\tAda\n");
        editor.paste_text("x".to_string());
        let why = editor.enter_grid_mode(b'\t').unwrap_err();
        assert!(why.contains("tabs into spaces") && why.contains("without saving"), "{why}");
        assert!(!editor.is_spreadsheet_mode());
    }

    #[test]
    fn text_that_isnt_a_spreadsheet_is_refused_and_stays_text() {
        let (mut editor, _tmp) = text_editor(".txt", "a,b\n1,2,3\n");
        let why = editor.enter_grid_mode(b',').unwrap_err();
        assert!(why.contains("header row has only 2"), "{why}");
        assert!(!editor.is_spreadsheet_mode());
        assert_eq!(editor.buffer().to_string(), "a,b\n1,2,3\n");
    }

    #[test]
    fn text_view_toggle_is_a_no_op_outside_csv() {
        let mut editor = Editor::new();
        editor.toggle_grid_text_view();
        assert!(!editor.is_grid_text_view());
        assert!(editor.status_message.is_some());
    }
}
