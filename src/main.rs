mod buffer;
mod editor;
mod renderer;
mod commands;
mod prompt;

use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};
use std::io;

fn main() -> io::Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    
    let mut editor = editor::Editor::new();
    let mut renderer = renderer::Renderer::new()?;
    
    // Load file if provided
    if let Some(path) = std::env::args().nth(1) {
        if let Err(e) = editor.load_file(&path) {
            eprintln!("Failed to load file: {}", e);
        }
    }
    
    // Main loop
    let result = run(&mut editor, &mut renderer);
    
    // Cleanup
    renderer.cleanup()?;
    disable_raw_mode()?;
    
    if let Err(e) = result {
        eprintln!("Error: {}", e);
    }
    
    Ok(())
}

fn run(editor: &mut editor::Editor, renderer: &mut renderer::Renderer) -> io::Result<()> {
    loop {
        // Draw the editor
        renderer.draw(editor)?;
        
        // Handle input
        if let Event::Key(key) = event::read()? {
            // Windows: ignore key release events
            #[cfg(target_os = "windows")]
            if key.kind == event::KeyEventKind::Release {
                continue;
            }
            
            let cmd = match key.code {
                // Quit
                KeyCode::Char('q') | KeyCode::Char('Q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if editor.is_modified() {
                        // TODO: Add save prompt
                        return Ok(());
                    } else {
                        return Ok(());
                    }
                }
                
                // Save / Save As
                KeyCode::Char('s') | KeyCode::Char('S') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        commands::Command::SaveAs
                    } else {
                        commands::Command::Save
                    }
                }
                
                // Undo/Redo
                KeyCode::Char('z') | KeyCode::Char('Z') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        commands::Command::Redo
                    } else {
                        commands::Command::Undo
                    }
                }
                
                // Clipboard operations
                KeyCode::Char('c') | KeyCode::Char('C') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    commands::Command::Copy
                }
                
                KeyCode::Char('x') | KeyCode::Char('X') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    commands::Command::Cut
                }
                
                KeyCode::Char('v') | KeyCode::Char('V') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    commands::Command::Paste
                }
                
                // Select All
                KeyCode::Char('a') | KeyCode::Char('A') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    commands::Command::SelectAll
                }
                
                // Movement (with selection support)
                KeyCode::Up => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        commands::Command::SelectUp
                    } else {
                        commands::Command::MoveUp
                    }
                }
                KeyCode::Down => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        commands::Command::SelectDown
                    } else {
                        commands::Command::MoveDown
                    }
                }
                KeyCode::Left => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        commands::Command::SelectLeft
                    } else {
                        commands::Command::MoveLeft
                    }
                }
                KeyCode::Right => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        commands::Command::SelectRight
                    } else {
                        commands::Command::MoveRight
                    }
                }
                KeyCode::Home => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        commands::Command::SelectHome
                    } else {
                        commands::Command::MoveHome
                    }
                }
                KeyCode::End => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        commands::Command::SelectEnd
                    } else {
                        commands::Command::MoveEnd
                    }
                }
                KeyCode::PageUp => commands::Command::PageUp,
                KeyCode::PageDown => commands::Command::PageDown,
                
                // Editing
                KeyCode::Char(c) => commands::Command::InsertChar(c),
                KeyCode::Enter => commands::Command::InsertNewline,
                KeyCode::Tab => commands::Command::InsertTab,
                KeyCode::Backspace => commands::Command::Backspace,
                KeyCode::Delete => commands::Command::Delete,
                
                _ => commands::Command::None,
            };
            
            // Handle commands that need special UI interaction
            match cmd {
                commands::Command::Save => {
                    // Check if we have a file path
                    if editor.file_path().is_none() {
                        // No file path, trigger Save As
                        let initial_path = editor.get_save_as_initial_path();
                        let mut prompt = prompt::Prompt::new("Save As", &initial_path);
                        
                        // Hide cursor before showing prompt
                        execute!(io::stdout(), crossterm::cursor::Hide)?;
                        
                        // Run the prompt and get result
                        let result = prompt.run(&mut io::stdout())?;
                        
                        // Clear the entire screen and force complete redraw
                        execute!(io::stdout(), 
                            crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
                            crossterm::cursor::Hide
                        )?;
                        renderer.force_redraw();
                        
                        // Process the result
                        if let Some(path) = result {
                            if let Err(e) = editor.save_as(path) {
                                eprintln!("Failed to save file: {}", e);
                            }
                        }
                        
                        // Redraw the editor
                        renderer.draw(editor)?;
                    } else {
                        // Normal save
                        editor.execute(cmd)?;
                    }
                }
                commands::Command::SaveAs => {
                    let initial_path = editor.get_save_as_initial_path();
                    let mut prompt = prompt::Prompt::new("Save As", &initial_path);
                    
                    // Hide cursor before showing prompt
                    execute!(io::stdout(), crossterm::cursor::Hide)?;
                    
                    // Run the prompt and get result
                    let result = prompt.run(&mut io::stdout())?;
                    
                    // Clear the entire screen and force complete redraw
                    execute!(io::stdout(), 
                        crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
                        crossterm::cursor::Hide
                    )?;
                    renderer.force_redraw();
                    
                    // Process the result
                    if let Some(path) = result {
                        if let Err(e) = editor.save_as(path) {
                            eprintln!("Failed to save file: {}", e);
                        }
                    }
                    
                    // Redraw the editor
                    renderer.draw(editor)?;
                }
                _ => {
                    // All other commands are handled normally
                    editor.execute(cmd)?;
                }
            }
        }
    }
}