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
        self.text_view_origin = None;
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
        self.mark_text_clean();
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

        self.auto_setup_repl(path);

        Ok(())
    }

    /// Enable REPL mode and auto-connect a kernel based on the file extension.
    /// .sql -> Snowflake (if configured); .py/.pyw -> Python interpreter;
    /// .sh/.bash/.zsh -> persistent shell session. Called both when loading an
    /// existing file and when opening a path that doesn't exist yet (a new
    /// script), so Ctrl+E works the same in either case.
    fn auto_setup_repl(&mut self, path: &str) {
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
            } else if ext == "sh" || ext == "bash" || ext == "zsh" {
                self.enable_repl_mode();
                if !self.is_kernel_connected() {
                    self.try_connect_shell_kernel(path);
                }
            }
        }
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

    /// Connect a persistent shell session for a shell script. The shell is
    /// picked from the script's shebang when one names a discovered shell,
    /// then the file's extension (`.zsh` prefers zsh), then whatever
    /// discovery ranked first (bash before zsh before sh). Silently requires
    /// nothing beyond a shell on PATH; Ctrl+K can still swap it.
    fn try_connect_shell_kernel(&mut self, path: &str) {
        let kernels = crate::shell_kernel::discover_shell_kernels();
        if kernels.is_empty() {
            self.status_message = Some((
                "No shell found on PATH (looked for bash, zsh, sh)".to_string(),
                true,
            ));
            return;
        }

        // Basename the shebang interpreter ("/usr/bin/env zsh" and
        // "/bin/zsh" both come back as a name/path from detect_shebang).
        let shebang_shell = self.detect_shebang().and_then(|interp| {
            Path::new(&interp)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        });
        let ext_shell = Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .filter(|e| *e == "bash" || *e == "zsh")
            .map(str::to_string);

        let wanted = shebang_shell.or(ext_shell);
        let info = wanted
            .and_then(|name| {
                kernels
                    .iter()
                    .find(|k| k.display_name == format!("Shell ({})", name))
                    .cloned()
            })
            .unwrap_or_else(|| kernels[0].clone());

        // The file itself may not exist yet (a new script opened by path), so
        // resolve the directory rather than the file. An empty parent means
        // a bare relative filename — that's the current directory.
        let workdir = {
            let parent = Path::new(path).parent().filter(|p| !p.as_os_str().is_empty());
            let dir = parent.unwrap_or(Path::new("."));
            std::fs::canonicalize(dir).ok()
        };
        let mut kernel = crate::shell_kernel::ShellKernel::new(
            info.python_path.clone(),
            info.display_name.clone(),
        )
        .with_workdir(workdir);

        match kernel.connect() {
            Ok(()) => {
                let display = info.display_name.clone();
                self.set_kernel(Box::new(kernel));
                self.status_message =
                    Some((format!("Auto-connected to {}", display), false));
            }
            Err(e) => {
                self.status_message = Some((
                    format!("Shell auto-connect failed: {} (press Ctrl+K to pick another shell)", e),
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

        // A grid saves itself; its text view saves the text, as typed.
        if !self.grid_text_view {
            if let Some(ss) = self.spreadsheet.as_mut() {
                return match ss.save(&path) {
                    Ok(()) => {
                        self.status_message = None;
                        Ok(())
                    }
                    Err(e) => {
                        self.status_message = Some((format!("Save failed: {}", e), true));
                        Err(e)
                    }
                };
            }
        }

        match fs::write(&path, self.buffer.to_string()) {
            Ok(_) => {
                self.mark_text_clean();
                self.last_saved_undo_len = 0; // Reset save point
                self.status_message = None; // Clear any error messages
                // The parked grid no longer matches the file: going back to the
                // grid reads the saved text instead.
                self.text_view_origin = None;
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

        if !self.grid_text_view {
            if let Some(ss) = self.spreadsheet.as_mut() {
                return match ss.save(&path) {
                    Ok(()) => {
                        self.file_path = Some(path.clone());
                        self.read_only = new_read_only;
                        if let Some(path_str) = path.to_str() {
                            self.syntax.set_language_from_path(path_str);
                        }
                        self.status_message = None;
                        Ok(())
                    }
                    Err(e) => {
                        self.status_message = Some((format!("Save as failed: {}", e), true));
                        Err(e)
                    }
                };
            }
        }

        match fs::write(&path, self.buffer.to_string()) {
            Ok(_) => {
                self.file_path = Some(path.clone());
                self.mark_text_clean();
                self.last_saved_undo_len = 0; // Reset save point
                self.read_only = new_read_only;
                self.status_message = None; // Clear any error messages
                self.text_view_origin = None;
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
        self.text_view_origin = None;
        self.file_path = Some(PathBuf::from(path));
        self.syntax.set_language_from_path(path);
        self.auto_setup_repl(path);
    }

    /// Ctrl+Y → Spreadsheet (CSV / TSV): show this text as a grid. Reads the
    /// file on disk when there are no unsaved edits (its exact bytes: the text
    /// editor turns tabs into spaces and folds curly quotes, which would change
    /// the data), else the text as shown. Refuses, with the reason, text that
    /// doesn't read as delimited data (see `Spreadsheet::from_text`).
    ///
    /// From a grid's text view: if the text is as the view opened it, the
    /// parked grid comes back as it was (cursor, filters, undo history);
    /// otherwise the text is read into a new grid, refused (staying in the
    /// text) if the edits broke it. On a grid: the same delimiter does
    /// nothing; the other one reads the file again with it, unless the grid
    /// has unsaved edits (they would be lost). A grid is only replaced once
    /// the new reading succeeds.
    pub fn enter_grid_mode(&mut self, delimiter: u8) -> Result<(), String> {
        let language = if delimiter == b'\t' { crate::syntax::Language::Tsv } else { crate::syntax::Language::Csv };
        let kind = if delimiter == b'\t' { "TSV" } else { "CSV" };
        if self.grid_text_view {
            let text = self.buffer.to_string();
            let unchanged = self.text_view_origin.as_deref() == Some(text.as_str())
                && self.spreadsheet.as_ref().map_or(false, |ss| ss.delimiter == delimiter);
            if unchanged {
                self.grid_text_view = false;
                self.text_view_origin = None;
                self.buffer = Buffer::new(); // the parked grid is the document again
                self.selection_start = None;
                self.status_message = None;
                self.syntax.set_language(language);
                return Ok(());
            }
            // The text's tabs and quotes are intact (the view keeps it raw).
            let mut ss = Spreadsheet::from_text(&text, delimiter, true)?;
            if self.modified {
                ss.mark_unsaved();
            }
            self.install_grid(ss, language);
            return Ok(());
        }
        let had_grid = match &self.spreadsheet {
            Some(ss) if ss.delimiter == delimiter => {
                self.syntax.set_language(language);
                return Ok(());
            }
            Some(ss) if ss.is_modified() => {
                return Err(format!(
                    "the grid has unsaved edits; save (Ctrl+S) or undo them before reading the file as {}",
                    kind
                ));
            }
            Some(_) => true,
            None => false,
        };
        // A grid is re-read from its file; text from the file unless it has unsaved edits.
        let on_disk = self.file_path.clone().filter(|p| (had_grid || !self.modified) && p.exists());
        // (An empty buffer has no tabs to lose: it becomes an empty grid.)
        if !had_grid && on_disk.is_none() && delimiter == b'\t' && !self.buffer.to_string().trim().is_empty() {
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
            // A grid never saved to disk (and unmodified) is empty.
            None if had_grid => String::new(),
            None => self.buffer.to_string(),
        };
        let mut ss = Spreadsheet::from_text(&text, delimiter, true)?;
        if !had_grid && on_disk.is_none() && self.modified {
            ss.mark_unsaved(); // the text had unsaved edits; so does the grid
        }
        self.install_grid(ss, language);
        Ok(())
    }

    /// Make `ss` the document, shown as a grid.
    fn install_grid(&mut self, ss: Spreadsheet, language: crate::syntax::Language) {
        self.spreadsheet = Some(ss);
        self.grid_text_view = false;
        self.text_view_origin = None;
        self.buffer = Buffer::new(); // the grid is the document now
        self.cursor = 0;
        self.selection_start = None;
        self.modified = false;
        self.viewport_offset = (0, 0);
        self.viewport_top_seg = 0;
        self.find_matches.clear();
        self.current_find_match = None;
        self.syntax = SyntaxHighlighter::new();
        self.syntax.set_language(language);
    }

    /// Ctrl+Y → a text language (Plain Text, say) on a grid: show its data as
    /// text to edit. The text is the file as on disk (its exact bytes) when the
    /// grid has no unsaved edits, else what Save would write, so the edits
    /// carry over. It stays raw: typing and pasting insert exactly what is
    /// typed, Tab types a tab (drawn as →), and Save writes the text. Ctrl+Y,
    /// Spreadsheet reads it back into a grid. Already showing the text, this
    /// does nothing.
    ///
    /// The editor's lines must be the data's records, so record ends become
    /// `\n` (CRLF and lone CR read the same). Refuses, staying in the grid,
    /// data that can't be shown as lines without changing a value: a carriage
    /// return inside a quoted value, or a character the editor breaks lines on
    /// that the data reads as part of a field (U+2028, say).
    pub fn show_grid_text_view(&mut self) -> Result<(), String> {
        let Some(ss) = self.spreadsheet.as_mut() else { return Ok(()) };
        if self.grid_text_view {
            return Ok(());
        }
        if ss.is_editing() {
            ss.commit_edit();
        }
        let unsaved = ss.is_modified();
        let kind = ss.delimiter_name();
        let from_disk = if unsaved { None } else { self.file_path.as_ref().and_then(|p| fs::read_to_string(p).ok()) };
        let text = from_disk.unwrap_or_else(|| ss.to_text());
        let text = text.strip_prefix('\u{FEFF}').unwrap_or(&text);
        let text = crate::dsv::records_to_lf(text).map_err(|(line, ch)| {
            let what = match ch {
                '\r' => "a carriage return inside a quoted value".to_string(),
                '\u{000B}' => "a vertical tab".to_string(),
                '\u{000C}' => "a form feed".to_string(),
                '\u{0085}' => "a next-line character (U+0085)".to_string(),
                '\u{2028}' => "a line separator (U+2028)".to_string(),
                '\u{2029}' => "a paragraph separator (U+2029)".to_string(),
                c => format!("U+{:04X}", c as u32),
            };
            format!(
                "line {} holds {}, which the text view can't show without changing the value. \
                 Stayed in the grid",
                line, what
            )
        })?;
        self.buffer = Buffer::from_raw(&text);
        self.text_view_origin = Some(self.buffer.to_string());
        // The disk's text, unless it came from unsaved grid edits (then undo
        // can never make it match the disk).
        self.clean_text_hash = (!unsaved).then(|| self.buffer.content_hash());
        self.grid_text_view = true;
        self.modified = unsaved; // text written from unsaved grid edits isn't on disk yet
        self.cursor = 0;
        self.selection_start = None;
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
        self.status_message = Some((
            format!(
                "{} data as text, kept exactly (tabs show as \u{2192}). Ctrl+Y, Spreadsheet reads it back into a grid.",
                kind
            ),
            false,
        ));
        Ok(())
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

    fn type_text(editor: &mut Editor, text: &str) {
        for c in text.chars() {
            let cmd = match c {
                '\n' => Command::InsertNewline,
                '\t' => Command::InsertTab,
                c => Command::InsertChar(c),
            };
            editor.execute(cmd).unwrap();
        }
    }

    #[test]
    fn unsaved_grid_edits_carry_into_the_text_and_unchanged_text_brings_the_grid_back() {
        let (mut editor, _tmp) = csv_editor("a,b\n1,2\n");
        {
            let ss = editor.spreadsheet_mut().unwrap();
            ss.cursor = (1, 0);
            ss.enter_edit_mode_replace('9'); // an unsaved grid edit, with an undo step
            ss.commit_edit();
        }
        editor.show_grid_text_view().unwrap();
        assert!(editor.is_grid_text_view() && !editor.is_spreadsheet_mode());
        assert_eq!(editor.buffer().to_string(), "a,b\n9,2\n"); // what Save would write
        assert!(editor.is_modified());

        editor.enter_grid_mode(b',').unwrap(); // the text wasn't touched
        let ss = editor.spreadsheet_mut().unwrap();
        assert_eq!(ss.cell(1, 0), "9");
        assert!(ss.undo()); // the same grid, undo history and all
        assert_eq!(ss.cell(1, 0), "1");
    }

    #[test]
    fn text_view_edits_stay_exact_and_read_back_into_a_grid() {
        let mut tmp = tempfile::Builder::new().suffix(".tsv").tempfile().unwrap();
        tmp.write_all("id\tname\n1\tAda\n".as_bytes()).unwrap();
        tmp.flush().unwrap();
        let mut editor = Editor::new();
        editor.load_file(tmp.path().to_str().unwrap()).unwrap();

        editor.show_grid_text_view().unwrap();
        assert_eq!(editor.buffer().to_string(), "id\tname\n1\tAda\n"); // tabs intact
        editor.execute(Command::MoveDown).unwrap();
        editor.execute(Command::MoveDown).unwrap();
        // A tab and a curly quote typed in go in exactly; a new line isn't indented.
        type_text(&mut editor, "2\tBo\u{201d}");
        editor.paste_text("\n3\t\u{201c}Cy\u{201d}".to_string());
        assert_eq!(editor.buffer().to_string(), "id\tname\n1\tAda\n2\tBo\u{201d}\n3\t\u{201c}Cy\u{201d}");

        editor.enter_grid_mode(b'\t').unwrap(); // edited, so the text is read again
        let ss = editor.spreadsheet().unwrap();
        assert_eq!((ss.cell(2, 0), ss.cell(2, 1)), ("2", "Bo\u{201d}"));
        assert_eq!(ss.cell(3, 1), "\u{201c}Cy\u{201d}");
        assert!(editor.is_modified()); // not saved yet
        editor.save().unwrap();
        assert_eq!(
            std::fs::read_to_string(tmp.path()).unwrap(),
            "id\tname\n1\tAda\n2\tBo\u{201d}\n3\t\u{201c}Cy\u{201d}\n"
        );
    }

    #[test]
    fn saving_from_the_text_view_writes_the_text_as_typed() {
        let (mut editor, tmp) = csv_editor("a,b\r\n1,2\r\n");
        editor.show_grid_text_view().unwrap();
        editor.execute(Command::MoveDown).unwrap();
        editor.execute(Command::MoveDown).unwrap();
        type_text(&mut editor, "3,4");
        editor.save().unwrap();
        assert_eq!(std::fs::read_to_string(tmp.path()).unwrap(), "a,b\n1,2\n3,4");
        assert!(!editor.is_modified());
        editor.enter_grid_mode(b',').unwrap();
        assert_eq!(editor.spreadsheet().unwrap().cell(2, 1), "4");
        assert!(!editor.is_modified()); // the grid matches the saved file
    }

    #[test]
    fn text_edits_that_break_the_data_are_refused_and_stay_as_text() {
        let (mut editor, _tmp) = csv_editor("a,b\n1,2\n");
        editor.show_grid_text_view().unwrap();
        editor.execute(Command::MoveDown).unwrap();
        editor.execute(Command::MoveEnd).unwrap();
        type_text(&mut editor, ",3");
        let why = editor.enter_grid_mode(b',').unwrap_err();
        assert!(why.contains("header row has only 2"), "{why}");
        assert!(editor.is_grid_text_view());
        assert_eq!(editor.buffer().to_string(), "a,b\n1,2,3\n");
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
    fn a_text_language_shows_the_grid_as_text_and_spreadsheet_brings_it_back() {
        let (mut editor, _tmp) = csv_editor("a,b\n1,2\n");
        editor.show_grid_text_view().unwrap();
        assert!(editor.is_grid_text_view());
        editor.set_language(crate::syntax::Language::PlainText); // what Ctrl+Y, Plain Text does
        editor.show_grid_text_view().unwrap(); // already there: nothing changes
        assert!(editor.is_grid_text_view());
        editor.enter_grid_mode(b',').unwrap();
        assert!(editor.is_spreadsheet_mode());
        assert_eq!(*editor.get_language(), crate::syntax::Language::Csv);
    }

    #[test]
    fn choosing_the_other_delimiter_rereads_the_file_unless_the_grid_has_edits() {
        let (mut editor, _tmp) = csv_editor("a\tb,c\n1\t2,3\n");
        assert_eq!(editor.spreadsheet().unwrap().cell(0, 1), "c"); // as CSV
        editor.enter_grid_mode(b'\t').unwrap();
        assert_eq!(editor.spreadsheet().unwrap().cell(0, 1), "b,c"); // as TSV
        assert_eq!(*editor.get_language(), crate::syntax::Language::Tsv);
        {
            let ss = editor.spreadsheet_mut().unwrap();
            ss.cursor = (1, 0);
            ss.enter_edit_mode_replace('9');
            ss.commit_edit();
        }
        let why = editor.enter_grid_mode(b',').unwrap_err();
        assert!(why.contains("unsaved edits"), "{why}");
        assert_eq!(editor.spreadsheet().unwrap().cell(1, 0), "9"); // the edit is kept
    }

    #[test]
    fn a_rereading_that_fails_keeps_the_grid() {
        let (mut editor, _tmp) = csv_editor("x,y\n1,2\n");
        assert!(editor.enter_grid_mode(b'\t').is_err()); // no tabs in the file
        assert!(editor.is_spreadsheet_mode());
        assert_eq!(editor.spreadsheet().unwrap().cell(1, 1), "2");
    }

    #[test]
    fn text_view_is_a_no_op_outside_csv() {
        let mut editor = Editor::new();
        editor.show_grid_text_view().unwrap();
        assert!(!editor.is_grid_text_view());
    }

    #[test]
    fn undo_in_a_text_view_built_from_unsaved_grid_edits_stays_unsaved() {
        let (mut editor, _tmp) = csv_editor("a,b\n1,2\n");
        {
            let ss = editor.spreadsheet_mut().unwrap();
            ss.cursor = (1, 0);
            ss.enter_edit_mode_replace('9');
            ss.commit_edit();
        }
        editor.show_grid_text_view().unwrap(); // "a,b\n9,2\n", not on disk
        type_text(&mut editor, "x");
        editor.execute(Command::Undo).unwrap();
        assert_eq!(editor.buffer().to_string(), "a,b\n9,2\n");
        assert!(editor.is_modified()); // the grid edit still isn't saved
    }

    #[test]
    fn undo_tracks_the_saved_text_not_the_undo_stack() {
        let (mut editor, tmp) = csv_editor("a,b\n1,2\n");
        editor.show_grid_text_view().unwrap();
        type_text(&mut editor, "x");
        editor.execute(Command::Undo).unwrap();
        assert!(!editor.is_modified()); // back to the file as on disk
        editor.execute(Command::Redo).unwrap();
        assert!(editor.is_modified());
        editor.save().unwrap();
        assert!(!editor.is_modified());
        editor.execute(Command::Undo).unwrap(); // past the save
        assert_eq!(std::fs::read_to_string(tmp.path()).unwrap(), "xa,b\n1,2\n");
        assert!(editor.is_modified());
        editor.execute(Command::Redo).unwrap(); // back to the save
        assert!(!editor.is_modified());
    }

    #[test]
    fn a_carriage_return_inside_a_quoted_value_keeps_the_grid() {
        let (mut editor, _tmp) = csv_editor("a,b\r\n\"x\r\ny\",2\r\n");
        let why = editor.show_grid_text_view().unwrap_err();
        assert!(why.contains("line 2") && why.contains("carriage return"), "{why}");
        assert!(editor.is_spreadsheet_mode());
        assert_eq!(editor.spreadsheet().unwrap().cell(1, 0), "x\r\ny");
    }

    #[test]
    fn a_line_separator_in_a_value_keeps_the_grid() {
        let (mut editor, _tmp) = csv_editor("a,b\n1,x\u{2028}y\n");
        let why = editor.show_grid_text_view().unwrap_err();
        assert!(why.contains("U+2028"), "{why}");
        assert!(editor.is_spreadsheet_mode());
    }

    #[test]
    fn lone_carriage_return_records_become_lines() {
        let (mut editor, _tmp) = csv_editor("a,b\r1,2\r3,4");
        editor.show_grid_text_view().unwrap();
        assert_eq!(editor.buffer().to_string(), "a,b\n1,2\n3,4");
        assert_eq!(editor.buffer().len_lines(), 3);
        editor.enter_grid_mode(b',').unwrap(); // unchanged: the same grid comes back
        assert_eq!(editor.spreadsheet().unwrap().cell(2, 1), "4");
    }

    #[test]
    fn pasting_into_the_text_view_keeps_lines_as_records() {
        let (mut editor, _tmp) = csv_editor("a,b\n");
        editor.show_grid_text_view().unwrap();
        editor.execute(Command::MoveDown).unwrap();
        editor.paste_text("1,2\r3,4".to_string());
        assert_eq!(editor.buffer().to_string(), "a,b\n1,2\n3,4");
        editor.paste_text("5\u{2028}6".to_string());
        assert_eq!(editor.buffer().to_string(), "a,b\n1,2\n3,4"); // refused
        assert!(editor.status_message.as_ref().map_or(false, |(m, _)| m.contains("U+2028")));
    }

    #[test]
    fn find_lines_up_after_a_character_that_lowercases_longer() {
        let mut editor = Editor::new();
        editor.paste_text("\u{130}x abc ABC".to_string()); // 'İ' lowercases to 2 chars
        assert_eq!(editor.find_all("abc"), vec![(4, 7), (8, 11)]);
        assert_eq!(editor.find_all("\u{130}x"), vec![(0, 3)]);
        // Greek capital sigma: the search and the buffer lowercase alike.
        let mut editor = Editor::new();
        editor.paste_text("\u{39F}\u{3A3} \u{3BF}\u{3C3}".to_string());
        assert_eq!(editor.find_all("\u{39F}\u{3A3}").len(), 2);
    }
}
