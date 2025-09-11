mod buffer;
mod editor;
mod renderer;
mod commands;

use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
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
                KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if editor.is_modified() {
                        // TODO: Add save prompt
                        return Ok(());
                    } else {
                        return Ok(());
                    }
                }
                
                // Save
                KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    commands::Command::Save
                }
                
                // Undo/Redo
                KeyCode::Char('z') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        commands::Command::Redo
                    } else {
                        commands::Command::Undo
                    }
                }
                
                // Movement
                KeyCode::Up => commands::Command::MoveUp,
                KeyCode::Down => commands::Command::MoveDown,
                KeyCode::Left => commands::Command::MoveLeft,
                KeyCode::Right => commands::Command::MoveRight,
                KeyCode::Home => commands::Command::MoveHome,
                KeyCode::End => commands::Command::MoveEnd,
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
            
            editor.execute(cmd)?;
        }
    }
}