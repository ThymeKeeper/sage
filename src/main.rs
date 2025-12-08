mod buffer;
mod editor;
mod renderer;
mod commands;
mod prompt;
mod find_replace;
mod exit_prompt;
mod syntax;
mod kernel;
mod direct_kernel;
mod cell;
mod kernel_selector;
mod output_pane;
mod autocomplete;
mod event_loop;

use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers, MouseEventKind, MouseButton, EnableBracketedPaste, DisableBracketedPaste},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};
use std::io::{self, Write};

fn debug_log(msg: &str) {
    use std::fs::OpenOptions;
    if let Ok(mut log) = OpenOptions::new().create(true).append(true).open("/tmp/sage_debug.log") {
        let _ = writeln!(log, "{}", msg);
        let _ = log.flush();
    }
}

fn main() -> io::Result<()> {
    debug_log("=== SAGE DEBUG LOG ===");

    // Check if we're running in a terminal
    if let Err(_) = enable_raw_mode() {
        // No terminal available - relaunch in a terminal emulator
        let args: Vec<String> = std::env::args().collect();
        let program_path = &args[0];
        let file_args: Vec<&str> = args[1..].iter().map(|s| s.as_str()).collect();
        
        // Build full command with all arguments
        let mut full_command = vec![program_path.as_str()];
        full_command.extend(&file_args);
        
        // Method 1: Check environment variable
        if let Ok(terminal) = std::env::var("TERMINAL") {
            if launch_in_terminal(&terminal, &full_command) {
                return Ok(());
            }
        }
        
        // Method 2: Check Cinnamon desktop settings (Linux Mint)
        if let Ok(output) = std::process::Command::new("gsettings")
            .args(&["get", "org.cinnamon.desktop.default-applications.terminal", "exec"])
            .output() 
        {
            if output.status.success() {
                let terminal = String::from_utf8_lossy(&output.stdout).trim().trim_matches('\'').to_string();
                if !terminal.is_empty() && launch_in_terminal(&terminal, &full_command) {
                    return Ok(());
                }
            }
        }
        
        // Method 3: Check GNOME desktop settings
        if let Ok(output) = std::process::Command::new("gsettings")
            .args(&["get", "org.gnome.desktop.default-applications.terminal", "exec"])
            .output() 
        {
            if output.status.success() {
                let terminal = String::from_utf8_lossy(&output.stdout).trim().trim_matches('\'').to_string();
                if !terminal.is_empty() && terminal != "x-terminal-emulator" {
                    if launch_in_terminal(&terminal, &full_command) {
                        return Ok(());
                    }
                }
            }
        }
        
        // Method 4: Use x-terminal-emulator (Debian/Ubuntu standard)
        if launch_in_terminal("x-terminal-emulator", &full_command) {
            return Ok(());
        }
        
        // Method 5: Try sensible-terminal
        if launch_in_terminal("sensible-terminal", &full_command) {
            return Ok(());
        }
        
        // Method 6: Fallback to common terminal emulators
        let fallback_terminals = ["gnome-terminal", "konsole", "xfce4-terminal", "xterm", "mate-terminal", "alacritty", "kitty", "terminator"];
        for terminal in &fallback_terminals {
            if launch_in_terminal(terminal, &full_command) {
                return Ok(());
            }
        }
        
        eprintln!("Error: Not running in a terminal and no terminal emulator found");
        return Err(io::Error::new(io::ErrorKind::Other, "No terminal available"));
    }
    
    // If we get here, we're in a terminal - continue with normal setup
    
    // Enable mouse support
    execute!(io::stdout(), crossterm::event::EnableMouseCapture)?;
    
    // Enable bracketed paste mode
    execute!(io::stdout(), event::EnableBracketedPaste)?;

    // Enable enhanced keyboard protocol for better key combination support
    // This helps disambiguate Ctrl+Backspace from Ctrl+H
    if let Ok(_) = execute!(
        io::stdout(),
        crossterm::event::PushKeyboardEnhancementFlags(
            crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | crossterm::event::KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
                | crossterm::event::KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
        )
    ) {
        // Enhanced keyboard mode enabled successfully
    }
    
    let mut editor = editor::Editor::new();
    let mut renderer = renderer::Renderer::new()?;

    // Set initial help message
    editor.status_message = Some(("Press Ctrl+K to select kernel, Ctrl+E to execute cell".to_string(), false));
    
    // Load file if provided
    if let Some(path) = std::env::args().nth(1) {
        match editor.load_file(&path) {
            Ok(_) => {},
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // File doesn't exist - create new file with this path
                editor.set_file_path(&path);
            },
            Err(e) => {
                eprintln!("Failed to load file: {}", e);
            }
        }
    }
    
    // Initialize viewport to follow cursor
    editor.update_viewport_for_cursor();
    
    // Main loop
    let result = event_loop::run(&mut editor, &mut renderer);
    
    // Cleanup
    renderer.cleanup()?;
    execute!(io::stdout(), crossterm::event::DisableMouseCapture)?;
    execute!(io::stdout(), event::DisableBracketedPaste)?;
    // Disable enhanced keyboard protocol
    let _ = execute!(io::stdout(), crossterm::event::PopKeyboardEnhancementFlags);
    disable_raw_mode()?;
    
    if let Err(e) = result {
        eprintln!("Error: {}", e);
    }
    
    Ok(())
}

fn launch_in_terminal(terminal: &str, command: &[&str]) -> bool {
    // First check if the terminal exists
    if let Ok(output) = std::process::Command::new("which").arg(terminal).output() {
        if !output.status.success() {
            return false;
        }
    } else {
        return false;
    }
    
    // Different terminals have different command line formats
    let result = match terminal {
        // Terminals that use -- to separate their args from the command
        "gnome-terminal" | "mate-terminal" | "tilix" => {
            let mut args = vec!["--"];
            args.extend_from_slice(command);
            std::process::Command::new(terminal).args(&args).spawn()
        },
        // Terminals that use -e for execute
        "xterm" | "konsole" | "xfce4-terminal" | "lxterminal" | "sakura" | "roxterm" => {
            let mut args = vec!["-e"];
            args.extend_from_slice(command);
            std::process::Command::new(terminal).args(&args).spawn()
        },
        // Terminals that take the command directly
        "alacritty" | "kitty" | "foot" => {
            std::process::Command::new(terminal).args(command).spawn()
        },
        // Terminator uses -x
        "terminator" => {
            let mut args = vec!["-x"];
            args.extend_from_slice(command);
            std::process::Command::new(terminal).args(&args).spawn()
        },
        // For x-terminal-emulator and sensible-terminal, try -e format
        "x-terminal-emulator" | "sensible-terminal" => {
            let mut args = vec!["-e"];
            args.extend_from_slice(command);
            std::process::Command::new(terminal).args(&args).spawn()
        },
        // Default: try direct command
        _ => {
            std::process::Command::new(terminal).args(command).spawn()
        }
    };
    
    result.is_ok()
}

