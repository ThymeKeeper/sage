use crate::editor::Editor;
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    execute,
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, SetTitle},
};
use std::io::{self, Write};
use unicode_width::UnicodeWidthChar;

pub struct Renderer {
    stdout: io::Stdout,
    last_size: (u16, u16),
    last_screen: Vec<String>,  // Store what we last rendered
    last_status: String,        // Store last status line
    last_title: String,         // Store last terminal title
    #[cfg(target_os = "windows")]
    needs_full_redraw: bool,
}

impl Renderer {
    pub fn new() -> io::Result<Self> {
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, Hide)?;
        
        // Initial clear
        execute!(stdout, Clear(ClearType::All))?;
        
        let (width, height) = terminal::size()?;
        
        Ok(Renderer {
            stdout,
            last_size: (width, height),
            last_screen: vec![String::new(); height as usize],
            last_status: String::new(),
            last_title: String::new(),
            #[cfg(target_os = "windows")]
            needs_full_redraw: true,
        })
    }
    
    pub fn cleanup(&mut self) -> io::Result<()> {
        // Reset terminal title
        execute!(self.stdout, SetTitle(""))?;
        execute!(self.stdout, Show, LeaveAlternateScreen)?;
        Ok(())
    }
    
    pub fn draw(&mut self, editor: &mut Editor) -> io::Result<()> {
        // Update terminal title with filename and modified indicator
        let file_name = editor.file_name();
        let modified_indicator = if editor.is_modified() { " *" } else { "" };
        
        let title = if file_name == "[No Name]" {
            format!("No Name{}", modified_indicator)
        } else {
            format!("{}{}", file_name, modified_indicator)
        };
        
        if title != self.last_title {
            execute!(self.stdout, SetTitle(&title))?;
            self.last_title = title;
        }
        
        let (width, height) = terminal::size()?;
        
        // Handle resize
        if (width, height) != self.last_size {
            self.last_size = (width, height);
            self.last_screen = vec![String::new(); height as usize];
            self.last_status.clear();
            execute!(self.stdout, Clear(ClearType::All))?;
            #[cfg(target_os = "windows")]
            {
                self.needs_full_redraw = true;
            }
        }
        
        // Update viewport - normal height calculation
        let content_height = height.saturating_sub(1) as usize; // Reserve 1 for status
        editor.update_viewport(content_height, width as usize);
        
        let viewport_offset = editor.viewport_offset();
        let buffer = editor.buffer();
        let selection = editor.selection();
        
        // Hide cursor while drawing
        #[cfg(target_os = "windows")]
        write!(self.stdout, "\x1b[?25l")?;
        
        #[cfg(not(target_os = "windows"))]
        execute!(self.stdout, Hide)?;
        
        // Draw all lines
        for screen_row in 0..content_height {
            let mut line_content = String::with_capacity(width as usize);
            
            // Calculate which logical line we're displaying
            // Logical lines: 0 and 1 are virtual, 2+ map to buffer lines 0+
            let logical_line = viewport_offset.0 + screen_row;
            
            if logical_line < 2 {
                // Virtual lines before the buffer - respect horizontal scrolling
                if viewport_offset.1 == 0 {
                    // Only show the ~ if we're not horizontally scrolled
                    line_content.push('~');
                    for _ in 1..width {
                        line_content.push(' ');
                    }
                } else {
                    // If horizontally scrolled, show all spaces
                    for _ in 0..width {
                        line_content.push(' ');
                    }
                }
            } else {
                // Map logical line to buffer line (subtract 2 for the virtual lines)
                let file_row = logical_line - 2;
                
                if file_row < buffer.len_lines() {
                    let line = buffer.line(file_row);
                    let line_display = if line.ends_with('\n') {
                        &line[..line.len() - 1]
                    } else {
                        &line
                    };
                    
                    // Calculate byte positions for this line
                    let line_byte_start = buffer.line_to_byte(file_row);
                    
                    // Build line with selection highlighting and proper Unicode width handling
                    let mut formatted_line = String::new();
                    let mut byte_pos = line_byte_start;
                    let mut display_col = 0;  // Display column position (accounts for wide chars)
                    let mut screen_col = 0;    // Screen column position after horizontal scroll
                    
                    for ch in line_display.chars() {
                        // Get the display width of this character (0, 1, or 2 columns)
                        let char_width = ch.width().unwrap_or(1);
                        
                        // Check if we're past the horizontal scroll offset
                        if display_col + char_width > viewport_offset.1 {
                            // Check if this character fits on screen
                            if screen_col + char_width > width as usize {
                                // Character doesn't fit, stop here
                                break;
                            }
                            
                            // For the first character after scroll, handle partial visibility
                            if display_col < viewport_offset.1 && char_width > 1 {
                                // Wide character is partially cut off by horizontal scroll
                                // Skip it and add padding space
                                formatted_line.push(' ');
                                screen_col += 1;
                            } else {
                                // Check if this character is selected
                                let is_selected = selection.map_or(false, |(sel_start, sel_end)| {
                                    byte_pos >= sel_start && byte_pos < sel_end
                                });
                                
                                #[cfg(target_os = "windows")]
                                {
                                    if is_selected {
                                        formatted_line.push_str("\x1b[48;5;27m"); // Blue background
                                    }
                                    formatted_line.push(ch);
                                    if is_selected {
                                        formatted_line.push_str("\x1b[0m"); // Reset
                                    }
                                }
                                
                                #[cfg(not(target_os = "windows"))]
                                {
                                    if is_selected {
                                        formatted_line.push_str("\x1b[48;5;27m"); // Blue background
                                    }
                                    formatted_line.push(ch);
                                    if is_selected {
                                        formatted_line.push_str("\x1b[0m"); // Reset
                                    }
                                }
                                
                                screen_col += char_width;
                            }
                        }
                        
                        byte_pos += ch.len_utf8();
                        display_col += char_width;
                    }
                    
                    // Pad the rest of the line with spaces
                    while screen_col < width as usize {
                        formatted_line.push(' ');
                        screen_col += 1;
                    }
                    
                    line_content = formatted_line;
                } else {
                    // Virtual line after the buffer - respect horizontal scrolling
                    if viewport_offset.1 == 0 {
                        // Only show the ~ if we're not horizontally scrolled
                        line_content.push('~');
                        for _ in 1..width {
                            line_content.push(' ');
                        }
                    } else {
                        // If horizontally scrolled, show all spaces
                        for _ in 0..width {
                            line_content.push(' ');
                        }
                    }
                }
            }
            
            // Only update if this line has changed
            #[cfg(target_os = "windows")]
            {
                if self.needs_full_redraw || self.last_screen.get(screen_row) != Some(&line_content) {
                    write!(self.stdout, "\x1b[{};1H{}", screen_row + 1, line_content)?;
                    if screen_row < self.last_screen.len() {
                        self.last_screen[screen_row] = line_content;
                    }
                }
            }
            
            #[cfg(not(target_os = "windows"))]
            {
                if self.last_screen.get(screen_row) != Some(&line_content) {
                    execute!(self.stdout, MoveTo(0, screen_row as u16))?;
                    print!("{}", line_content);
                    if screen_row < self.last_screen.len() {
                        self.last_screen[screen_row] = line_content;
                    }
                }
            }
        }
        
        // Build status line
        let status_row = height as usize - 1;
        let modified_indicator = if editor.is_modified() { "*" } else { "" };
        let file_name = editor.file_name();
        let (line, col) = editor.cursor_position();
        
        let left_status = format!(" {}{} ", file_name, modified_indicator);
        let right_status = format!(" {}:{} ", line + 1, col + 1);
        
        let mut status_line = String::with_capacity(width as usize);
        status_line.push_str(&left_status);
        let padding = width as usize - left_status.len() - right_status.len();
        for _ in 0..padding {
            status_line.push(' ');
        }
        status_line.push_str(&right_status);
        
        // Only update status if it changed
        #[cfg(target_os = "windows")]
        {
            if self.needs_full_redraw || status_line != self.last_status {
                write!(self.stdout, 
                    "\x1b[{};1H\x1b[48;5;238m\x1b[38;5;15m{}\x1b[0m", 
                    height, status_line)?;
                self.last_status = status_line;
            }
            self.needs_full_redraw = false;
        }
        
        #[cfg(not(target_os = "windows"))]
        {
            if status_line != self.last_status {
                execute!(
                    self.stdout,
                    MoveTo(0, status_row as u16),
                    crossterm::style::SetBackgroundColor(crossterm::style::Color::DarkGrey),
                    crossterm::style::SetForegroundColor(crossterm::style::Color::White),
                    crossterm::style::Print(&status_line),
                    crossterm::style::ResetColor
                )?;
                self.last_status = status_line;
            }
        }
        
        // Position cursor - map buffer position to screen position
        let (cursor_line, cursor_col) = editor.cursor_position();
        let logical_cursor_line = cursor_line + 2; // Add 2 for virtual lines before buffer
        
        if logical_cursor_line >= viewport_offset.0 && 
           logical_cursor_line < viewport_offset.0 + content_height &&
           cursor_col >= viewport_offset.1 &&
           cursor_col < viewport_offset.1 + width as usize {
            
            let screen_row = logical_cursor_line - viewport_offset.0;
            let screen_col = cursor_col - viewport_offset.1;
            
            #[cfg(target_os = "windows")]
            write!(self.stdout, "\x1b[{};{}H\x1b[?25h", 
                screen_row + 1, screen_col + 1)?;
            
            #[cfg(not(target_os = "windows"))]
            execute!(
                self.stdout,
                MoveTo(screen_col as u16, screen_row as u16),
                Show
            )?;
        }
        
        self.stdout.flush()?;
        Ok(())
    }
    
    /// Force a complete screen clear on the next draw (Windows fix)
    #[cfg(target_os = "windows")]
    pub fn force_clear(&mut self) {
        self.needs_full_redraw = true;
        execute!(self.stdout, Clear(ClearType::All)).ok();
    }
}