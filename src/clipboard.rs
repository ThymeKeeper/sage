use std::io::{self, Write};

/// A unified clipboard interface that works both locally and over SSH
pub enum ClipboardProvider {
    /// Native clipboard (X11/Wayland) via arboard
    Native(arboard::Clipboard),
    /// OSC 52 terminal escape sequences (works over SSH to local clipboard)
    Osc52,
    /// No clipboard available
    None,
}

impl ClipboardProvider {
    /// Create a new clipboard provider, trying methods in order of preference
    pub fn new() -> Self {
        // Try native clipboard first (works locally with X11/Wayland)
        if let Ok(clipboard) = arboard::Clipboard::new() {
            return ClipboardProvider::Native(clipboard);
        }

        // Fall back to OSC 52 (works over SSH, syncs to local clipboard)
        // Check if we're in a terminal that likely supports OSC 52
        if Self::is_osc52_likely_supported() {
            return ClipboardProvider::Osc52;
        }

        // No clipboard available
        ClipboardProvider::None
    }

    /// Check if OSC 52 is likely to be supported
    fn is_osc52_likely_supported() -> bool {
        // Check for SSH connection
        let in_ssh = std::env::var("SSH_CONNECTION").is_ok()
                  || std::env::var("SSH_CLIENT").is_ok()
                  || std::env::var("SSH_TTY").is_ok();

        // Check for terminals known to support OSC 52
        let term = std::env::var("TERM").unwrap_or_default();
        let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();

        let known_support = term.contains("xterm")
            || term.contains("screen")
            || term.contains("tmux")
            || term_program.contains("iTerm")
            || term_program.contains("WezTerm")
            || term_program.contains("kitty")
            || term_program.contains("Alacritty");

        // Use OSC 52 if we're in SSH or in a supported terminal
        in_ssh || known_support
    }

    /// Set clipboard text
    pub fn set_text(&mut self, text: String) -> Result<(), String> {
        match self {
            ClipboardProvider::Native(clipboard) => {
                clipboard.set_text(text).map_err(|e| e.to_string())
            }
            ClipboardProvider::Osc52 => {
                Self::osc52_copy(&text)
            }
            ClipboardProvider::None => {
                Err("No clipboard available".to_string())
            }
        }
    }

    /// Get clipboard text
    pub fn get_text(&mut self) -> Result<String, String> {
        match self {
            ClipboardProvider::Native(clipboard) => {
                clipboard.get_text().map_err(|e| e.to_string())
            }
            ClipboardProvider::Osc52 => {
                // OSC 52 reading is not widely supported, so we can't reliably implement this
                Err("Reading from clipboard via OSC 52 is not supported".to_string())
            }
            ClipboardProvider::None => {
                Err("No clipboard available".to_string())
            }
        }
    }

    /// Copy text to clipboard using OSC 52 escape sequence
    /// Format: ESC ] 52 ; c ; <base64> BEL
    /// This writes to the LOCAL clipboard even when running over SSH
    fn osc52_copy(text: &str) -> Result<(), String> {
        use base64::{Engine as _, engine::general_purpose::STANDARD};

        let encoded = STANDARD.encode(text.as_bytes());

        // Check if we're in tmux (requires special wrapping)
        let in_tmux = std::env::var("TMUX").is_ok();

        let mut stdout = io::stdout();

        if in_tmux {
            // tmux requires DCS wrapping: ESC P tmux; ESC <osc52_sequence> ESC \ ESC \
            write!(stdout, "\x1bPtmux;\x1b\x1b]52;c;{}\x07\x1b\\", encoded)
                .map_err(|e| e.to_string())?;
        } else {
            // Standard OSC 52: ESC ] 52 ; c ; <base64> BEL
            write!(stdout, "\x1b]52;c;{}\x07", encoded)
                .map_err(|e| e.to_string())?;
        }

        stdout.flush().map_err(|e| e.to_string())?;

        Ok(())
    }

    /// Check if this clipboard provider is available
    pub fn is_available(&self) -> bool {
        !matches!(self, ClipboardProvider::None)
    }

    /// Get a description of the clipboard provider
    pub fn description(&self) -> &str {
        match self {
            ClipboardProvider::Native(_) => "native (X11/Wayland)",
            ClipboardProvider::Osc52 => "OSC 52 (terminal)",
            ClipboardProvider::None => "none",
        }
    }
}
