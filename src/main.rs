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
mod sql_context;

use kernel::Kernel;

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

/// Parse the shebang line from a Python file
fn parse_shebang(file_path: &str) -> Option<String> {
    use std::fs::File;
    use std::io::BufRead;

    let file = File::open(file_path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let mut first_line = String::new();

    reader.read_line(&mut first_line).ok()?;

    // Check if it's a shebang line
    if !first_line.starts_with("#!") {
        return None;
    }

    // Remove the #! prefix and trim whitespace
    let shebang = first_line[2..].trim();

    // Handle different shebang formats:
    // 1. #!/usr/bin/python3 -> /usr/bin/python3
    // 2. #!/usr/bin/env python3 -> resolve python3 from PATH
    // 3. #!/usr/bin/env python -> resolve python from PATH

    if shebang.starts_with("/usr/bin/env ") || shebang.starts_with("/bin/env ") {
        // Extract the command after 'env'
        let parts: Vec<&str> = shebang.split_whitespace().collect();
        if parts.len() >= 2 {
            let python_cmd = parts[1];
            // Try to find it in PATH using 'which'
            if let Ok(output) = std::process::Command::new("which")
                .arg(python_cmd)
                .output()
            {
                if output.status.success() {
                    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !path.is_empty() {
                        return Some(path);
                    }
                }
            }
        }
        None
    } else if shebang.starts_with("/") {
        // Direct path to interpreter
        Some(shebang.to_string())
    } else {
        None
    }
}

/// Find the default Python interpreter on the system
fn find_default_python() -> io::Result<String> {
    let kernels = kernel::discover_kernels();
    if kernels.is_empty() {
        eprintln!("Error: No Python interpreter found on the system");
        eprintln!("Install Python or specify a Python interpreter with --python");
        return Err(io::Error::new(io::ErrorKind::NotFound, "No Python interpreter found"));
    }
    Ok(kernels[0].python_path.clone())
}

/// Execute a Python file non-interactively
fn execute_file(file_path: Option<String>, python_path: Option<String>) -> io::Result<()> {
    // Check if file path was provided
    let file_path = match file_path {
        Some(path) => path,
        None => {
            eprintln!("Error: No file specified for execution");
            eprintln!("Usage: sage --execute <file.py> [--python <python_path>]");
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "No file specified"));
        }
    };

    // Check if file exists
    if !std::path::Path::new(&file_path).exists() {
        eprintln!("Error: File '{}' not found", file_path);
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("File '{}' not found", file_path)));
    }

    // Determine Python interpreter to use
    let python_executable = match python_path {
        Some(path) => path,
        None => {
            // First, try to parse shebang from the file
            match parse_shebang(&file_path) {
                Some(shebang_python) => {
                    // Verify the shebang interpreter exists
                    if std::path::Path::new(&shebang_python).exists() {
                        shebang_python
                    } else {
                        eprintln!("Warning: Shebang interpreter '{}' not found, using system default", shebang_python);
                        find_default_python()?
                    }
                }
                None => {
                    // No shebang found, discover Python interpreters on the system
                    find_default_python()?
                }
            }
        }
    };

    // Read file content
    let file_content = std::fs::read_to_string(&file_path)
        .map_err(|e| {
            eprintln!("Error reading file '{}': {}", file_path, e);
            e
        })?;

    // Parse file into cells
    let rope = ropey::Rope::from_str(&file_content);
    let cells = cell::parse_cells(&rope);

    // If no cells with delimiters, just run the whole file with Python directly
    if cells.len() == 1 && cells[0].start == 0 && cells[0].end == rope.len_bytes() {
        // No cell delimiters found - execute as a regular Python script
        let status = std::process::Command::new(&python_executable)
            .arg(&file_path)
            .status()
            .map_err(|e| {
                eprintln!("Error executing Python: {}", e);
                e
            })?;

        // Exit with the same status code as the Python process
        if !status.success() {
            std::process::exit(status.code().unwrap_or(1));
        }
        return Ok(());
    }

    // File has cells - execute them one by one with kernel
    let kernel_name = format!("Python ({})", python_executable);
    let mut kernel = direct_kernel::DirectKernel::new(
        python_executable.clone(),
        kernel_name.clone(),
        kernel_name,
    );

    // Connect to kernel
    if let Err(e) = kernel.connect() {
        eprintln!("Error connecting to Python kernel: {}", e);
        return Err(io::Error::new(io::ErrorKind::Other, format!("Failed to connect to kernel: {}", e)));
    }

    // Execute each cell in order
    for (cell_idx, cell) in cells.iter().enumerate() {
        let cell_number = cell_idx + 1;
        let code = cell::get_cell_content(&rope, cell);

        // Skip empty cells
        if code.trim().is_empty() {
            continue;
        }

        // Execute cell
        match kernel.execute(&code) {
            Ok(result) => {
                // Print outputs
                for output in &result.outputs {
                    match output {
                        kernel::ExecutionOutput::Stdout(text) => {
                            print!("{}", text);
                        }
                        kernel::ExecutionOutput::Stderr(text) => {
                            eprint!("{}", text);
                        }
                        kernel::ExecutionOutput::Result(text) => {
                            println!("{}", text);
                        }
                        kernel::ExecutionOutput::Error { ename, evalue, traceback } => {
                            eprintln!("Cell {} error: {}: {}", cell_number, ename, evalue);
                            for line in traceback {
                                if !line.trim().is_empty() {
                                    eprintln!("{}", line);
                                }
                            }
                        }
                        kernel::ExecutionOutput::Display { data, .. } => {
                            println!("{}", data);
                        }
                    }
                }

                // Stop on error
                if !result.success {
                    eprintln!("\nExecution stopped at cell {} due to error", cell_number);
                    let _ = kernel.disconnect();
                    std::process::exit(1);
                }
            }
            Err(e) => {
                eprintln!("Cell {} kernel error: {}", cell_number, e);
                let _ = kernel.disconnect();
                std::process::exit(1);
            }
        }
    }

    // Disconnect kernel
    let _ = kernel.disconnect();

    Ok(())
}

fn main() -> io::Result<()> {
    debug_log("=== SAGE DEBUG LOG ===");

    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();

    // Check for --execute or --run flag
    let mut execute_mode = false;
    let mut python_path: Option<String> = None;
    let mut file_to_execute: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--execute" | "--run" | "-e" => {
                execute_mode = true;
                // Next argument should be the file
                if i + 1 < args.len() {
                    file_to_execute = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            "--python" => {
                // Next argument should be the Python path
                if i + 1 < args.len() {
                    python_path = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            _ => {
                // If not in execute mode and no flags, this is the file to open
                if !execute_mode && file_to_execute.is_none() {
                    file_to_execute = Some(args[i].clone());
                }
            }
        }
        i += 1;
    }

    // Handle execute mode
    if execute_mode {
        return execute_file(file_to_execute, python_path);
    }

    // Check if we're running in a terminal
    if let Err(_) = enable_raw_mode() {
        // No terminal available - relaunch in a terminal emulator
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
    if let Some(path) = file_to_execute {
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

