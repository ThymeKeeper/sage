use crate::editor::Editor;
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    execute,
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use std::io::{self, Write};

pub struct Renderer {
    stdout: io::Stdout,
    last_size: (u16, u16),
    last_screen: Vec<String>,  // Store what we last rendered
    last_status: String,        // Store last status line
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
            #[cfg(target_os = "windows")]
            needs_full_redraw: true,
        })
    }
    
    pub fn cleanup(&mut self) -> io::Result<()> {
        execute!(self.stdout, Show, LeaveAlternateScreen)?;
        Ok(())
    }
    
    pub fn draw(&mut self, editor: &mut Editor) -> io::Result<()> {
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
                    
                    // Handle horizontal scrolling
                    let display_start = viewport_offset.1.min(line_display.len());
                    let display_text = &line_display[display_start..];
                    
                    // Add visible text
                    let mut chars_written = 0;
                    for ch in display_text.chars() {
                        if chars_written >= width as usize {
                            break;
                        }
                        line_content.push(ch);
                        chars_written += 1;
                    }
                    
                    // Pad with spaces
                    while chars_written < width as usize {
                        line_content.push(' ');
                        chars_written += 1;
                    }
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