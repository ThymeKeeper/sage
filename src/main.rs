mod buffer;
mod editor;
mod renderer;
mod commands;
mod prompt;
mod find_replace;
mod exit_prompt;

use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers, MouseEventKind, MouseButton},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};
use std::io;

fn main() -> io::Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    
    // Enable mouse support
    execute!(io::stdout(), crossterm::event::EnableMouseCapture)?;
    
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
    execute!(io::stdout(), crossterm::event::DisableMouseCapture)?;
    disable_raw_mode()?;
    
    if let Err(e) = result {
        eprintln!("Error: {}", e);
    }
    
    Ok(())
}

fn run(editor: &mut editor::Editor, renderer: &mut renderer::Renderer) -> io::Result<()> {
    let mut find_replace: Option<find_replace::FindReplace> = None;
    
    loop {
        // Draw the editor with find/replace window if active
        if find_replace.is_some() {
            renderer.draw_with_bottom_window(editor, 3)?;  // Changed from 5 to 3
        } else {
            renderer.draw(editor)?;
        }
        
        if let Some(ref fr) = find_replace {
            fr.draw(&mut io::stdout())?;
        }
        
        // Handle input
        match event::read()? {
            Event::Mouse(mouse_event) => {
                // Handle mouse events for text selection
                match mouse_event.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        // Start selection
                        if let Some(position) = editor.screen_to_buffer_position(
                            mouse_event.column as usize,
                            mouse_event.row as usize,
                        ) {
                            editor.start_mouse_selection(position);
                            renderer.force_redraw();
                        }
                    }
                    MouseEventKind::Drag(MouseButton::Left) => {
                        // Update selection
                        if let Some(position) = editor.screen_to_buffer_position(
                            mouse_event.column as usize,
                            mouse_event.row as usize,
                        ) {
                            editor.update_mouse_selection(position);
                        }
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        // Finish selection
                        editor.finish_mouse_selection();
                    }
                    _ => {}
                }
            }
            Event::Key(key) => {
                // Windows: ignore key release events
                #[cfg(target_os = "windows")]
                if key.kind == event::KeyEventKind::Release {
                    continue;
                }
                
                // If find/replace window is active, handle its input first
                if let Some(ref mut fr) = find_replace {
                    // Special handling for find/replace shortcuts
                    let fr_cmd = match key.code {
                        // Ctrl+F while find is open = find next
                        KeyCode::Char('f') | KeyCode::Char('F') if key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::SHIFT) => {
                            Some(commands::Command::FindNext)
                        }
                        // Ctrl+Shift+F = find previous
                        KeyCode::Char('f') | KeyCode::Char('F') if key.modifiers.contains(KeyModifiers::CONTROL) && key.modifiers.contains(KeyModifiers::SHIFT) => {
                            Some(commands::Command::FindPrev)
                        }
                        // Ctrl+H = replace current and find next
                        KeyCode::Char('h') | KeyCode::Char('H') if key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::SHIFT) => {
                            Some(commands::Command::Replace)
                        }
                        // Ctrl+Shift+H = replace all
                        KeyCode::Char('h') | KeyCode::Char('H') if key.modifiers.contains(KeyModifiers::CONTROL) && key.modifiers.contains(KeyModifiers::SHIFT) => {
                            Some(commands::Command::ReplaceAll)
                        }
                        // F3 = find next
                        KeyCode::F(3) if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                            Some(commands::Command::FindNext)
                        }
                        // Shift+F3 = find previous
                        KeyCode::F(3) if key.modifiers.contains(KeyModifiers::SHIFT) => {
                            Some(commands::Command::FindPrev)
                        }
                        _ => None
                    };
                    
                    // If we have a find/replace command, execute it
                    if let Some(cmd) = fr_cmd {
                        match cmd {
                            commands::Command::FindNext => {
                                if !fr.is_empty() {
                                    if let Some((start, end)) = fr.next_match() {
                                        editor.select_range(start, end);
                                    }
                                }
                            }
                            commands::Command::FindPrev => {
                                if !fr.is_empty() {
                                    if let Some((start, end)) = fr.prev_match() {
                                        editor.select_range(start, end);
                                    }
                                }
                            }
                            commands::Command::Replace => {
                                if !fr.is_empty() {
                                    // Replace current selection
                                    if editor.replace_selection(fr.replace_text()) {
                                        // Re-search after replacement
                                        let matches = editor.find_all(fr.find_text());
                                        fr.update_matches(matches);
                                        // Move to next match
                                        if let Some((start, end)) = fr.current_match_position() {
                                            editor.select_range(start, end);
                                        }
                                    }
                                }
                            }
                            commands::Command::ReplaceAll => {
                                if !fr.is_empty() {
                                    let find_text = fr.find_text().to_string();
                                    let replace_text = fr.replace_text().to_string();
                                    let matches = editor.find_all(&find_text);
                                    
                                    // Replace all from last to first to maintain positions
                                    for &(start, end) in matches.iter().rev() {
                                        editor.replace_at(start, end, &replace_text);
                                    }
                                    
                                    // Clear matches and update
                                    fr.update_matches(Vec::new());
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }
                    
                    // Handle regular input for find/replace window
                    let result = fr.handle_input(key.code, key.modifiers);
                    match result {
                        find_replace::InputResult::Close => {
                            find_replace = None;
                            // Clear selection when closing find
                            editor.selection_start = None;
                            // Force redraw
                            execute!(io::stdout(), 
                                crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
                                crossterm::cursor::Hide
                            )?;
                            renderer.force_redraw();
                        }
                        find_replace::InputResult::FindTextChanged => {
                            // Update search results
                            let matches = editor.find_all(fr.find_text());
                            fr.update_matches(matches.clone());
                            // Select first match if any
                            if let Some((start, end)) = fr.current_match_position() {
                                editor.select_range(start, end);
                            } else {
                                editor.selection_start = None;
                            }
                        }
                        find_replace::InputResult::FindNext => {
                            if !fr.is_empty() {
                                if let Some((start, end)) = fr.next_match() {
                                    editor.select_range(start, end);
                                }
                            }
                        }
                        find_replace::InputResult::Continue => {}
                    }
                    continue; // Skip normal command processing
                }
                
                let cmd = match key.code {
                    // Quit
                    KeyCode::Char('q') | KeyCode::Char('Q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if editor.is_modified() {
                            // Show exit prompt for unsaved changes
                            let mut exit_prompt = exit_prompt::ExitPrompt::new();
                            
                            // Hide cursor before showing prompt
                            execute!(io::stdout(), crossterm::cursor::Hide)?;
                            
                            let filename = editor.file_name();
                            
                            // Run the prompt and get result
                            let result = exit_prompt.run(&mut io::stdout(), filename)?;
                            
                            // Clear the screen and force complete redraw
                            execute!(io::stdout(), 
                                crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
                                crossterm::cursor::Hide
                            )?;
                            renderer.force_redraw();
                            
                            match result {
                                exit_prompt::ExitOption::Save => {
                                    // Try to save
                                    if editor.file_path().is_none() {
                                        // Need Save As
                                        let initial_path = editor.get_save_as_initial_path();
                                        let mut prompt = prompt::Prompt::new("Save As", &initial_path);
                                        
                                        if let Some(path) = prompt.run(&mut io::stdout())? {
                                            if let Err(e) = editor.save_as(path) {
                                                eprintln!("Failed to save file: {}", e);
                                                // Clear and redraw
                                                execute!(io::stdout(), 
                                                    crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
                                                    crossterm::cursor::Hide
                                                )?;
                                                renderer.force_redraw();
                                                renderer.draw(editor)?;
                                                continue; // Don't exit if save failed
                                            } else {
                                                return Ok(()); // Successfully saved, exit
                                            }
                                        } else {
                                            // User cancelled Save As, don't exit
                                            execute!(io::stdout(), 
                                                crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
                                                crossterm::cursor::Hide
                                            )?;
                                            renderer.force_redraw();
                                            renderer.draw(editor)?;
                                            continue;
                                        }
                                    } else {
                                        // Normal save
                                        if let Err(e) = editor.save() {
                                            eprintln!("Failed to save file: {}", e);
                                            renderer.draw(editor)?;
                                            continue; // Don't exit if save failed
                                        } else {
                                            return Ok(()); // Successfully saved, exit
                                        }
                                    }
                                }
                                exit_prompt::ExitOption::ExitWithoutSaving => {
                                    return Ok(()); // Exit without saving
                                }
                                exit_prompt::ExitOption::Cancel => {
                                    // Cancel exit, redraw and continue
                                    renderer.draw(editor)?;
                                    continue;
                                }
                            }
                        } else {
                            // No unsaved changes, exit immediately
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
                    
                    // Find/Replace
                    KeyCode::Char('f') | KeyCode::Char('F') if key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::SHIFT) => {
                        commands::Command::FindReplace
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
                    commands::Command::FindReplace => {
                        // Open find/replace window
                        find_replace = Some(find_replace::FindReplace::new());
                    }
                    _ => {
                        // All other commands are handled normally
                        editor.execute(cmd)?;
                    }
                }
            }
            _ => {}
        }
    }
}
