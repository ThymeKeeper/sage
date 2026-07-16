use crate::{editor, renderer, find_replace, output_pane, kernel, autocomplete, prompt, exit_prompt, kernel_selector, language_selector, commands, direct_kernel, syntax, help_screen, config, snippet_picker};
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers, MouseEventKind, MouseButton},
    execute,
};
use std::io;

/// Messages flowing from the background execution thread to the host.
/// One `Cell` per executed statement (sent as soon as it finishes), then
/// exactly one `Done` carrying the kernel back and the final autocomplete
/// metadata snapshot.
enum ExecMsg {
    Cell {
        label: String,
        output: String,
        is_error: bool,
        elapsed: f64,
    },
    Done {
        kernel: Box<dyn kernel::Kernel>,
        completions: Vec<kernel::CompletionItem>,
        type_relationships: kernel::TypeRelationships,
        sql_metadata: kernel::SqlMetadata,
    },
}


pub fn run(editor: &mut editor::Editor, renderer: &mut renderer::Renderer) -> io::Result<()> {
    let mut find_replace: Option<find_replace::FindReplace> = None;
    let mut output_pane = output_pane::OutputPane::new();
    // Output pane should only be visible for Python/REPL mode
    let mut output_pane_visible = editor.is_repl_mode();
    let mut output_pane_height = 8; // Default height in lines
    let mut needs_redraw = true; // Track if we need to redraw
    let mut skip_event_read = false; // Skip event read to force immediate redraw
    let mut help_screen: Option<help_screen::HelpScreen> = None; // Help screen state

    // State for background execution with live timer
    let mut execution_rx: Option<std::sync::mpsc::Receiver<ExecMsg>> = None;
    let mut execution_start_time: Option<std::time::Instant> = None;
    let mut executing_kernel_info: Option<kernel::KernelInfo> = None;
    // Cancel handle for the kernel currently inside the background execution
    // thread. Captured before the kernel is moved, so the host can still reach
    // it during a long-running execute(). Polymorphic — DirectKernel returns
    // one that kills its OS process, SnowflakeKernel returns one that POSTs
    // an abort to the SQL API.
    let mut executing_cancel: Option<std::sync::Arc<dyn kernel::CancelHandle>> = None;
    // True when the executing kernel can survive cancellation (Snowflake).
    // False when cancel destroys the kernel (DirectKernel) and we have to
    // rebuild it. Captured at spawn time alongside the cancel handle.
    let mut executing_preserves_session: bool = false;

    // Autocomplete
    let mut autocomplete = autocomplete::Autocomplete::new();
    let mut suppress_autocomplete_once = false; // Suppress after Tab completion

    // Spreadsheet: track recent click for double-click detection
    let mut ss_last_click: Option<(std::time::Instant, crate::spreadsheet::GridHit)> = None;

    loop {
        // Drain any pending execution messages from the background thread.
        // We loop until the channel is empty so that several cells finishing
        // in quick succession all render in the same tick instead of one
        // per main-loop iteration.
        if execution_rx.is_some() {
            let mut done = false;
            let mut failed = false;
            let mut any_cell_drawn = false;
            loop {
                let msg = match execution_rx.as_ref().unwrap().try_recv() {
                    Ok(m) => m,
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        failed = true;
                        break;
                    }
                };
                match msg {
                    ExecMsg::Cell {
                        label,
                        output,
                        is_error,
                        elapsed,
                    } => {
                        output_pane.add_output(output_pane::OutputEntry {
                            label,
                            output,
                            is_error,
                            elapsed_secs: elapsed,
                        });
                        any_cell_drawn = true;
                    }
                    ExecMsg::Done {
                        kernel,
                        completions,
                        type_relationships,
                        sql_metadata,
                    } => {
                        editor.set_kernel(kernel);
                        if !completions.is_empty() {
                            let completion_names: Vec<String> =
                                completions.iter().map(|c| c.name.clone()).collect();
                            autocomplete.add_dynamic_completions(completion_names);
                        }
                        autocomplete.set_type_relationships(type_relationships);
                        autocomplete.set_sql_metadata(sql_metadata);
                        done = true;
                    }
                }
            }

            if any_cell_drawn {
                // First cell of the batch — make sure the output pane is up
                // so the user can see it. Subsequent ticks won't re-open it
                // because output_pane_visible is already true.
                if !output_pane_visible {
                    output_pane_visible = true;
                    editor.update_viewport_for_cursor_with_bottom(output_pane_height);
                }
                output_pane.set_focused(false);
                renderer.force_redraw();
                needs_redraw = true;
            }

            if done {
                let elapsed = execution_start_time
                    .take()
                    .map(|t| t.elapsed().as_secs_f64())
                    .unwrap_or(0.0);
                editor.status_message = Some((format!("Executed ({:.3}s)", elapsed), false));
                execution_rx = None;
                executing_kernel_info = None;
                executing_cancel = None;
                executing_preserves_session = false;
                needs_redraw = true;
            } else if failed {
                editor.status_message = Some(("Execution failed".to_string(), true));
                execution_rx = None;
                execution_start_time = None;
                executing_kernel_info = None;
                executing_cancel = None;
                executing_preserves_session = false;
                needs_redraw = true;
            } else if !any_cell_drawn {
                // Channel still open, nothing new — update the running-timer
                // status bar without a full redraw to avoid pane flicker.
                if let Some(start_time) = execution_start_time {
                    let elapsed = start_time.elapsed().as_secs_f64();
                    editor.status_message =
                        Some((format!("Executing... {:.1}s", elapsed), false));
                    let bottom_window_height = if output_pane_visible {
                        output_pane_height
                    } else {
                        0
                    };
                    renderer.update_status_bar_only(editor, bottom_window_height)?;
                }
            }
        }

        // Only draw if needed
        if needs_redraw {
            // If help screen is visible, draw it and skip everything else
            if let Some(ref help) = help_screen {
                help.draw(&mut io::stdout())?;
                needs_redraw = false;
            } else {
                // Calculate bottom window height
                let bottom_window_height = if find_replace.is_some() {
                    3 // Find/replace pane
                } else if output_pane_visible {
                    output_pane_height
                } else {
                    0
                };

                // Draw the editor with bottom window if needed
                let bottom_focused = output_pane_visible && output_pane.is_focused();
                renderer.draw_with_bottom_window(editor, bottom_window_height, bottom_focused)?;

                // Draw the appropriate pane
                if let Some(ref fr) = find_replace {
                    fr.draw(&mut io::stdout())?;
                } else if output_pane_visible {
                    let (width, height) = crossterm::terminal::size()?;
                    // Output pane starts after the status bar
                    let output_start_row = height.saturating_sub(output_pane_height as u16);
                    output_pane.draw(&mut io::stdout(), output_start_row, output_pane_height, width)?;
                    // Only reposition cursor to editor if output pane doesn't have focus
                    if !output_pane.is_focused() {
                        renderer.reposition_cursor(editor, bottom_window_height)?;
                    }
                }

                // Draw autocomplete dropdown if visible
                if autocomplete.is_visible() {
                    let (screen_col, screen_row) = editor.cursor_screen_position();
                    let (width, height) = crossterm::terminal::size()?;
                    autocomplete.draw(&mut io::stdout(), screen_row as u16, screen_col as u16, height, width)?;
                    // Reposition cursor after drawing autocomplete (but not if output pane is focused)
                    if !output_pane.is_focused() {
                        renderer.reposition_cursor(editor, bottom_window_height)?;
                    }
                }

                needs_redraw = false;
            }
        }

        // Skip event read if we need immediate redraw (after cell execution)
        if skip_event_read {
            skip_event_read = false;
            needs_redraw = true;
            continue;
        }
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
        match event {
            Event::Mouse(mouse_event) => {
                // Check if shift is held for horizontal scrolling
                let shift_held = mouse_event.modifiers.contains(crossterm::event::KeyModifiers::SHIFT);

                // Spreadsheet mode handles mouse itself
                if editor.is_spreadsheet_mode() {
                    // Double-click detection for auto-sizing columns
                    if mouse_event.kind == MouseEventKind::Down(MouseButton::Left) {
                        let (term_w, term_h) = crossterm::terminal::size()?;
                        let hit_now = editor
                            .spreadsheet()
                            .map(|ss| ss.hit_test(mouse_event.column, mouse_event.row, term_w, term_h));
                        if let Some(hit) = hit_now {
                            let now = std::time::Instant::now();
                            let is_double = ss_last_click
                                .as_ref()
                                .map(|(t, prev_hit)| {
                                    now.duration_since(*t) < std::time::Duration::from_millis(500)
                                        && *prev_hit == hit
                                })
                                .unwrap_or(false);
                            ss_last_click = Some((now, hit));
                            if is_double {
                                if let crate::spreadsheet::GridHit::ColumnSeparator { col } = hit {
                                    if let Some(ss) = editor.spreadsheet_mut() {
                                        ss.end_mouse(); // Cancel any pending resize drag
                                        ss.auto_size_column(col);
                                    }
                                    needs_redraw = true;
                                    continue;
                                }
                            }
                        }
                    }

                    let cursor_before = editor.spreadsheet().map(|ss| ss.cursor);
                    handle_spreadsheet_mouse(editor, &mouse_event, &mut needs_redraw)?;
                    let cursor_after = editor.spreadsheet().map(|ss| ss.cursor);
                    if cursor_before != cursor_after {
                        ensure_ss_cursor_visible(editor)?;
                    }
                    continue;
                }

                // Handle mouse scrolling for help screen
                if let Some(ref mut help) = help_screen {
                    match mouse_event.kind {
                        MouseEventKind::ScrollDown => {
                            help.scroll_down();
                            needs_redraw = true;
                        }
                        MouseEventKind::ScrollUp => {
                            help.scroll_up();
                            needs_redraw = true;
                        }
                        _ => {}
                    }
                    continue; // Skip other mouse event handling when help screen is open
                }

                // Only handle mouse events if find/replace is NOT open
                if find_replace.is_none() {
                    // Handle mouse events for text selection
                    match mouse_event.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            // Hide autocomplete on any mouse click
                            if autocomplete.is_visible() {
                                autocomplete.hide();
                                renderer.force_redraw(); // Force full redraw to clear autocomplete popup
                                if output_pane_visible {
                                    output_pane.invalidate_cache();
                                }
                                needs_redraw = true;
                            }

                            // Check if click is in output pane area
                            let (_, height) = crossterm::terminal::size()?;
                            // The actual output pane start row (matching draw calculation)
                            let pane_start_row = height.saturating_sub(output_pane_height as u16);
                            // Include status line row in click area
                            let click_area_start = height.saturating_sub(output_pane_height as u16 + 1);

                            if output_pane_visible && mouse_event.row >= click_area_start {
                                // Click is in output pane - start mouse selection (which handles focus)
                                output_pane.start_mouse_selection(
                                    mouse_event.column as usize,
                                    mouse_event.row as usize,
                                    pane_start_row,
                                    output_pane_height,
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
                                    // Don't force full redraw - differential rendering handles cursor movement
                                    needs_redraw = true;
                                }
                            }
                        }
                        MouseEventKind::Drag(MouseButton::Left) => {
                            // Check if we're dragging in the output pane
                            let (_, height) = crossterm::terminal::size()?;
                            let pane_start_row = height.saturating_sub(output_pane_height as u16);
                            let click_area_start = height.saturating_sub(output_pane_height as u16 + 1);

                            if output_pane.is_focused() && output_pane_visible && mouse_event.row >= click_area_start {
                                // Update selection in output pane
                                output_pane.update_mouse_selection(
                                    mouse_event.column as usize,
                                    mouse_event.row as usize,
                                    pane_start_row,
                                    output_pane_height,
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
                                // Over output pane
                                if shift_held {
                                    // Shift+scroll = horizontal scroll right
                                    output_pane.scroll_horizontal(5);
                                } else {
                                    // Normal scroll = vertical scroll down
                                    output_pane.scroll_down();
                                }
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
                                // Over output pane
                                if shift_held {
                                    // Shift+scroll = horizontal scroll left
                                    output_pane.scroll_horizontal(-5);
                                } else {
                                    // Normal scroll = vertical scroll up
                                    output_pane.scroll_up();
                                }
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
                            // Check if mouse is over output pane
                            let (_, height) = crossterm::terminal::size()?;
                            let output_start_row = height.saturating_sub(output_pane_height as u16 + 1);

                            if output_pane_visible && mouse_event.row >= output_start_row {
                                // Scroll output pane left
                                output_pane.scroll_horizontal(-5);
                            } else {
                                // Scroll editor viewport left
                                editor.scroll_viewport_horizontal(-5);
                            }
                            needs_redraw = true; // Need to redraw for scroll
                        }
                        MouseEventKind::ScrollRight => {
                            // Check if mouse is over output pane
                            let (_, height) = crossterm::terminal::size()?;
                            let output_start_row = height.saturating_sub(output_pane_height as u16 + 1);

                            if output_pane_visible && mouse_event.row >= output_start_row {
                                // Scroll output pane right
                                output_pane.scroll_horizontal(5);
                            } else {
                                // Scroll editor viewport right
                                editor.scroll_viewport_horizontal(5);
                            }
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

                // Modal "busy" state: while an execution is in flight — a cell,
                // or a standalone app whose window is still open — swallow every
                // keystroke except the cancel chord, so the buffer can't be
                // edited or navigated mid-run. The ticking "Executing..." status
                // signals the block. Cancel comes in as Ctrl+Backspace, or Ctrl+H
                // on terminals that map the two; let both reach their handlers.
                if execution_rx.is_some() {
                    let is_cancel = key.modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(
                            key.code,
                            KeyCode::Backspace | KeyCode::Char('h') | KeyCode::Char('H')
                        );
                    if !is_cancel {
                        continue;
                    }
                }

                needs_redraw = true; // Key events usually need redraw

                // Spreadsheet mode: intercept most keys before any normal handling,
                // except when the find pane is open — then input belongs to the find pane.
                if editor.is_spreadsheet_mode() && find_replace.is_none() {
                    let cursor_before = editor.spreadsheet().map(|ss| ss.cursor);
                    if handle_spreadsheet_key(editor, &key, &mut needs_redraw) {
                        let cursor_after = editor.spreadsheet().map(|ss| ss.cursor);
                        if cursor_before != cursor_after {
                            ensure_ss_cursor_visible(editor)?;
                        }
                        continue;
                    }
                    // fall-through for Ctrl+S, Ctrl+Shift+S, Ctrl+Q
                }

                // Spreadsheet-mode find: a separate, simpler input path. Matches are cells
                // (row, col) and navigating them moves the cell cursor, not text ranges.
                if editor.is_spreadsheet_mode() && find_replace.is_some() {
                    let action = {
                        let fr = find_replace.as_mut().unwrap();
                        handle_spreadsheet_find_key(editor, fr, &key)
                    };
                    match action {
                        SsFindAction::Close => {
                            find_replace = None;
                            execute!(io::stdout(),
                                crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
                                crossterm::cursor::Hide
                            )?;
                            renderer.force_redraw();
                        }
                        SsFindAction::Handled => {
                            ensure_ss_cursor_visible_with_bottom(editor, 3)?;
                        }
                    }
                    needs_redraw = true;
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
                                        // Update viewport to show the match (find/replace pane is 3 lines)
                                        editor.update_viewport_for_cursor_with_bottom(3);
                                        renderer.force_redraw();
                                    }
                                }
                            }
                            commands::Command::FindPrev => {
                                if !fr.is_empty() {
                                    if let Some((start, end)) = fr.prev_match() {
                                        editor.select_range(start, end);
                                        // Update current match index for highlighting
                                        editor.set_find_matches(fr.get_all_matches().to_vec(), fr.get_current_match_index());
                                        // Update viewport to show the match (find/replace pane is 3 lines)
                                        editor.update_viewport_for_cursor_with_bottom(3);
                                        renderer.force_redraw();
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
                                    // Update viewport to show the first match (find/replace pane is 3 lines)
                                    editor.update_viewport_for_cursor_with_bottom(3);
                                    renderer.force_redraw();
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
                                        // Update viewport to show the match (find/replace pane is 3 lines)
                                        editor.update_viewport_for_cursor_with_bottom(3);
                                        renderer.force_redraw();
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

                // F1 - Toggle help screen
                if key.code == KeyCode::F(1) {
                    if help_screen.is_some() {
                        // Hide help screen
                        help_screen = None;
                        execute!(io::stdout(),
                            crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
                            crossterm::cursor::Hide
                        )?;
                        renderer.force_redraw();
                    } else {
                        // Show help screen
                        help_screen = Some(help_screen::HelpScreen::new());
                        execute!(io::stdout(),
                            crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
                        )?;
                    }
                    needs_redraw = true;
                    continue; // Skip normal command processing
                }

                // If help screen is visible, handle its input
                if let Some(ref mut help) = help_screen {
                    match key.code {
                        KeyCode::Esc => {
                            // Close help screen
                            help_screen = None;
                            execute!(io::stdout(),
                                crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
                                crossterm::cursor::Hide
                            )?;
                            renderer.force_redraw();
                            needs_redraw = true;
                        }
                        KeyCode::Up => {
                            // Scroll up
                            help.scroll_up();
                            needs_redraw = true;
                        }
                        KeyCode::Down => {
                            // Scroll down
                            help.scroll_down();
                            needs_redraw = true;
                        }
                        _ => {
                            // Ignore all other keys
                        }
                    }
                    continue; // Skip normal command processing
                }

                let cmd = match key.code {
                    // Esc - Hide autocomplete or toggle output pane focus
                    KeyCode::Esc => {
                        if autocomplete.is_visible() {
                            autocomplete.hide();
                            if output_pane_visible {
                                output_pane.invalidate_cache();
                            }
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

                    // Export query results (F9) — copies the active kernel's
                    // result spool (e.g. SnowflakeKernel's CSV tempfile) to a
                    // user-chosen path, prepopulated with the OS Downloads
                    // folder and a timestamped filename. No-op for kernels
                    // that don't spool (e.g. DirectKernel).
                    KeyCode::F(9) => {
                        match editor.kernel_latest_result_file() {
                            Some(src) => {
                                let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
                                let filename = format!("export-{}.csv", stamp);
                                // dirs::download_dir() resolves to the OS
                                // Downloads folder (USERPROFILE\Downloads on
                                // Windows, ~/Downloads on Linux/macOS via
                                // XDG_DOWNLOAD_DIR with sane fallback). If
                                // neither downloads nor home is available we
                                // fall back to a bare filename (CWD-relative).
                                let initial = dirs::download_dir()
                                    .or_else(dirs::home_dir)
                                    .map(|d| d.join(&filename).to_string_lossy().into_owned())
                                    .unwrap_or(filename);
                                let mut p = prompt::Prompt::new("Export results to", &initial);
                                if let Some(dest) = p.run(&mut io::stdout())? {
                                    let dest_display = format!("{}", std::path::Path::new(&dest).display());
                                    match std::fs::copy(&src, &dest) {
                                        Ok(bytes) => {
                                            editor.status_message = Some((
                                                format!("Exported {} bytes to {}", bytes, dest_display),
                                                false,
                                            ));
                                        }
                                        Err(e) => {
                                            editor.status_message = Some((
                                                format!("Failed to export results: {}", e),
                                                true,
                                            ));
                                        }
                                    }
                                }
                                renderer.force_redraw();
                                needs_redraw = true;
                            }
                            None => {
                                editor.status_message = Some((
                                    "No query results to export.".to_string(),
                                    true,
                                ));
                                needs_redraw = true;
                            }
                        }
                        commands::Command::None
                    }

                    // Open query results in a new sage session (F8) — copies the
                    // first 10,000 rows of the active kernel's (already-CSV) result spool
                    // into a temp file and opens it in a fresh sage window in spreadsheet
                    // mode. SQL mode only; like F9 export, a no-op for kernels that don't
                    // spool results.
                    KeyCode::F(8) => {
                        if *editor.get_language() != syntax::Language::Sql {
                            editor.status_message = Some((
                                "Open results (F8) is only available in SQL mode.".to_string(),
                                true,
                            ));
                        } else {
                            match editor.kernel_latest_result_file() {
                                Some(src) => match results_head_tempfile(&src, 10_000) {
                                    Ok((out_path, rows)) => {
                                        let out_str = out_path.to_string_lossy().into_owned();
                                        if crate::launch_child_session(&out_str) {
                                            editor.status_message = Some((
                                                format!("Opened {} row(s) in a new sage session", rows),
                                                false,
                                            ));
                                        } else {
                                            editor.status_message = Some((
                                                format!(
                                                    "Saved results to {} but couldn't open a terminal window",
                                                    out_str
                                                ),
                                                true,
                                            ));
                                        }
                                    }
                                    Err(e) => {
                                        editor.status_message = Some((
                                            format!("Failed to prepare results: {}", e),
                                            true,
                                        ));
                                    }
                                },
                                None => {
                                    editor.status_message = Some((
                                        "No query results to open.".to_string(),
                                        true,
                                    ));
                                }
                            }
                        }
                        needs_redraw = true;
                        commands::Command::None
                    }
                    
                    // Undo/Redo
                    KeyCode::Char('z') | KeyCode::Char('Z') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if key.modifiers.contains(KeyModifiers::SHIFT) {
                            commands::Command::Redo
                        } else {
                            commands::Command::Undo
                        }
                    }
                    
                    // Cancellation (Ctrl+Backspace)
                    // Soft-cancel kernels (Snowflake) get a graceful abort and keep
                    // the session. Hard-cancel kernels (Python) get the process killed
                    // and rebuilt from scratch (variables lost).
                    KeyCode::Backspace if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if execution_rx.is_some() {
                            if executing_preserves_session {
                                // Soft cancel: signal the kernel and let the normal
                                // channel-recv flow finish (execute() returns Err,
                                // thread sends the kernel back, we reuse it).
                                if let Some(handle) = executing_cancel.take() {
                                    handle.cancel();
                                }
                                editor.status_message = Some(("Cancelling...".to_string(), false));
                                needs_redraw = true;
                            } else {
                                // Hard cancel: drop the channel (abandon thread),
                                // kill the process via the cancel handle, rebuild the
                                // kernel from scratch.
                                execution_rx = None;
                                execution_start_time = None;
                                executing_preserves_session = false;

                                if let Some(handle) = executing_cancel.take() {
                                    handle.cancel();
                                }

                                if let Some(kernel_info) = executing_kernel_info.take() {
                                    match kernel::build_from_info(&kernel_info) {
                                        Ok(mut new_kernel) => {
                                            if new_kernel.connect().is_ok() {
                                                editor.set_kernel(new_kernel);
                                                editor.status_message = Some(("CANCELLED - Kernel reset (all variables lost)".to_string(), true));
                                            } else {
                                                editor.status_message = Some(("CANCELLED - Kernel reconnection failed".to_string(), true));
                                            }
                                        }
                                        Err(e) => {
                                            editor.status_message = Some((format!("CANCELLED - Kernel rebuild failed: {}", e), true));
                                        }
                                    }
                                } else {
                                    editor.status_message = Some(("Execution cancelled".to_string(), true));
                                }

                                renderer.force_redraw();
                                needs_redraw = true;
                            }
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

                    // Toggle Case (Ctrl+U)
                    KeyCode::Char('u') | KeyCode::Char('U') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        commands::Command::ToggleCase
                    }

                    // Toggle Word Wrap (Ctrl+W) — takes effect in plain text & Markdown
                    KeyCode::Char('w') | KeyCode::Char('W') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        let now_on = !editor.word_wrap_enabled();
                        editor.set_word_wrap(now_on);
                        // Drop any horizontal scroll / stale top-segment, then re-follow the cursor.
                        editor.reset_wrap_view();
                        let bottom_height = if find_replace.is_some() {
                            3
                        } else if output_pane_visible {
                            output_pane_height
                        } else {
                            0
                        };
                        editor.update_viewport_for_cursor_with_bottom(bottom_height);
                        let wrappable = matches!(
                            *editor.get_language(),
                            syntax::Language::PlainText | syntax::Language::Markdown
                        );
                        editor.status_message = Some((
                            match (now_on, wrappable) {
                                (true, true) => "Word wrap: on".to_string(),
                                (true, false) => {
                                    "Word wrap: on (applies in plain text & Markdown)".to_string()
                                }
                                (false, _) => "Word wrap: off".to_string(),
                            },
                            false,
                        ));
                        renderer.force_redraw();
                        needs_redraw = true;
                        commands::Command::None
                    }

                    // Execute Cell (Ctrl+E as alternative)
                    KeyCode::Char('e') | KeyCode::Char('E') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        // Only allow execution in REPL-mode languages (Python or SQL).
                        if !editor.is_repl_mode() {
                            editor.status_message = Some(("Cell execution only available in REPL mode (Python/SQL). Press Ctrl+Y to switch language.".to_string(), true));
                            needs_redraw = true;
                        } else if execution_rx.is_some() {
                            // Check if already executing
                            editor.status_message = Some(("Already executing (Ctrl+Backspace to cancel - WARNING: resets kernel)".to_string(), true));
                            needs_redraw = true;
                        } else {
                            // Start background execution
                            if let Some((rx, kernel_info, cancel, preserves)) = spawn_background_execution(editor) {
                                execution_rx = Some(rx);
                                execution_start_time = Some(std::time::Instant::now());
                                executing_kernel_info = Some(kernel_info);
                                executing_cancel = cancel;
                                executing_preserves_session = preserves;
                                editor.status_message = Some(("Executing...".to_string(), false));
                                needs_redraw = true;
                            } else if !editor.is_kernel_connected() {
                                // None with no kernel → prompt to connect. (When
                                // a kernel IS connected, None means "nothing to
                                // execute" and spawn already set that message.)
                                editor.status_message = Some(("No kernel connected. Press Ctrl+K to select a kernel.".to_string(), true));
                                needs_redraw = true;
                            } else {
                                needs_redraw = true;
                            }
                        }
                        commands::Command::None
                    }

                    // Clear Output Pane (Ctrl+L)
                    KeyCode::Char('l') | KeyCode::Char('L') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if editor.is_repl_mode() && output_pane_visible {
                            output_pane.clear();
                            editor.status_message = Some(("Output cleared".to_string(), false));
                            needs_redraw = true;
                        }
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
                        // Allow kernel selection in REPL-mode languages (Python or SQL).
                        let lang = *editor.get_language();
                        if lang != syntax::Language::Python && lang != syntax::Language::Sql {
                            editor.status_message = Some(("Kernel selection only available in REPL mode (Python/SQL). Press Ctrl+Y to switch language.".to_string(), true));
                            commands::Command::None
                        } else {
                        // Show loading message
                        editor.status_message = Some(("Discovering kernels...".to_string(), false));
                        renderer.draw(editor)?;
                        use std::io::Write;
                        let mut stdout = io::stdout();
                        stdout.flush()?;

                        // Create selector (this does the discovery)
                        let mut selector =
                            kernel_selector::KernelSelector::for_language(*editor.get_language());

                        execute!(io::stdout(), crossterm::cursor::Hide)?;

                        let result = match selector.run(&mut io::stdout()) {
                            Ok(r) => r,
                            Err(e) => {
                                editor.status_message = Some((format!("Selector error: {}", e), true));
                                None
                            }
                        };

                        // Clear and redraw - important to clear the entire screen
                        execute!(io::stdout(),
                            crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
                        )?;

                        // Reset terminal state completely
                        execute!(io::stdout(), crossterm::cursor::Hide)?;

                        renderer.force_redraw();

                        if let Some(kernel_info) = result {
                            // Polymorphic dispatch — DirectKernel for Python interpreters,
                            // SnowflakeKernel when info.name is the Snowflake sentinel.
                            let mut kernel = match kernel::build_from_info(&kernel_info) {
                                Ok(k) => k,
                                Err(e) => {
                                    editor.status_message = Some((format!("Failed to build kernel: {}", e), true));
                                    needs_redraw = true;
                                    continue;
                                }
                            };

                            // Disconnect old kernel first if exists
                            if editor.is_kernel_connected() {
                                let _ = editor.disconnect_kernel();
                            }

                            // Connect to kernel
                            match kernel.connect() {
                                Ok(_) => {
                                    editor.set_kernel(kernel);
                                    editor.enable_repl_mode();
                                    editor.status_message = Some(("Connected to kernel".to_string(), false));
                                }
                                Err(e) => {
                                    editor.status_message = Some((format!("Failed to connect: {}", e), true));
                                }
                            }
                        } else {
                            // User cancelled - clear any status message
                            editor.status_message = None;
                        }

                        // Force full redraw after kernel selector
                        execute!(io::stdout(),
                            crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
                        )?;
                        renderer.force_redraw();
                        needs_redraw = true;

                        commands::Command::None
                        }
                    }

                    // Snippet Library (Ctrl+J)
                    KeyCode::Char('j') | KeyCode::Char('J') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        let cfg = config::Config::load();
                        let mut picker = snippet_picker::SnippetPicker::new(cfg.snippets);

                        execute!(io::stdout(), crossterm::cursor::Hide)?;

                        let result = picker.run(&mut io::stdout());

                        // Clear and redraw
                        execute!(io::stdout(),
                            crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
                        )?;
                        execute!(io::stdout(), crossterm::cursor::Hide)?;
                        renderer.force_redraw();

                        if let Ok(Some(text)) = result {
                            editor.paste_text(text);
                        }

                        needs_redraw = true;
                        commands::Command::None
                    }

                    // Language Selection (Ctrl+Y)
                    KeyCode::Char('y') | KeyCode::Char('Y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        // Get current language
                        let current_language = editor.get_language();

                        // Create selector
                        let mut selector = language_selector::LanguageSelector::new(current_language);

                        execute!(io::stdout(), crossterm::cursor::Hide)?;

                        let result = selector.run(&mut io::stdout());

                        // Clear and redraw
                        execute!(io::stdout(),
                            crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
                        )?;

                        // Reset terminal state
                        execute!(io::stdout(), crossterm::cursor::Hide)?;

                        renderer.force_redraw();

                        if let Ok(Some(language)) = result {
                            // Set the new language
                            editor.set_language(language);
                            // Switching language can flip whether wrap is active, which
                            // changes what preferred_column means (visual vs logical); clear
                            // the wrap view state so a stale column can't cause a jump.
                            editor.reset_wrap_view();

                            // Check if we need to enable/disable REPL mode
                            if language == syntax::Language::Python {
                                // Enable REPL mode for Python
                                if !editor.is_repl_mode() {
                                    // If no kernel connected, auto-connect to first available kernel.
                                    // Filter out the Snowflake entry — this path is the explicit
                                    // "switch to Python mode" toggle and shouldn't pick a SQL kernel.
                                    if !editor.is_kernel_connected() {
                                        let kernels = kernel::discover_kernels();
                                        if let Some(kernel_info) = kernels
                                            .into_iter()
                                            .find(|k| k.name != kernel::SNOWFLAKE_KERNEL_NAME)
                                        {
                                            // Auto-connect to first kernel
                                            let mut new_kernel: Box<dyn kernel::Kernel> = Box::new(
                                                direct_kernel::DirectKernel::new(
                                                    kernel_info.python_path.clone(),
                                                    kernel_info.name.clone(),
                                                    kernel_info.display_name.clone()
                                                )
                                            );
                                            if new_kernel.connect().is_ok() {
                                                editor.set_kernel(new_kernel);
                                                editor.enable_repl_mode();
                                                editor.status_message = Some((format!("Python mode enabled with {}", kernel_info.display_name), false));
                                            } else {
                                                editor.enable_repl_mode();
                                                editor.status_message = Some(("Python mode enabled. Press Ctrl+K to select a kernel.".to_string(), false));
                                            }
                                        } else {
                                            editor.enable_repl_mode();
                                            editor.status_message = Some(("Python mode enabled but no Python found. Install Python first.".to_string(), true));
                                        }
                                    } else {
                                        editor.enable_repl_mode();
                                        editor.status_message = Some(("Switched to Python mode with REPL enabled".to_string(), false));
                                    }
                                }
                                // Show output pane for Python
                                output_pane_visible = true;
                            } else if language == syntax::Language::Sql {
                                // Enable REPL mode for SQL and auto-connect Snowflake.
                                // Mirrors the Python branch: only acts if not already in
                                // REPL mode, so switching language between two REPL-mode
                                // sessions doesn't swap a working kernel out from under
                                // the user.
                                if !editor.is_repl_mode() {
                                    if !editor.is_kernel_connected() {
                                        let info = kernel::KernelInfo {
                                            name: kernel::SNOWFLAKE_KERNEL_NAME.to_string(),
                                            display_name: "Snowflake".to_string(),
                                            python_path: String::new(),
                                        };
                                        match kernel::build_from_info(&info) {
                                            Ok(mut new_kernel) => {
                                                match new_kernel.connect() {
                                                    Ok(()) => {
                                                        let display = new_kernel.info().display_name.clone();
                                                        editor.set_kernel(new_kernel);
                                                        editor.enable_repl_mode();
                                                        editor.status_message = Some((format!("SQL mode enabled with {}", display), false));
                                                    }
                                                    Err(e) => {
                                                        editor.enable_repl_mode();
                                                        editor.status_message = Some((format!("SQL mode enabled. Snowflake connect failed: {} — press Ctrl+K to pick a kernel.", e), true));
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                editor.enable_repl_mode();
                                                editor.status_message = Some((format!("SQL mode enabled. Snowflake config missing: {} (add C:\\.dotfile\\snowflake.toml)", e), true));
                                            }
                                        }
                                    } else {
                                        editor.enable_repl_mode();
                                        editor.status_message = Some(("Switched to SQL mode with REPL enabled".to_string(), false));
                                    }
                                }
                                // Show output pane for SQL
                                output_pane_visible = true;
                            } else {
                                // Disable REPL mode for other languages
                                if editor.is_repl_mode() {
                                    editor.disable_repl_mode();
                                    editor.status_message = Some((format!("Switched to {} mode (REPL disabled)",
                                        match language {
                                            syntax::Language::PlainText => "Plain Text",
                                            syntax::Language::Sql => "SQL",
                                            syntax::Language::Rust => "Rust",
                                            syntax::Language::R => "R",
                                            syntax::Language::Yaml => "YAML",
                                            syntax::Language::Markdown => "Markdown",
                                            syntax::Language::Json => "JSON",
                                            syntax::Language::Shell => "Shell",
                                            syntax::Language::Toml => "TOML",
                                            _ => "Unknown",
                                        }
                                    ), false));
                                } else {
                                    editor.status_message = Some((format!("Switched to {} mode",
                                        match language {
                                            syntax::Language::PlainText => "Plain Text",
                                            syntax::Language::Sql => "SQL",
                                            syntax::Language::Rust => "Rust",
                                            syntax::Language::R => "R",
                                            syntax::Language::Yaml => "YAML",
                                            syntax::Language::Markdown => "Markdown",
                                            syntax::Language::Json => "JSON",
                                            syntax::Language::Shell => "Shell",
                                            syntax::Language::Toml => "TOML",
                                            _ => "Unknown",
                                        }
                                    ), false));
                                }
                                // Hide output pane for non-Python languages
                                output_pane_visible = false;
                            }
                        } else {
                            // User cancelled
                            editor.status_message = None;
                        }

                        // Force full redraw
                        execute!(io::stdout(),
                            crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
                        )?;
                        renderer.force_redraw();
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
                        } else if key.modifiers.contains(KeyModifiers::CONTROL | KeyModifiers::ALT) {
                            commands::Command::MoveLineUp
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
                        } else if key.modifiers.contains(KeyModifiers::CONTROL | KeyModifiers::ALT) {
                            commands::Command::MoveLineDown
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
                        if output_pane_visible && output_pane.is_focused() {
                            // When output pane is focused, PageUp moves cursor up by a page
                            let with_selection = key.modifiers.contains(KeyModifiers::SHIFT);
                            output_pane.page_up(with_selection);
                            needs_redraw = true;
                            commands::Command::None
                        } else if key.modifiers.contains(KeyModifiers::SHIFT) && output_pane_visible {
                            // Shift+PageUp = Scroll output pane up (when editor focused)
                            output_pane.scroll_up();
                            needs_redraw = true;
                            commands::Command::None
                        } else {
                            commands::Command::PageUp
                        }
                    }
                    KeyCode::PageDown => {
                        if output_pane_visible && output_pane.is_focused() {
                            // When output pane is focused, PageDown moves cursor down by a page
                            let with_selection = key.modifiers.contains(KeyModifiers::SHIFT);
                            output_pane.page_down(with_selection);
                            needs_redraw = true;
                            commands::Command::None
                        } else if key.modifiers.contains(KeyModifiers::SHIFT) && output_pane_visible {
                            // Shift+PageDown = Scroll output pane down (when editor focused)
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
                        // Same cancellation logic as Ctrl+Backspace — see that handler
                        // for the soft-vs-hard cancel rationale.
                        if execution_rx.is_some() {
                            if executing_preserves_session {
                                if let Some(handle) = executing_cancel.take() {
                                    handle.cancel();
                                }
                                editor.status_message = Some(("Cancelling...".to_string(), false));
                                needs_redraw = true;
                            } else {
                                execution_rx = None;
                                execution_start_time = None;
                                executing_preserves_session = false;

                                if let Some(handle) = executing_cancel.take() {
                                    handle.cancel();
                                }

                                if let Some(kernel_info) = executing_kernel_info.take() {
                                    match kernel::build_from_info(&kernel_info) {
                                        Ok(mut new_kernel) => {
                                            if new_kernel.connect().is_ok() {
                                                editor.set_kernel(new_kernel);
                                                editor.status_message = Some(("CANCELLED - Kernel reset (all variables lost)".to_string(), true));
                                            } else {
                                                editor.status_message = Some(("CANCELLED - Kernel reconnection failed".to_string(), true));
                                            }
                                        }
                                        Err(e) => {
                                            editor.status_message = Some((format!("CANCELLED - Kernel rebuild failed: {}", e), true));
                                        }
                                    }
                                } else {
                                    editor.status_message = Some(("Execution cancelled".to_string(), true));
                                }

                                renderer.force_redraw();
                                needs_redraw = true;
                            }
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
                                if let Some((rx, kernel_info, cancel, preserves)) = spawn_background_execution(editor) {
                                    execution_rx = Some(rx);
                                    execution_start_time = Some(std::time::Instant::now());
                                    executing_kernel_info = Some(kernel_info);
                                    executing_cancel = cancel;
                                    executing_preserves_session = preserves;
                                    editor.status_message = Some(("Executing...".to_string(), false));
                                    needs_redraw = true;
                                } else {
                                    // No kernel, or nothing executable — spawn set
                                    // the status message in the latter case.
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
                                if output_pane_visible {
                                    output_pane.invalidate_cache();
                                }
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
                        // Open find/replace window. Spreadsheet mode uses a find-only pane
                        // because replacing cell contents via this UI isn't supported.
                        if editor.is_spreadsheet_mode() {
                            let mut fr = find_replace::FindReplace::new_find_only();
                            if let Some(ss) = editor.spreadsheet() {
                                let matches = ss.find_cells(fr.find_text());
                                fr.update_matches(matches);
                            }
                            find_replace = Some(fr);
                        } else {
                            find_replace = Some(find_replace::FindReplace::new());
                        }
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
                        // Only enable autocomplete in REPL mode (Python)
                        if suppress_autocomplete_once {
                            // Skip autocomplete update this cycle (after Tab completion)
                            suppress_autocomplete_once = false;
                        } else if should_update_autocomplete && editor.is_repl_mode() {
                            let (base_callable, prefix, is_sql_context) = editor.get_completion_context();
                            // Debug logging
                            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/sage_debug.log") {
                                use std::io::Write;
                                let _ = writeln!(f, "DEBUG event_loop: should_update_autocomplete, base_callable={:?}, prefix='{}', is_sql={}", base_callable, prefix, is_sql_context);
                            }
                            autocomplete.update_with_context(base_callable, &prefix, is_sql_context);
                            renderer.force_redraw(); // Clear artifacts when menu changes
                            if output_pane_visible {
                                output_pane.invalidate_cache();
                            }
                        } else if should_check_backspace_delete && editor.is_repl_mode() {
                            let (base_callable, prefix, is_sql_context) = editor.get_completion_context();
                            if prefix.is_empty() && base_callable.is_none() && !is_sql_context {
                                autocomplete.hide();
                                renderer.force_redraw();
                                if output_pane_visible {
                                    output_pane.invalidate_cache();
                                }
                            } else {
                                autocomplete.update_with_context(base_callable, &prefix, is_sql_context);
                                renderer.force_redraw(); // Clear artifacts when menu changes
                                if output_pane_visible {
                                    output_pane.invalidate_cache();
                                }
                            }
                        } else if should_hide_autocomplete || !editor.is_repl_mode() {
                            autocomplete.hide();
                            renderer.force_redraw();
                            if output_pane_visible {
                                output_pane.invalidate_cache();
                            }
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
) -> Option<(std::sync::mpsc::Receiver<ExecMsg>, kernel::KernelInfo, Option<std::sync::Arc<dyn kernel::CancelHandle>>, bool)> {
    // Extract kernel from editor (temporarily)
    let mut kernel = editor.take_kernel()?;

    // Store kernel info for potential recreation, grab a cancel handle so
    // the host can abort execution from outside the background thread, and
    // record whether the kernel survives cancellation so the cancel handlers
    // know whether to wait for the channel or rebuild from scratch.
    let kernel_info = kernel.info().clone();
    let cancel = kernel.cancel_handle();
    let preserves_session = kernel.cancel_preserves_session();

    // Get selection or current cell position
    let selection = editor.selection();
    let cursor_offset = editor.cursor();  // Get byte offset, not line/col
    let is_sql = *editor.get_language() == crate::syntax::Language::Sql;

    // Build the statements to run as (header_label, code). The label is a
    // title from the cell's marker line (`--##` for SQL, the `##--` delimiter
    // for other languages), falling back to "Cell N".
    editor.update_cells();
    let cells: Vec<(String, String)> = {
        use crate::cell::{
            cell_title, get_cell_at_position, get_cell_content, has_executable_content,
            statements_in_text,
        };

        if let Some((sel_start, sel_end)) = selection {
            // Explicit selection: treat the selected text as its own buffer and
            // run the statements inside it (split on semicolons for SQL). We run
            // exactly what's selected — nothing before sel_start or after
            // sel_end — so selecting a statement plus the blank line that
            // follows it never drags the next statement into the run.
            let selected = editor
                .buffer_rope()
                .byte_slice(sel_start..sel_end)
                .to_string();
            statements_in_text(&selected, is_sql)
                .into_iter()
                .enumerate()
                .map(|(i, (title, code))| {
                    let label = title.unwrap_or_else(|| format!("Cell {}", i + 1));
                    (label, code)
                })
                .collect()
        } else if let Some(cell_idx) =
            get_cell_at_position(editor.get_cells_ref(), cursor_offset)
        {
            // No selection: run the single statement under the caret, unless
            // it has nothing executable (only comments / blank lines).
            let cell = &editor.get_cells_ref()[cell_idx];
            let code = get_cell_content(editor.buffer_rope(), cell);
            if has_executable_content(&code, is_sql) {
                let full = editor.buffer_rope().byte_slice(cell.start..cell.end).to_string();
                let label = cell_title(&full, is_sql).unwrap_or_else(|| format!("Cell {}", cell_idx + 1));
                vec![(label, code)]
            } else {
                vec![]
            }
        } else {
            vec![]
        }
    };

    if cells.is_empty() {
        // Nothing executable (a comment-only or blank cell/selection). Restore
        // the kernel and explain why nothing ran — callers distinguish this
        // from the no-kernel case via is_kernel_connected().
        editor.set_kernel(kernel);
        editor.status_message = Some(("Nothing to execute".to_string(), false));
        return None;
    }

    // Application lane: a single Python program that would take over its own
    // event loop (a GUI app / game — see cell::is_standalone_program) can't run
    // in the shared kernel without blocking the REPL thread for its whole
    // lifetime. Instead run just this cell as its own OS process, but keep it in
    // the foreground: capture stdout/stderr and stream the result — including a
    // crash traceback — back into the output pane, exactly like a cell. We run
    // only the current cell's code (not the whole buffer) so `##--` separators
    // are respected: running the app cell doesn't drag sibling cells along. sage
    // shows "Executing..." while the app's window is open; Ctrl+Backspace kills
    // the app (soft cancel — the kernel is never touched and is handed straight
    // back via Done). Explicit selections and SQL always stay in-session; a
    // Snowflake kernel (empty python_path) has no interpreter to run, so skip.
    if !is_sql
        && selection.is_none()
        && cells.len() == 1
        && !kernel_info.python_path.is_empty()
        && crate::cell::is_standalone_program(&cells[0].1)
    {
        let program = cells[0].1.clone();
        match spawn_app_process(&kernel_info.python_path, &program) {
            Ok(child) => {
                let cancel: Option<std::sync::Arc<dyn kernel::CancelHandle>> = Some(
                    std::sync::Arc::new(direct_kernel::ProcessKillHandle::new(child.id())),
                );
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let start = std::time::Instant::now();
                    let (output, is_error) = run_and_capture(child);
                    let _ = tx.send(ExecMsg::Cell {
                        label: "application".to_string(),
                        output,
                        is_error,
                        elapsed: start.elapsed().as_secs_f64(),
                    });
                    let _ = tx.send(ExecMsg::Done {
                        kernel,
                        completions: Vec::new(),
                        type_relationships: kernel::TypeRelationships::default(),
                        sql_metadata: kernel::SqlMetadata::default(),
                    });
                });
                // preserves_session = true → soft cancel: Ctrl+Backspace kills
                // the app and the thread hands the (unused) kernel back via Done.
                return Some((rx, kernel_info, cancel, true));
            }
            Err(e) => {
                editor.set_kernel(kernel);
                editor.status_message =
                    Some((format!("Failed to launch application: {}", e), true));
                return None;
            }
        }
    }

    // Spawn background thread. Sends one ExecMsg::Cell per statement as it
    // finishes — the host drains the channel and paints incrementally — then
    // a single ExecMsg::Done hands the kernel back and carries the final
    // autocomplete metadata snapshot.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut all_completions = Vec::new();
        let mut type_relationships = crate::kernel::TypeRelationships::default();
        let mut sql_metadata = crate::kernel::SqlMetadata::default();

        for (label, code) in cells {
            let start_time = std::time::Instant::now();

            match kernel.execute(&code) {
                Ok(result) => {
                    let elapsed = start_time.elapsed().as_secs_f64();
                    let output_text = crate::cell::format_output(&result);
                    let is_error = !result.success;

                    all_completions.extend(result.completions);
                    type_relationships = result.type_relationships;
                    sql_metadata = result.sql_metadata;

                    let _ = tx.send(ExecMsg::Cell {
                        label,
                        output: output_text,
                        is_error,
                        elapsed,
                    });

                    if is_error {
                        break;
                    }
                }
                Err(e) => {
                    let elapsed = start_time.elapsed().as_secs_f64();
                    let _ = tx.send(ExecMsg::Cell {
                        label,
                        output: format!("Error: {}", e),
                        is_error: true,
                        elapsed,
                    });
                    break;
                }
            }
        }

        let _ = tx.send(ExecMsg::Done {
            kernel,
            completions: all_completions,
            type_relationships,
            sql_metadata,
        });
    });

    Some((rx, kernel_info, cancel, preserves_session))
}

/// Spawn `source` as a standalone Python program (its own OS process) with
/// stdout/stderr piped so the host can capture them. The GUI window still
/// appears; only the console is suppressed.
///
/// The program is written to a temp `.py` so the child runs a real file (a
/// correct `__file__`, tracebacks, and `__name__ == "__main__"`). It inherits
/// none of sage's stdio — sage is a raw-mode TUI, and letting the child touch
/// those terminal handles (or opening a new console against them) is what made
/// an earlier version flash-and-die. `CREATE_NO_WINDOW` keeps it off sage's
/// console entirely while still allowing its own GUI window.
fn spawn_app_process(python_path: &str, source: &str) -> io::Result<std::process::Child> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static APP_SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = APP_SEQ.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("sage_app_{}_{}.py", std::process::id(), seq));
    std::fs::File::create(&path)?.write_all(source.as_bytes())?;

    let mut cmd = std::process::Command::new(python_path);
    cmd.arg(&path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: the app never attaches to / flashes sage's raw-mode
        // console; its own GUI window still appears, and we read stdout/stderr
        // through the pipes.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn()
}

/// Wait for a standalone app to exit and turn its captured output into a pane
/// entry. stderr (where Python tracebacks land) is appended after stdout; a
/// non-zero exit marks the entry as an error so it renders red, just like a cell
/// error. Blocks until the process exits — the app's window being open is what
/// keeps sage in the "Executing..." state.
fn run_and_capture(child: std::process::Child) -> (String, bool) {
    match child.wait_with_output() {
        Ok(out) => {
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            let err = String::from_utf8_lossy(&out.stderr);
            if !err.trim().is_empty() {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&err);
            }
            let is_error = !out.status.success();
            let text = text.trim_end().to_string();
            if text.is_empty() {
                let note = if is_error {
                    match out.status.code() {
                        Some(c) => format!("Application exited with code {}", c),
                        None => "Application terminated".to_string(),
                    }
                } else {
                    "Application exited.".to_string()
                };
                return (note, is_error);
            }
            (text, is_error)
        }
        Err(e) => (format!("Failed to run application: {}", e), true),
    }
}

fn ensure_ss_cursor_visible(editor: &mut editor::Editor) -> io::Result<()> {
    ensure_ss_cursor_visible_with_bottom(editor, 0)
}

fn ensure_ss_cursor_visible_with_bottom(
    editor: &mut editor::Editor,
    bottom_pane_height: usize,
) -> io::Result<()> {
    use crate::spreadsheet::FORMULA_BAR_HEIGHT;
    let (width, height) = crossterm::terminal::size()?;
    let data_start = FORMULA_BAR_HEIGHT + 2;
    let status_row = (height as usize).saturating_sub(1 + bottom_pane_height);
    let visible_data_rows = status_row.saturating_sub(data_start);
    if let Some(ss) = editor.spreadsheet_mut() {
        ss.ensure_cursor_visible(visible_data_rows, width as usize);
    }
    Ok(())
}

enum SsFindAction {
    Close,
    Handled,
}

/// Handle a key event while the find pane is open in spreadsheet mode.
/// Matches are cells identified by (row, col); moving to a match selects that cell.
fn handle_spreadsheet_find_key(
    editor: &mut editor::Editor,
    fr: &mut find_replace::FindReplace,
    key: &event::KeyEvent,
) -> SsFindAction {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    // Ctrl+F / Ctrl+Shift+F: cycle next/prev match without touching the find text.
    if ctrl {
        match key.code {
            KeyCode::Char('f') | KeyCode::Char('F') if !shift => {
                if let Some((r, c)) = fr.next_match() {
                    if let Some(ss) = editor.spreadsheet_mut() {
                        ss.move_to(r, c, false);
                    }
                }
                return SsFindAction::Handled;
            }
            KeyCode::Char('f') | KeyCode::Char('F') if shift => {
                if let Some((r, c)) = fr.prev_match() {
                    if let Some(ss) = editor.spreadsheet_mut() {
                        ss.move_to(r, c, false);
                    }
                }
                return SsFindAction::Handled;
            }
            _ => {}
        }
    }

    let result = fr.handle_input(key.code, key.modifiers);
    match result {
        find_replace::InputResult::Close => SsFindAction::Close,
        find_replace::InputResult::FindTextChanged => {
            let matches = editor
                .spreadsheet()
                .map(|ss| ss.find_cells(fr.find_text()))
                .unwrap_or_default();
            fr.set_matches_from_start(matches);
            if let Some((r, c)) = fr.current_match_position() {
                if let Some(ss) = editor.spreadsheet_mut() {
                    ss.move_to(r, c, false);
                }
            }
            SsFindAction::Handled
        }
        find_replace::InputResult::FindNext => {
            if let Some((r, c)) = fr.next_match() {
                if let Some(ss) = editor.spreadsheet_mut() {
                    ss.move_to(r, c, false);
                }
            }
            SsFindAction::Handled
        }
        find_replace::InputResult::Continue => SsFindAction::Handled,
    }
}

fn handle_spreadsheet_mouse(
    editor: &mut editor::Editor,
    me: &event::MouseEvent,
    needs_redraw: &mut bool,
) -> io::Result<()> {
    use crate::spreadsheet::{GridHit, MouseMode};
    let (width, height) = crossterm::terminal::size()?;
    let shift = me.modifiers.contains(KeyModifiers::SHIFT);
    let Some(ss) = editor.spreadsheet_mut() else { return Ok(()) };

    match me.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let hit = ss.hit_test(me.column, me.row, width, height);
            match hit {
                GridHit::ColumnSeparator { col } => {
                    if ss.is_editing() {
                        ss.commit_edit();
                    }
                    ss.begin_mouse_column_resize(col, me.column);
                }
                GridHit::DataCell { row, col } => {
                    ss.begin_mouse_cell_select(row, col, shift);
                }
                GridHit::ColumnHeader { col } => {
                    if ss.is_editing() {
                        ss.commit_edit();
                    }
                    ss.select_column(col, shift);
                    ss.mouse_mode = MouseMode::ColumnSelect;
                }
                GridHit::RowNumber { row } => {
                    if ss.is_editing() {
                        ss.commit_edit();
                    }
                    ss.select_row(row, shift);
                    ss.mouse_mode = MouseMode::RowSelect;
                }
                GridHit::FormulaBar { row, text_col } => {
                    ss.begin_mouse_formula_bar_select(row, text_col, shift);
                }
                GridHit::Divider | GridHit::Outside => {}
            }
            *needs_redraw = true;
        }
        MouseEventKind::Drag(MouseButton::Left) => match ss.mouse_mode {
            MouseMode::ColumnResize {
                col,
                anchor_screen_col,
                anchor_width,
            } => {
                let delta = me.column as i32 - anchor_screen_col as i32;
                let new_width = (anchor_width as i32 + delta).max(1);
                ss.set_column_width(col, new_width as usize);
                *needs_redraw = true;
            }
            MouseMode::CellSelect => {
                let hit = ss.hit_test(me.column, me.row, width, height);
                match hit {
                    GridHit::DataCell { row, col } => {
                        ss.move_to(row, col, true);
                        *needs_redraw = true;
                    }
                    GridHit::RowNumber { row } => {
                        let cur_col = ss.cursor.1;
                        ss.move_to(row, cur_col, true);
                        *needs_redraw = true;
                    }
                    GridHit::ColumnHeader { col } => {
                        let cur_row = ss.cursor.0;
                        ss.move_to(cur_row, col, true);
                        *needs_redraw = true;
                    }
                    _ => {}
                }
            }
            MouseMode::ColumnSelect => {
                let hit = ss.hit_test(me.column, me.row, width, height);
                let target_col = match hit {
                    GridHit::ColumnHeader { col } => Some(col),
                    GridHit::DataCell { col, .. } => Some(col),
                    GridHit::ColumnSeparator { col } => Some(col),
                    _ => None,
                };
                if let Some(col) = target_col {
                    ss.extend_column_selection(col);
                    *needs_redraw = true;
                }
            }
            MouseMode::RowSelect => {
                let hit = ss.hit_test(me.column, me.row, width, height);
                let target_row = match hit {
                    GridHit::RowNumber { row } => Some(row),
                    GridHit::DataCell { row, .. } => Some(row),
                    _ => None,
                };
                if let Some(row) = target_row {
                    ss.extend_row_selection(row);
                    *needs_redraw = true;
                }
            }
            MouseMode::FormulaBarSelect => {
                let hit = ss.hit_test(me.column, me.row, width, height);
                if let GridHit::FormulaBar { row, text_col } = hit {
                    let byte = ss.formula_bar_text_to_byte(row, text_col);
                    ss.edit_set_cursor(byte, true);
                    *needs_redraw = true;
                }
            }
            MouseMode::None => {}
        },
        MouseEventKind::Up(MouseButton::Left) => {
            ss.end_mouse();
            *needs_redraw = true;
        }
        MouseEventKind::ScrollDown => {
            if shift {
                ss.scroll_by(0, 3);
            } else {
                ss.scroll_by(3, 0);
            }
            *needs_redraw = true;
        }
        MouseEventKind::ScrollUp => {
            if shift {
                ss.scroll_by(0, -3);
            } else {
                ss.scroll_by(-3, 0);
            }
            *needs_redraw = true;
        }
        MouseEventKind::ScrollLeft => {
            ss.scroll_by(0, -3);
            *needs_redraw = true;
        }
        MouseEventKind::ScrollRight => {
            ss.scroll_by(0, 3);
            *needs_redraw = true;
        }
        _ => {}
    }
    Ok(())
}

/// Handle a key in spreadsheet mode. Returns true if consumed, false if it should fall through
/// to the normal editor handling (used for Ctrl+S, Ctrl+Shift+S, Ctrl+Q which use the standard
/// save/exit prompt flows).
fn handle_spreadsheet_key(
    editor: &mut editor::Editor,
    key: &event::KeyEvent,
    needs_redraw: &mut bool,
) -> bool {
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    // Let save/quit go through the normal prompt dialogs
    if ctrl && !alt {
        match key.code {
            KeyCode::Char('s') | KeyCode::Char('S') => {
                if let Some(ss) = editor.spreadsheet_mut() {
                    if ss.is_editing() {
                        ss.commit_edit();
                    }
                }
                return false;
            }
            KeyCode::Char('q') | KeyCode::Char('Q') => {
                if let Some(ss) = editor.spreadsheet_mut() {
                    if ss.is_editing() {
                        ss.cancel_edit();
                    }
                }
                return false;
            }
            KeyCode::Char('f') | KeyCode::Char('F') if !shift => {
                if let Some(ss) = editor.spreadsheet_mut() {
                    if ss.is_editing() {
                        ss.commit_edit();
                    }
                }
                return false;
            }
            _ => {}
        }
    }

    let editing = match editor.spreadsheet() {
        Some(ss) => ss.is_editing(),
        None => return false,
    };

    if editing {
        let ss = editor.spreadsheet_mut().expect("spreadsheet");
        match key.code {
            KeyCode::Esc => {
                ss.cancel_edit();
                *needs_redraw = true;
                true
            }
            KeyCode::Enter if shift => {
                ss.edit_insert_newline();
                *needs_redraw = true;
                true
            }
            KeyCode::Enter => {
                ss.commit_edit();
                *needs_redraw = true;
                true
            }
            KeyCode::Tab => {
                ss.commit_edit();
                if shift {
                    ss.move_left(false);
                } else {
                    ss.move_right(false);
                }
                *needs_redraw = true;
                true
            }
            KeyCode::BackTab => {
                ss.commit_edit();
                ss.move_left(false);
                *needs_redraw = true;
                true
            }
            KeyCode::Backspace => {
                ss.edit_backspace();
                *needs_redraw = true;
                true
            }
            KeyCode::Delete => {
                ss.edit_delete();
                *needs_redraw = true;
                true
            }
            KeyCode::Left => {
                ss.edit_move_left(shift);
                *needs_redraw = true;
                true
            }
            KeyCode::Right => {
                ss.edit_move_right(shift);
                *needs_redraw = true;
                true
            }
            KeyCode::Up => {
                ss.edit_move_up(shift);
                *needs_redraw = true;
                true
            }
            KeyCode::Down => {
                ss.edit_move_down(shift);
                *needs_redraw = true;
                true
            }
            KeyCode::Home => {
                ss.edit_move_home(shift);
                *needs_redraw = true;
                true
            }
            KeyCode::End => {
                ss.edit_move_end(shift);
                *needs_redraw = true;
                true
            }
            KeyCode::Char('a') | KeyCode::Char('A') if ctrl && !alt => {
                ss.edit_select_all();
                *needs_redraw = true;
                true
            }
            KeyCode::Char('c') | KeyCode::Char('C') if ctrl && !alt => {
                if let Some(text) = ss.edit_get_selected_text() {
                    if let Ok(mut cb) = arboard::Clipboard::new() {
                        let _ = cb.set_text(text);
                    }
                }
                true
            }
            KeyCode::Char('x') | KeyCode::Char('X') if ctrl && !alt => {
                if let Some(text) = ss.edit_get_selected_text() {
                    if let Ok(mut cb) = arboard::Clipboard::new() {
                        let _ = cb.set_text(text);
                    }
                    ss.edit_paste("");
                    *needs_redraw = true;
                }
                true
            }
            KeyCode::Char('v') | KeyCode::Char('V') if ctrl && !alt => {
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    if let Ok(text) = cb.get_text() {
                        ss.edit_paste(&text);
                        *needs_redraw = true;
                    }
                }
                true
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                ss.edit_insert_char(c);
                *needs_redraw = true;
                true
            }
            _ => true,
        }
    } else {
        // Navigation mode
        let ss = editor.spreadsheet_mut().expect("spreadsheet");
        match key.code {
            KeyCode::Up => {
                ss.move_up(shift);
                *needs_redraw = true;
                true
            }
            KeyCode::Down => {
                ss.move_down(shift);
                *needs_redraw = true;
                true
            }
            KeyCode::Left => {
                ss.move_left(shift);
                *needs_redraw = true;
                true
            }
            KeyCode::Right => {
                ss.move_right(shift);
                *needs_redraw = true;
                true
            }
            KeyCode::Home if ctrl => {
                ss.move_top_left(shift);
                *needs_redraw = true;
                true
            }
            KeyCode::Home => {
                ss.move_home(shift);
                *needs_redraw = true;
                true
            }
            KeyCode::End if ctrl => {
                ss.move_bottom_right(shift);
                *needs_redraw = true;
                true
            }
            KeyCode::End => {
                ss.move_end(shift);
                *needs_redraw = true;
                true
            }
            KeyCode::PageUp => {
                ss.page_up(20, shift);
                *needs_redraw = true;
                true
            }
            KeyCode::PageDown => {
                ss.page_down(20, shift);
                *needs_redraw = true;
                true
            }
            KeyCode::Enter | KeyCode::F(2) => {
                ss.enter_edit_mode();
                *needs_redraw = true;
                true
            }
            KeyCode::Tab => {
                if shift {
                    ss.move_left(false);
                } else {
                    ss.move_right(false);
                }
                *needs_redraw = true;
                true
            }
            KeyCode::BackTab => {
                ss.move_left(false);
                *needs_redraw = true;
                true
            }
            KeyCode::Backspace | KeyCode::Delete => {
                ss.clear_selection_content();
                *needs_redraw = true;
                true
            }
            KeyCode::Esc => {
                ss.selection_anchor = None;
                *needs_redraw = true;
                true
            }
            KeyCode::Char('a') | KeyCode::Char('A') if ctrl && !alt => {
                ss.select_all();
                *needs_redraw = true;
                true
            }
            KeyCode::Char('c') | KeyCode::Char('C') if ctrl && !alt => {
                let text = ss.copy_selection_tsv();
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    let _ = cb.set_text(text);
                }
                true
            }
            KeyCode::Char('x') | KeyCode::Char('X') if ctrl && !alt => {
                let text = ss.copy_selection_tsv();
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    let _ = cb.set_text(text);
                }
                ss.clear_selection_content();
                *needs_redraw = true;
                true
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                ss.enter_edit_mode_replace(c);
                *needs_redraw = true;
                true
            }
            _ => true,
        }
    }
}

/// Copy the kernel's CSV result spool at `src` into a freshly created temp CSV
/// file holding the header row plus up to `max_data_rows` data rows, and return
/// its (persisted) path along with the number of data rows written.
///
/// Used by the "open results in a new sage session" keybinding (F8 in SQL
/// mode). The spool is already CSV and sage opens CSV directly, so this only
/// truncates — no re-encoding. The copy is byte-accurate (via
/// [`crate::dsv::copy_first_records`]), so the null/empty encoding and any
/// quoting are preserved exactly, and a quoted field with an embedded newline
/// still counts as one row. The file is opened by a separate `sage` process, so
/// it is kept rather than deleted on drop and lives in the OS temp dir until the
/// OS cleans it up.
fn results_head_tempfile(
    src: &std::path::Path,
    max_data_rows: usize,
) -> Result<(std::path::PathBuf, usize), Box<dyn std::error::Error>> {
    let infile = std::fs::File::open(src)?;
    let mut tmp = tempfile::Builder::new()
        .prefix("sage-results-")
        .suffix(".csv")
        .tempfile()?;
    // +1 for the header row; copy_first_records returns the total records copied.
    let records =
        crate::dsv::copy_first_records(infile, tmp.as_file_mut(), max_data_rows.saturating_add(1))?;
    // Persist the tempfile so the child sage process can still read it after
    // this process moves on (and after the parent eventually exits).
    let (_file, path) = tmp.keep()?;
    Ok((path, records.saturating_sub(1)))
}
