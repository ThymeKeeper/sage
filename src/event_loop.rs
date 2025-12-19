use crate::{editor, renderer, find_replace, output_pane, kernel, autocomplete, prompt, exit_prompt, kernel_selector, commands, direct_kernel};
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers, MouseEventKind, MouseButton},
    execute,
};
use std::io::{self, Write};
use std::time::Duration;

fn debug_log(msg: &str) {
    use std::fs::OpenOptions;
    if let Ok(mut log) = OpenOptions::new().create(true).append(true).open("/tmp/sage_debug.log") {
        let _ = writeln!(log, "{}", msg);
        let _ = log.flush();
    }
}

pub fn run(editor: &mut editor::Editor, renderer: &mut renderer::Renderer) -> io::Result<()> {
    let mut find_replace: Option<find_replace::FindReplace> = None;
    let mut output_pane = output_pane::OutputPane::new();
    let mut output_pane_visible = true; // Visible by default
    let mut output_pane_height = 8; // Default height in lines
    let mut needs_redraw = true; // Track if we need to redraw
    let mut skip_event_read = false; // Skip event read to force immediate redraw

    // State for background execution with live timer
    let mut execution_rx: Option<std::sync::mpsc::Receiver<(Box<dyn kernel::Kernel>, Vec<(usize, usize, String, bool, f64)>, Vec<kernel::CompletionItem>, kernel::TypeRelationships, kernel::SqlMetadata)>> = None;
    let mut execution_start_time: Option<std::time::Instant> = None;
    let mut executing_kernel_info: Option<kernel::KernelInfo> = None;

    // Autocomplete
    let mut autocomplete = autocomplete::Autocomplete::new();
    let mut suppress_autocomplete_once = false; // Suppress after Tab completion

    loop {
        debug_log(&format!("Loop iteration start"));

        // Check if background execution is complete
        if let Some(ref rx) = execution_rx {
            match rx.try_recv() {
                Ok((kernel, results, completions, type_relationships, sql_metadata)) => {
                    // Execution complete! Put kernel back and process results
                    editor.set_kernel(kernel);
                    execution_rx = None;
                    executing_kernel_info = None;
                    let elapsed = execution_start_time.take().map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);

                    // Add outputs to pane
                    for (count, line, output, is_error, cell_elapsed) in results {
                        output_pane.add_output(output_pane::OutputEntry {
                            execution_count: count,
                            cell_line: line,
                            output,
                            is_error,
                            elapsed_secs: cell_elapsed,
                        });
                    }

                    // Update autocomplete with dynamic completions
                    if !completions.is_empty() {
                        let completion_names: Vec<String> = completions.iter()
                            .map(|c| c.name.clone())
                            .collect();
                        autocomplete.add_dynamic_completions(completion_names);
                    }

                    // Update autocomplete with type relationships and SQL metadata
                    autocomplete.set_type_relationships(type_relationships);
                    autocomplete.set_sql_metadata(sql_metadata);

                    // Update status message with final time
                    editor.status_message = Some((format!("Executed ({:.3}s)", elapsed), false));

                    // Show output pane if needed
                    output_pane.set_focused(false);
                    if !output_pane_visible {
                        output_pane_visible = true;
                        editor.update_viewport_for_cursor_with_bottom(output_pane_height);
                    }

                    renderer.force_redraw();
                    needs_redraw = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    // Still executing - update status bar with elapsed time
                    if let Some(start_time) = execution_start_time {
                        let elapsed = start_time.elapsed().as_secs_f64();
                        editor.status_message = Some((format!("Executing... {:.1}s", elapsed), false));
                        needs_redraw = true;
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // Thread panicked or channel closed unexpectedly
                    editor.status_message = Some(("Execution failed".to_string(), true));
                    execution_rx = None;
                    execution_start_time = None;
                    executing_kernel_info = None;
                    needs_redraw = true;
                }
            }
        }

        // Only draw if needed
        if needs_redraw {
            debug_log(&format!("needs_redraw is true, starting draw"));
            // Calculate bottom window height
            let bottom_window_height = if find_replace.is_some() {
                3 // Find/replace pane
            } else if output_pane_visible {
                output_pane_height
            } else {
                0
            };

            debug_log(&format!("About to call draw_with_bottom_window"));
            // Draw the editor with bottom window if needed
            renderer.draw_with_bottom_window(editor, bottom_window_height)?;
            debug_log(&format!("draw_with_bottom_window completed"));

            // Draw the appropriate pane
            if let Some(ref fr) = find_replace {
                debug_log(&format!("Drawing find_replace"));
                fr.draw(&mut io::stdout())?;
            } else if output_pane_visible {
                debug_log(&format!("Drawing output_pane"));
                let (width, height) = crossterm::terminal::size()?;
                // Output pane starts after the status bar
                let output_start_row = height.saturating_sub(output_pane_height as u16);
                output_pane.draw(&mut io::stdout(), output_start_row, output_pane_height, width)?;
                // Only reposition cursor to editor if output pane doesn't have focus
                if !output_pane.is_focused() {
                    renderer.reposition_cursor(editor)?;
                }
                debug_log(&format!("output_pane draw completed"));
            }

            // Draw autocomplete dropdown if visible
            if autocomplete.is_visible() {
                let (screen_col, screen_row) = editor.cursor_screen_position();
                let (width, height) = crossterm::terminal::size()?;
                autocomplete.draw(&mut io::stdout(), screen_row as u16, screen_col as u16, height, width)?;
                // Reposition cursor after drawing autocomplete
                renderer.reposition_cursor(editor)?;
            }

            needs_redraw = false; // Reset flag after drawing
            debug_log(&format!("Draw complete, needs_redraw set to false"));
        }

        // Skip event read if we need immediate redraw (after cell execution)
        if skip_event_read {
            debug_log(&format!("Skipping event read, continuing loop"));
            skip_event_read = false;
            needs_redraw = true;
            continue;
        }

        debug_log(&format!("About to read event"));
        // Handle input - use polling with timeout when execution is running
        let event_available = if execution_rx.is_some() {
            // Poll with 100ms timeout to update timer frequently
            event::poll(std::time::Duration::from_millis(100))?
        } else {
            // Block waiting for event when not executing
            event::poll(std::time::Duration::from_secs(3600))? // 1 hour timeout (effectively blocking)
        };

        if !event_available {
            // No event, continue loop to update timer
            continue;
        }

        let event = event::read()?;
        debug_log(&format!("Event read completed: {:?}", match &event {
            Event::Key(k) => format!("Key({:?})", k.code),
            Event::Mouse(_) => "Mouse".to_string(),
            Event::Resize(w, h) => format!("Resize({}, {})", w, h),
            _ => "Other".to_string(),
        }));
        match event {
            Event::Mouse(mouse_event) => {
                // Check if shift is held for horizontal scrolling
                let shift_held = mouse_event.modifiers.contains(crossterm::event::KeyModifiers::SHIFT);
                
                // Only handle mouse events if find/replace is NOT open
                if find_replace.is_none() {
                    // Handle mouse events for text selection
                    match mouse_event.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            // Hide autocomplete on any mouse click
                            if autocomplete.is_visible() {
                                autocomplete.hide();
                                needs_redraw = true;
                            }

                            // Check if click is in output pane area
                            let (_, height) = crossterm::terminal::size()?;
                            let output_start_row = height.saturating_sub(output_pane_height as u16 + 1);

                            if output_pane_visible && mouse_event.row >= output_start_row {
                                // Click is in output pane - focus it and start mouse selection
                                output_pane.set_focused(true);
                                output_pane.start_mouse_selection(
                                    mouse_event.column as usize,
                                    mouse_event.row as usize,
                                );
                                needs_redraw = true;
                            } else {
                                // Click is in editor - unfocus output pane and start selection
                                output_pane.set_focused(false);
                                if let Some(position) = editor.screen_to_buffer_position(
                                    mouse_event.column as usize,
                                    mouse_event.row as usize,
                                ) {
                                    editor.start_mouse_selection(position);
                                    // Update viewport with correct bottom window height
                                    let bottom_height = if output_pane_visible { output_pane_height } else { 0 };
                                    editor.update_viewport_for_cursor_with_bottom(bottom_height);
                                    renderer.force_redraw();
                                    needs_redraw = true; // Need to redraw for selection
                                }
                            }
                        }
                        MouseEventKind::Drag(MouseButton::Left) => {
                            // Check if we're dragging in the output pane
                            let (_, height) = crossterm::terminal::size()?;
                            let output_start_row = height.saturating_sub(output_pane_height as u16 + 1);

                            if output_pane.is_focused() && output_pane_visible && mouse_event.row >= output_start_row {
                                // Update selection in output pane
                                output_pane.update_mouse_selection(
                                    mouse_event.column as usize,
                                    mouse_event.row as usize,
                                );
                                needs_redraw = true;
                            } else {
                                // Update selection in editor
                                if let Some(position) = editor.screen_to_buffer_position(
                                    mouse_event.column as usize,
                                    mouse_event.row as usize,
                                ) {
                                    editor.update_mouse_selection(position);
                                    // Update viewport with correct bottom window height
                                    let bottom_height = if output_pane_visible { output_pane_height } else { 0 };
                                    editor.update_viewport_for_cursor_with_bottom(bottom_height);
                                    needs_redraw = true; // Need to redraw for selection update
                                }
                            }
                        }
                        MouseEventKind::Up(MouseButton::Left) => {
                            // Finish selection in both editor and output pane
                            editor.finish_mouse_selection();
                            output_pane.finish_mouse_selection();
                            // Update viewport with correct bottom window height
                            let bottom_height = if output_pane_visible { output_pane_height } else { 0 };
                            editor.update_viewport_for_cursor_with_bottom(bottom_height);
                            needs_redraw = true; // Need to redraw to finalize selection
                        }
                        MouseEventKind::ScrollDown => {
                            // Check if mouse is over output pane
                            let (_, height) = crossterm::terminal::size()?;
                            let output_start_row = height.saturating_sub(output_pane_height as u16 + 1);

                            if output_pane_visible && mouse_event.row >= output_start_row {
                                // Scroll output pane
                                output_pane.scroll_down();
                            } else if shift_held {
                                // Shift+scroll = horizontal scroll right
                                editor.scroll_viewport_horizontal(5);
                            } else {
                                // Normal scroll = vertical scroll down
                                editor.scroll_viewport_vertical(3);
                            }
                            needs_redraw = true; // Need to redraw for scroll
                        }
                        MouseEventKind::ScrollUp => {
                            // Check if mouse is over output pane
                            let (_, height) = crossterm::terminal::size()?;
                            let output_start_row = height.saturating_sub(output_pane_height as u16 + 1);

                            if output_pane_visible && mouse_event.row >= output_start_row {
                                // Scroll output pane
                                output_pane.scroll_up();
                            } else if shift_held {
                                // Shift+scroll = horizontal scroll left
                                editor.scroll_viewport_horizontal(-5);
                            } else {
                                // Normal scroll = vertical scroll up
                                editor.scroll_viewport_vertical(-3);
                            }
                            needs_redraw = true; // Need to redraw for scroll
                        }
                        MouseEventKind::ScrollLeft => {
                            // Scroll viewport left without moving cursor
                            editor.scroll_viewport_horizontal(-5);
                            needs_redraw = true; // Need to redraw for scroll
                        }
                        MouseEventKind::ScrollRight => {
                            // Scroll viewport right without moving cursor
                            editor.scroll_viewport_horizontal(5);
                            needs_redraw = true; // Need to redraw for scroll
                        }
                        MouseEventKind::Moved => {
                            // Mouse just moved, no interaction - DO NOT REDRAW
                            // This prevents flickering when mouse moves
                        }
                        _ => {
                            // Other mouse events we don't handle - DO NOT REDRAW
                        }
                    }
                } else {
                    // Find/replace is open, only handle scroll events
                    match mouse_event.kind {
                        MouseEventKind::ScrollDown => {
                            if shift_held {
                                editor.scroll_viewport_horizontal(5);
                            } else {
                                editor.scroll_viewport_vertical(3);
                            }
                            needs_redraw = true;
                        }
                        MouseEventKind::ScrollUp => {
                            if shift_held {
                                editor.scroll_viewport_horizontal(-5);
                            } else {
                                editor.scroll_viewport_vertical(-3);
                            }
                            needs_redraw = true;
                        }
                        MouseEventKind::ScrollLeft => {
                            editor.scroll_viewport_horizontal(-5);
                            needs_redraw = true;
                        }
                        MouseEventKind::ScrollRight => {
                            editor.scroll_viewport_horizontal(5);
                            needs_redraw = true;
                        }
                        _ => {
                            // Ignore all other mouse events when find/replace is open
                            // This includes mouse movement, clicks, and drags
                        }
                    }
                }
            }
            Event::Paste(text) => {
                // Handle bracketed paste - insert the entire text at once without triggering auto-indent
                editor.paste_text(text);
                needs_redraw = true;
            }
            Event::Key(key) => {
                // Ignore key release events (both Windows and other platforms)
                if key.kind == event::KeyEventKind::Release {
                    continue;
                }

                needs_redraw = true; // Key events usually need redraw
                
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

                        _ => None
                    };
                    
                    // If we have a find/replace command, execute it
                    if let Some(cmd) = fr_cmd {
                        match cmd {
                            commands::Command::FindNext => {
                                if !fr.is_empty() {
                                    if let Some((start, end)) = fr.next_match() {
                                        editor.select_range(start, end);
                                        // Update current match index for highlighting
                                        editor.set_find_matches(fr.get_all_matches().to_vec(), fr.get_current_match_index());
                                    }
                                }
                            }
                            commands::Command::FindPrev => {
                                if !fr.is_empty() {
                                    if let Some((start, end)) = fr.prev_match() {
                                        editor.select_range(start, end);
                                        // Update current match index for highlighting
                                        editor.set_find_matches(fr.get_all_matches().to_vec(), fr.get_current_match_index());
                                    }
                                }
                            }
                            commands::Command::Replace => {
                                if !fr.is_empty() {
                                    // Replace current selection
                                    if editor.replace_selection(fr.replace_text()) {
                                        // Re-search after replacement
                                        let matches = editor.find_all(fr.find_text());
                                        fr.update_matches(matches.clone());
                                        // Update editor's find matches for highlighting
                                        editor.set_find_matches(matches, fr.get_current_match_index());
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
                                    editor.clear_find_matches();
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }
                    
                    // Check if this is an undo/redo command first
                    let is_undo_redo = match (key.code, key.modifiers.contains(KeyModifiers::CONTROL)) {
                        (KeyCode::Char('z') | KeyCode::Char('Z'), true) => true,
                        _ => false,
                    };
                    
                    // If it's not undo/redo, handle it as find/replace input
                    if !is_undo_redo {
                        // Handle regular input for find/replace window
                        let result = fr.handle_input(key.code, key.modifiers);
                        match result {
                            find_replace::InputResult::Close => {
                                find_replace = None;
                                // Clear selection and find matches when closing find
                                editor.selection_start = None;
                                editor.clear_find_matches();
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
                                // Update editor's find matches for highlighting
                                // Only set current match to 0 if there are actually matches
                                let current_match = if matches.is_empty() { None } else { Some(0) };
                                editor.set_find_matches(matches, current_match);
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
                                        // Update current match index for highlighting
                                        editor.set_find_matches(fr.get_all_matches().to_vec(), fr.get_current_match_index());
                                    }
                                }
                            }
                            find_replace::InputResult::Continue => {}
                        }
                        continue; // Skip normal command processing
                    }
                    // If it's undo/redo, fall through to normal command processing
                }

                // Note: suppress_autocomplete_once flag (if set by Tab completion) will be
                // checked and cleared in the autocomplete update logic below

                let cmd = match key.code {
                    // Esc - Hide autocomplete, or toggle output pane focus
                    KeyCode::Esc => {
                        if autocomplete.is_visible() {
                            autocomplete.hide();
                            needs_redraw = true;
                        } else if output_pane_visible {
                            output_pane.toggle_focus();
                            needs_redraw = true;
                        }
                        commands::Command::None
                    }

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
                                            if editor.save_as(path).is_err() {
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
                                        if editor.save().is_err() {
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
                    
                    // Aggressive Cancellation (Ctrl+Backspace)
                    // TODO: Implement graceful interruption (SIGINT) to preserve kernel state
                    // Currently this does a hard reset which loses all Python variables/state
                    KeyCode::Backspace if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        // Ctrl+Backspace = AGGRESSIVE CANCEL
                        if execution_rx.is_some() {
                            // Drop the channel - abandons the background thread
                            execution_rx = None;
                            execution_start_time = None;

                            // Recreate a fresh kernel using stored info
                            if let Some(kernel_info) = executing_kernel_info.take() {
                                match kernel_info.kernel_type {
                                    kernel::KernelType::Direct => {
                                        let mut new_kernel: Box<dyn kernel::Kernel> = Box::new(direct_kernel::DirectKernel::new(
                                            kernel_info.python_path.clone(),
                                            kernel_info.name.clone(),
                                            kernel_info.display_name.clone(),
                                        ));
                                        // Connect the new kernel
                                        if new_kernel.connect().is_ok() {
                                            editor.set_kernel(new_kernel);
                                            editor.status_message = Some(("CANCELLED - Kernel reset (all variables lost)".to_string(), true));
                                        } else {
                                            editor.status_message = Some(("CANCELLED - Kernel reconnection failed".to_string(), true));
                                        }
                                    }
                                    _ => {
                                        // For Jupyter or other kernel types, just report cancellation
                                        editor.status_message = Some(("Execution cancelled - please reconnect kernel".to_string(), true));
                                    }
                                }
                            } else {
                                editor.status_message = Some(("Execution cancelled".to_string(), true));
                            }

                            renderer.force_redraw();
                            needs_redraw = true;
                        } else {
                            // Not executing - just show message to confirm Ctrl+Backspace was detected
                            editor.status_message = Some(("No execution to cancel".to_string(), false));
                        }
                        commands::Command::None
                    }

                    KeyCode::Char('c') | KeyCode::Char('C') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        // Check if output pane has focus and has selected text
                        if output_pane_visible && output_pane.is_focused() {
                            if let Some(selected_text) = output_pane.get_selected_text() {
                                // Copy to system clipboard
                                use arboard::Clipboard;
                                if let Ok(mut clipboard) = Clipboard::new() {
                                    let _ = clipboard.set_text(selected_text);
                                }
                            }
                            commands::Command::None
                        } else {
                            commands::Command::Copy
                        }
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

                    // Execute Cell (Ctrl+E as alternative)
                    KeyCode::Char('e') | KeyCode::Char('E') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        // Check if already executing
                        if execution_rx.is_some() {
                            editor.status_message = Some(("Already executing (Ctrl+Backspace to cancel - WARNING: resets kernel)".to_string(), true));
                            needs_redraw = true;
                        } else {
                            // Start background execution
                            if let Some((rx, kernel_info)) = spawn_background_execution(editor) {
                                execution_rx = Some(rx);
                                execution_start_time = Some(std::time::Instant::now());
                                executing_kernel_info = Some(kernel_info);
                                editor.status_message = Some(("Executing...".to_string(), false));
                                needs_redraw = true;
                            } else {
                                // No kernel connected
                                editor.status_message = Some(("No kernel connected. Press Ctrl+K to select a kernel.".to_string(), true));
                                needs_redraw = true;
                            }
                        }
                        commands::Command::None
                    }

                    // Clear Output Pane (Ctrl+L)
                    KeyCode::Char('l') | KeyCode::Char('L') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        output_pane.clear();
                        editor.status_message = Some(("Output cleared".to_string(), false));
                        needs_redraw = true;
                        commands::Command::None
                    }

                    // Toggle Output Pane (Ctrl+O)
                    KeyCode::Char('o') | KeyCode::Char('O') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        output_pane_visible = !output_pane_visible;
                        // Update viewport to account for new bottom window height
                        let bottom_height = if output_pane_visible { output_pane_height } else { 0 };
                        editor.update_viewport_for_cursor_with_bottom(bottom_height);
                        renderer.force_redraw();
                        commands::Command::None
                    }

                    // Kernel Selection (Ctrl+K)
                    KeyCode::Char('k') | KeyCode::Char('K') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        // Show loading message
                        editor.status_message = Some(("Discovering Python kernels...".to_string(), false));
                        renderer.draw(editor)?;
                        use std::io::Write;
                        let mut stdout = io::stdout();
                        stdout.flush()?;

                        // Create selector (this does the discovery)
                        let mut selector = kernel_selector::KernelSelector::new();

                        execute!(io::stdout(), crossterm::cursor::Hide)?;

                        debug_log(&format!("About to run kernel selector"));
                        let result = match selector.run(&mut io::stdout()) {
                            Ok(r) => {
                                debug_log(&format!("Kernel selector returned: {:?}", r.is_some()));
                                r
                            }
                            Err(e) => {
                                debug_log(&format!("Kernel selector error: {}", e));
                                editor.status_message = Some((format!("Selector error: {}", e), true));
                                None
                            }
                        };

                        debug_log(&format!("Clearing screen"));
                        // Clear and redraw - important to clear the entire screen
                        execute!(io::stdout(),
                            crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
                        )?;
                        debug_log(&format!("Screen cleared"));

                        // Reset terminal state completely
                        execute!(io::stdout(), crossterm::cursor::Hide)?;

                        renderer.force_redraw();
                        needs_redraw = true;

                        if let Some(kernel_info) = result {
                            use crate::direct_kernel::DirectKernel;

                            // Create kernel based on type
                            let mut kernel: Box<dyn kernel::Kernel> = match kernel_info.kernel_type {
                                kernel::KernelType::Direct => {
                                    Box::new(DirectKernel::new(
                                        kernel_info.python_path.clone(),
                                        kernel_info.name.clone(),
                                        kernel_info.display_name.clone()
                                    ))
                                }
                                kernel::KernelType::Jupyter => {
                                    // For now, fall back to direct kernel
                                    // TODO: Implement Jupyter kernel
                                    editor.status_message = Some(("Jupyter kernels not yet supported, using direct kernel".to_string(), false));
                                    Box::new(DirectKernel::new(
                                        kernel_info.python_path.clone(),
                                        kernel_info.name.clone(),
                                        kernel_info.display_name.clone()
                                    ))
                                }
                            };

                            // Disconnect old kernel first if exists
                            if editor.is_kernel_connected() {
                                debug_log(&format!("Disconnecting old kernel"));
                                let _ = editor.disconnect_kernel();
                                debug_log(&format!("Old kernel disconnected"));
                            }

                            // Connect to kernel
                            debug_log(&format!("Connecting to new kernel: {}", kernel_info.display_name));
                            match kernel.connect() {
                                Ok(_) => {
                                    debug_log(&format!("Connected successfully"));
                                    editor.set_kernel(kernel);
                                    editor.enable_repl_mode();
                                    editor.status_message = Some(("Connected to kernel".to_string(), false));
                                    debug_log(&format!("Kernel set"));
                                }
                                Err(e) => {
                                    debug_log(&format!("Connection failed: {}", e));
                                    editor.status_message = Some((format!("Failed to connect: {}", e), true));
                                    // Don't set the kernel if connection failed
                                }
                            }
                        } else {
                            // User cancelled - clear any status message
                            editor.status_message = None;
                        }

                        debug_log(&format!("About to force full redraw"));
                        // Force full redraw after kernel selector
                        execute!(io::stdout(),
                            crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
                        )?;
                        debug_log(&format!("Screen cleared 2"));
                        renderer.force_redraw();
                        debug_log(&format!("Force redraw done, setting needs_redraw"));
                        needs_redraw = true;

                        commands::Command::None
                    }

                    // Movement (with selection support)
                    KeyCode::Up => {
                        if autocomplete.is_visible() && !key.modifiers.contains(KeyModifiers::ALT) {
                            // Navigate autocomplete dropdown
                            autocomplete.select_previous();
                            needs_redraw = true;
                            commands::Command::None
                        } else if output_pane_visible && output_pane.is_focused() && !key.modifiers.contains(KeyModifiers::ALT) {
                            // When output pane is focused, Up moves cursor
                            if key.modifiers.contains(KeyModifiers::CONTROL) {
                                // Ctrl+Up: move to previous paragraph
                                let with_selection = key.modifiers.contains(KeyModifiers::SHIFT);
                                output_pane.move_cursor_paragraph_up(with_selection);
                            } else {
                                let with_selection = key.modifiers.contains(KeyModifiers::SHIFT);
                                output_pane.move_cursor_up(with_selection);
                            }
                            needs_redraw = true;
                            commands::Command::None
                        } else if key.modifiers.contains(KeyModifiers::ALT) {
                            // Alt+Up = Increase output pane height
                            let (_, term_height) = crossterm::terminal::size()?;
                            let max_height = (term_height as usize).saturating_sub(3); // Leave 3 lines for editor
                            if output_pane_visible && output_pane_height < max_height {
                                output_pane_height += 1;
                                // Update viewport to account for new bottom window height
                                editor.update_viewport_for_cursor_with_bottom(output_pane_height);
                                renderer.force_redraw();
                                needs_redraw = true;
                            }
                            commands::Command::None
                        } else if key.modifiers.contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT) {
                            commands::Command::SelectParagraphUp
                        } else if key.modifiers.contains(KeyModifiers::CONTROL) {
                            commands::Command::MoveParagraphUp
                        } else if key.modifiers.contains(KeyModifiers::SHIFT) {
                            commands::Command::SelectUp
                        } else {
                            commands::Command::MoveUp
                        }
                    }
                    KeyCode::Down => {
                        if autocomplete.is_visible() && !key.modifiers.contains(KeyModifiers::ALT) {
                            // Navigate autocomplete dropdown
                            autocomplete.select_next();
                            needs_redraw = true;
                            commands::Command::None
                        } else if output_pane_visible && output_pane.is_focused() && !key.modifiers.contains(KeyModifiers::ALT) {
                            // When output pane is focused, Down moves cursor
                            if key.modifiers.contains(KeyModifiers::CONTROL) {
                                // Ctrl+Down: move to next paragraph
                                let with_selection = key.modifiers.contains(KeyModifiers::SHIFT);
                                output_pane.move_cursor_paragraph_down(with_selection);
                            } else {
                                let with_selection = key.modifiers.contains(KeyModifiers::SHIFT);
                                output_pane.move_cursor_down(with_selection);
                            }
                            needs_redraw = true;
                            commands::Command::None
                        } else if key.modifiers.contains(KeyModifiers::ALT) {
                            // Alt+Down = Decrease output pane height
                            if output_pane_visible && output_pane_height > 3 {
                                output_pane_height -= 1;
                                // Update viewport to account for new bottom window height
                                editor.update_viewport_for_cursor_with_bottom(output_pane_height);
                                renderer.force_redraw();
                                needs_redraw = true;
                            }
                            commands::Command::None
                        } else if key.modifiers.contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT) {
                            commands::Command::SelectParagraphDown
                        } else if key.modifiers.contains(KeyModifiers::CONTROL) {
                            commands::Command::MoveParagraphDown
                        } else if key.modifiers.contains(KeyModifiers::SHIFT) {
                            commands::Command::SelectDown
                        } else {
                            commands::Command::MoveDown
                        }
                    }
                    KeyCode::Left => {
                        if output_pane_visible && output_pane.is_focused() {
                            if key.modifiers.contains(KeyModifiers::CONTROL) {
                                // Ctrl+Left: move to previous word
                                let with_selection = key.modifiers.contains(KeyModifiers::SHIFT);
                                output_pane.move_cursor_word_left(with_selection);
                            } else {
                                let with_selection = key.modifiers.contains(KeyModifiers::SHIFT);
                                output_pane.move_cursor_left(with_selection);
                            }
                            needs_redraw = true;
                            commands::Command::None
                        } else if key.modifiers.contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT) {
                            commands::Command::SelectWordLeft
                        } else if key.modifiers.contains(KeyModifiers::CONTROL) {
                            commands::Command::MoveWordLeft
                        } else if key.modifiers.contains(KeyModifiers::SHIFT) {
                            commands::Command::SelectLeft
                        } else {
                            commands::Command::MoveLeft
                        }
                    }
                    KeyCode::Right => {
                        if output_pane_visible && output_pane.is_focused() {
                            if key.modifiers.contains(KeyModifiers::CONTROL) {
                                // Ctrl+Right: move to next word
                                let with_selection = key.modifiers.contains(KeyModifiers::SHIFT);
                                output_pane.move_cursor_word_right(with_selection);
                            } else {
                                let with_selection = key.modifiers.contains(KeyModifiers::SHIFT);
                                output_pane.move_cursor_right(with_selection);
                            }
                            needs_redraw = true;
                            commands::Command::None
                        } else if key.modifiers.contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT) {
                            commands::Command::SelectWordRight
                        } else if key.modifiers.contains(KeyModifiers::CONTROL) {
                            commands::Command::MoveWordRight
                        } else if key.modifiers.contains(KeyModifiers::SHIFT) {
                            commands::Command::SelectRight
                        } else {
                            commands::Command::MoveRight
                        }
                    }
                    KeyCode::Home => {
                        if output_pane_visible && output_pane.is_focused() {
                            let with_selection = key.modifiers.contains(KeyModifiers::SHIFT);
                            output_pane.move_cursor_home(with_selection);
                            needs_redraw = true;
                            commands::Command::None
                        } else if key.modifiers.contains(KeyModifiers::SHIFT) {
                            commands::Command::SelectHome
                        } else {
                            commands::Command::MoveHome
                        }
                    }
                    KeyCode::End => {
                        if output_pane_visible && output_pane.is_focused() {
                            let with_selection = key.modifiers.contains(KeyModifiers::SHIFT);
                            output_pane.move_cursor_end(with_selection);
                            needs_redraw = true;
                            commands::Command::None
                        } else if key.modifiers.contains(KeyModifiers::SHIFT) {
                            commands::Command::SelectEnd
                        } else {
                            commands::Command::MoveEnd
                        }
                    }
                    KeyCode::PageUp => {
                        if key.modifiers.contains(KeyModifiers::SHIFT) && output_pane_visible {
                            // Shift+PageUp = Scroll output pane up
                            output_pane.scroll_up();
                            needs_redraw = true;
                            commands::Command::None
                        } else {
                            commands::Command::PageUp
                        }
                    }
                    KeyCode::PageDown => {
                        if key.modifiers.contains(KeyModifiers::SHIFT) && output_pane_visible {
                            // Shift+PageDown = Scroll output pane down
                            output_pane.scroll_down();
                            needs_redraw = true;
                            commands::Command::None
                        } else {
                            commands::Command::PageDown
                        }
                    }

                    // Editing
                    // Ctrl+H is often sent by terminals for Ctrl+Backspace - handle cancellation
                    KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) && find_replace.is_none() => {
                        // Same cancellation logic as Ctrl+Backspace
                        if execution_rx.is_some() {
                            execution_rx = None;
                            execution_start_time = None;

                            if let Some(kernel_info) = executing_kernel_info.take() {
                                match kernel_info.kernel_type {
                                    kernel::KernelType::Direct => {
                                        let mut new_kernel: Box<dyn kernel::Kernel> = Box::new(direct_kernel::DirectKernel::new(
                                            kernel_info.python_path.clone(),
                                            kernel_info.name.clone(),
                                            kernel_info.display_name.clone(),
                                        ));
                                        if new_kernel.connect().is_ok() {
                                            editor.set_kernel(new_kernel);
                                            editor.status_message = Some(("CANCELLED - Kernel reset (all variables lost)".to_string(), true));
                                        } else {
                                            editor.status_message = Some(("CANCELLED - Kernel reconnection failed".to_string(), true));
                                        }
                                    }
                                    _ => {
                                        editor.status_message = Some(("Execution cancelled - please reconnect kernel".to_string(), true));
                                    }
                                }
                            } else {
                                editor.status_message = Some(("Execution cancelled".to_string(), true));
                            }

                            renderer.force_redraw();
                            needs_redraw = true;
                        } else {
                            editor.status_message = Some(("No execution to cancel".to_string(), false));
                        }
                        commands::Command::None
                    }
                    KeyCode::Char(c) => commands::Command::InsertChar(c),
                    KeyCode::Enter => {
                        // Ctrl+Enter = Execute cell (primary binding)
                        if key.modifiers.contains(KeyModifiers::CONTROL) {
                            // Check if already executing
                            if execution_rx.is_some() {
                                editor.status_message = Some(("Already executing (Ctrl+Backspace to cancel - WARNING: resets kernel)".to_string(), true));
                                needs_redraw = true;
                            } else {
                                // Start background execution
                                if let Some((rx, kernel_info)) = spawn_background_execution(editor) {
                                    execution_rx = Some(rx);
                                    execution_start_time = Some(std::time::Instant::now());
                                    executing_kernel_info = Some(kernel_info);
                                    editor.status_message = Some(("Executing...".to_string(), false));
                                    needs_redraw = true;
                                }
                            }
                            commands::Command::None
                        } else {
                            commands::Command::InsertNewline
                        }
                    }
                    KeyCode::Tab => {
                        if autocomplete.is_visible() && !key.modifiers.contains(KeyModifiers::SHIFT) {
                            // Accept autocomplete suggestion
                            if let Some(suggestion) = autocomplete.get_selected() {
                                let prefix = editor.get_word_at_cursor();
                                // Delete the prefix and insert the full suggestion
                                for _ in 0..prefix.len() {
                                    editor.execute(commands::Command::Backspace)?;
                                }
                                for ch in suggestion.chars() {
                                    editor.execute(commands::Command::InsertChar(ch))?;
                                }
                                autocomplete.hide();
                                renderer.force_redraw(); // Force full redraw to clear autocomplete artifacts
                                suppress_autocomplete_once = true; // Don't show autocomplete on next key
                                needs_redraw = true;
                            }
                            commands::Command::None
                        } else if key.modifiers.contains(KeyModifiers::SHIFT) {
                            // Shift+Tab = dedent
                            commands::Command::Dedent
                        } else if editor.selection().is_some() {
                            // Tab with selection = indent all selected lines
                            commands::Command::Indent
                        } else {
                            // Tab without selection = insert 4 spaces
                            commands::Command::InsertTab
                        }
                    }
                    KeyCode::BackTab => {
                        // Some terminals send BackTab for Shift+Tab
                        commands::Command::Dedent
                    }
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
                                let _ = editor.save_as(path);
                            }
                            
                            // Redraw the editor
                            renderer.draw(editor)?;
                        } else {
                            // Normal save
                            let _ = editor.execute(cmd);
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
                            let _ = editor.save_as(path);
                        }
                        
                        // Redraw the editor
                        renderer.draw(editor)?;
                    }
                    commands::Command::FindReplace => {
                        // Open find/replace window
                        find_replace = Some(find_replace::FindReplace::new());
                    }
                    commands::Command::None => {
                        // No command - don't override needs_redraw flag
                        // (it may have been explicitly set to true by event handlers)
                    }
                    _ => {
                        // Update autocomplete after text-modifying commands (before executing to avoid move)
                        let should_update_autocomplete = matches!(cmd, commands::Command::InsertChar(_));
                        let should_check_backspace_delete = matches!(cmd, commands::Command::Backspace | commands::Command::Delete);
                        let should_hide_autocomplete = !matches!(cmd, commands::Command::None) && !should_update_autocomplete && !should_check_backspace_delete;

                        // All other commands are handled normally
                        editor.execute(cmd)?;
                        // Update viewport with correct bottom window height after movement commands
                        let bottom_height = if find_replace.is_some() {
                            3
                        } else if output_pane_visible {
                            output_pane_height
                        } else {
                            0
                        };
                        editor.update_viewport_for_cursor_with_bottom(bottom_height);

                        // Apply autocomplete updates based on command type
                        if suppress_autocomplete_once {
                            // Skip autocomplete update this cycle (after Tab completion)
                            suppress_autocomplete_once = false;
                        } else if should_update_autocomplete {
                            let (base_callable, prefix, is_sql_context) = editor.get_completion_context();
                            // Debug logging
                            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/sage_debug.log") {
                                use std::io::Write;
                                let _ = writeln!(f, "DEBUG event_loop: should_update_autocomplete, base_callable={:?}, prefix='{}', is_sql={}", base_callable, prefix, is_sql_context);
                            }
                            autocomplete.update_with_context(base_callable, &prefix, is_sql_context);
                            renderer.force_redraw(); // Clear artifacts when menu changes
                        } else if should_check_backspace_delete {
                            let (base_callable, prefix, is_sql_context) = editor.get_completion_context();
                            if prefix.is_empty() && base_callable.is_none() && !is_sql_context {
                                autocomplete.hide();
                                renderer.force_redraw();
                            } else {
                                autocomplete.update_with_context(base_callable, &prefix, is_sql_context);
                                renderer.force_redraw(); // Clear artifacts when menu changes
                            }
                        } else if should_hide_autocomplete {
                            autocomplete.hide();
                            renderer.force_redraw();
                        }
                    }
                }
            }
            Event::Resize(_, _) => {
                // Terminal was resized, force redraw
                renderer.force_redraw();
                needs_redraw = true;
            }
            _ => {
                // Other events don't need redraw
            }
        }
    }
}

fn spawn_background_execution(
    editor: &mut editor::Editor,
) -> Option<(std::sync::mpsc::Receiver<(Box<dyn kernel::Kernel>, Vec<(usize, usize, String, bool, f64)>, Vec<kernel::CompletionItem>, kernel::TypeRelationships, kernel::SqlMetadata)>, kernel::KernelInfo)> {
    // Extract kernel from editor (temporarily)
    let mut kernel = editor.take_kernel()?;

    // Store kernel info for potential recreation
    let kernel_info = kernel.info().clone();

    // Get selection or current cell position
    let selection = editor.selection();
    let cursor_offset = editor.cursor();  // Get byte offset, not line/col

    // Clone cell data we need
    editor.update_cells();
    let cells: Vec<(usize, usize, String)> = {
        use crate::cell::{get_cell_at_position, get_cell_content};

        // Find cells to execute (same logic as execute_selected_cells_with_output)
        let cells_to_execute: Vec<usize> = if let Some((sel_start, sel_end)) = selection {
            editor.get_cells_ref().iter().enumerate()
                .filter(|(_, cell)| cell.start < sel_end && cell.end > sel_start)
                .map(|(idx, _)| idx)
                .collect()
        } else {
            if let Some(cell_idx) = get_cell_at_position(editor.get_cells_ref(), cursor_offset) {
                vec![cell_idx]
            } else {
                vec![]
            }
        };

        // Extract cell contents
        cells_to_execute.iter().map(|&idx| {
            let cell = &editor.get_cells_ref()[idx];
            let code = get_cell_content(editor.buffer_rope(), cell);
            let cell_number = idx + 1;
            (idx, cell_number, code)
        }).collect()
    };

    if cells.is_empty() {
        editor.set_kernel(kernel);
        return None;
    }

    // Spawn background thread
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut results = Vec::new();
        let mut all_completions = Vec::new();
        let mut type_relationships = crate::kernel::TypeRelationships::default();
        let mut sql_metadata = crate::kernel::SqlMetadata::default();

        for (_cell_idx, cell_number, code) in cells {
            let start_time = std::time::Instant::now();

            match kernel.execute(&code) {
                Ok(result) => {
                    let elapsed = start_time.elapsed().as_secs_f64();
                    let execution_count = result.execution_count.unwrap_or(0);
                    let output_text = crate::cell::format_output(&result);
                    let is_error = !result.success;

                    // Collect completions from this execution
                    all_completions.extend(result.completions);

                    // Update type relationships and SQL metadata (keep the latest)
                    type_relationships = result.type_relationships;
                    sql_metadata = result.sql_metadata;

                    results.push((execution_count, cell_number, output_text, is_error, elapsed));

                    // Stop execution if this cell had an error
                    if is_error {
                        break;
                    }
                }
                Err(e) => {
                    let elapsed = start_time.elapsed().as_secs_f64();
                    results.push((0, cell_number, format!("Error: {}", e), true, elapsed));
                    // Stop execution on kernel error
                    break;
                }
            }
        }

        // Send back kernel, results, completions, type relationships, and SQL metadata
        let _ = tx.send((kernel, results, all_completions, type_relationships, sql_metadata));
    });

    Some((rx, kernel_info))
}
