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
    last_cursor_style: CursorStyle, // Track cursor style to avoid redundant updates
    #[cfg(target_os = "windows")]
    needs_full_redraw: bool,
}

#[derive(PartialEq, Clone, Copy)]
enum CursorStyle {
    Block,
    Underline,
}

impl Renderer {
    pub fn new() -> io::Result<Self> {
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, Hide)?;
        
        // Set initial cursor style and color
        write!(stdout, "\x1b[2 q")?; // Steady block cursor
        write!(stdout, "\x1b]12;#5F9EA0\x07")?; // Cadet blue - muted professional cyan
        
        // Alternative professional colors you can try:
        // write!(stdout, "\x1b]12;#708090\x07")?; // Slate grey (very muted)
        // write!(stdout, "\x1b]12;#4682B4\x07")?; // Steel blue (professional)
        // write!(stdout, "\x1b]12;#5F8787\x07")?; // Muted teal
        // write!(stdout, "\x1b]12;#6C8C8C\x07")?; // Steel blue-grey
        // write!(stdout, "\x1b]12;#7B9FAF\x07")?; // Light slate blue
        // write!(stdout, "\x1b]12;#8BA4B0\x07")?; // Muted sky blue
        // write!(stdout, "\x1b]12;#6495ED\x07")?; // Cornflower blue
        // write!(stdout, "\x1b]12;#B0C4DE\x07")?; // Light steel blue (very subtle)
        stdout.flush()?;
        
        // Set consistent background color regardless of how we're launched
        // This ensures the same appearance whether launched from terminal or Explorer
        write!(stdout, "\x1b[48;2;22;22;22m")?; // Dark neutral grey background
        execute!(stdout, Clear(ClearType::All))?;
        write!(stdout, "\x1b[0m")?; // Reset after clear
        
        let (width, height) = terminal::size()?;
        
        Ok(Renderer {
            stdout,
            last_size: (width, height),
            last_screen: vec![String::new(); height as usize],
            last_status: String::new(),
            last_title: String::new(),
            last_cursor_style: CursorStyle::Block,
            #[cfg(target_os = "windows")]
            needs_full_redraw: true,
        })
    }
    
    pub fn cleanup(&mut self) -> io::Result<()> {
        // Reset cursor style and color to terminal defaults
        write!(self.stdout, "\x1b[0 q")?; // Reset cursor style
        write!(self.stdout, "\x1b]112\x07")?; // Reset cursor color to default
        self.stdout.flush()?;
        // Reset terminal title
        execute!(self.stdout, SetTitle(""))?;
        execute!(self.stdout, Show, LeaveAlternateScreen)?;
        Ok(())
    }
    
    pub fn draw(&mut self, editor: &mut Editor) -> io::Result<()> {
        self.draw_with_bottom_window(editor, 0)
    }
    
    pub fn draw_with_bottom_window(&mut self, editor: &mut Editor, bottom_window_height: usize) -> io::Result<()> {
        // Update cursor style based on selection - but only if find/replace is closed
        if bottom_window_height == 0 {
            let desired_style = if editor.selection().is_some() {
                CursorStyle::Underline
            } else {
                CursorStyle::Block
            };
            
            // Always write the cursor style to ensure it's correct
            match desired_style {
                CursorStyle::Block => write!(self.stdout, "\x1b[2 q")?,
                CursorStyle::Underline => write!(self.stdout, "\x1b[4 q")?,
            }
            self.last_cursor_style = desired_style;
        }
        
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
            self.last_cursor_style = CursorStyle::Block; // Force cursor style refresh on resize
            // Maintain consistent background on resize
            write!(self.stdout, "\x1b[48;2;22;22;22m")?; // Dark neutral grey background
            execute!(self.stdout, Clear(ClearType::All))?;
            write!(self.stdout, "\x1b[0m")?; // Reset after clear
            #[cfg(target_os = "windows")]
            {
                self.needs_full_redraw = true;
            }
        }
        
        // Get viewport dimensions for rendering
        let content_height = height.saturating_sub(1 + bottom_window_height as u16) as usize; // Reserve for status and bottom window
        // Note: viewport is only updated when cursor moves, not on every render
        
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
                    line_content.push_str("\x1b[48;2;22;22;22m"); // Consistent background
                    line_content.push('~');
                    for _ in 1..width {
                        line_content.push(' ');
                    }
                    line_content.push_str("\x1b[0m");
                } else {
                    // If horizontally scrolled, show all spaces
                    line_content.push_str("\x1b[48;2;22;22;22m"); // Consistent background
                    for _ in 0..width {
                        line_content.push(' ');
                    }
                    line_content.push_str("\x1b[0m");
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
                    // Start with the background color for the entire line
                    formatted_line.push_str("\x1b[48;2;22;22;22m"); // Set line background
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
                                        // Use same cadet blue as cursor (#5F9EA0) with black text
                                        formatted_line.push_str("\x1b[48;2;95;158;160m\x1b[38;2;0;0;0m"); // RGB background + black foreground
                                    }
                                    formatted_line.push(ch);
                                    if is_selected {
                                        formatted_line.push_str("\x1b[0m\x1b[48;2;22;22;22m"); // Reset and restore line background
                                    }
                                }
                                
                                #[cfg(not(target_os = "windows"))]
                                {
                                    if is_selected {
                                        // Use same cadet blue as cursor (#5F9EA0) with black text
                                        formatted_line.push_str("\x1b[48;2;95;158;160m\x1b[38;2;0;0;0m"); // RGB background + black foreground
                                    }
                                    formatted_line.push(ch);
                                    if is_selected {
                                        formatted_line.push_str("\x1b[0m\x1b[48;2;22;22;22m"); // Reset and restore line background
                                    }
                                }
                                
                                screen_col += char_width;
                            }
                        }
                        
                        byte_pos += ch.len_utf8();
                        display_col += char_width;
                    }
                    
                    // Pad the rest of the line with spaces (background already set)
                    while screen_col < width as usize {
                        formatted_line.push(' ');
                        screen_col += 1;
                    }
                    formatted_line.push_str("\x1b[0m"); // Reset at end of line
                    
                    line_content = formatted_line;
                } else {
                    // Virtual line after the buffer - respect horizontal scrolling
                    if viewport_offset.1 == 0 {
                        // Only show the ~ if we're not horizontally scrolled
                        line_content.push_str("\x1b[48;2;22;22;22m"); // Consistent background
                        line_content.push('~');
                        for _ in 1..width {
                            line_content.push(' ');
                        }
                        line_content.push_str("\x1b[0m");
                    } else {
                        // If horizontally scrolled, show all spaces
                        line_content.push_str("\x1b[48;2;22;22;22m"); // Consistent background
                        for _ in 0..width {
                            line_content.push(' ');
                        }
                        line_content.push_str("\x1b[0m");
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
        
        // Build status line - position it above any bottom window
        let status_row = (height - 1 - bottom_window_height as u16) as usize;
        let modified_indicator = if editor.is_modified() { "*" } else { "" };
        let file_name = editor.file_name();
        let (line, col) = editor.cursor_position();
        let total_lines = buffer.len_lines();
        
        let left_status = format!(" {}{} ", file_name, modified_indicator);
        let right_status = format!(" {}/{}:{} ", line + 1, total_lines, col + 1);
        
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
        // Only show cursor if there's no bottom window (find/replace is closed)
        if bottom_window_height == 0 {
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
        }
        // If find/replace is open, cursor will be positioned by find_replace.draw()
        
        self.stdout.flush()?;
        Ok(())
    }
    
    /// Force a complete redraw by clearing cached state
    pub fn force_redraw(&mut self) {
        self.last_screen = vec![String::new(); self.last_size.1 as usize];
        self.last_status.clear();
        self.last_title.clear();
        // FIX 3: Don't reset cursor style here, let draw_with_bottom_window handle it properly
        #[cfg(target_os = "windows")]
        {
            self.needs_full_redraw = true;
        }
    }
}