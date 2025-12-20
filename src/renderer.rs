use crate::editor::Editor;
use crate::syntax::{HighlightSpan, SyntaxState};
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
        write!(stdout, "\x1b[48;2;30;30;30m")?; // Background color RGB(30,30,30)
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
        crate::debug_log("draw_with_bottom_window: start");
        // Update cursor style based on selection
        // Note: Output pane handles its own cursor style when focused
        let desired_style = if editor.selection().is_some() {
            CursorStyle::Underline
        } else {
            CursorStyle::Block
        };

        // Always write the cursor style to ensure it's correct
        if self.last_cursor_style != desired_style {
            match desired_style {
                CursorStyle::Block => write!(self.stdout, "\x1b[2 q")?,
                CursorStyle::Underline => write!(self.stdout, "\x1b[4 q")?,
            }
            self.last_cursor_style = desired_style;
        }

        crate::debug_log("draw_with_bottom_window: getting file_name");
        // Update terminal title with filename and modified indicator
        let file_name = editor.file_name();
        crate::debug_log("draw_with_bottom_window: getting is_modified");
        let modified_indicator = if editor.is_modified() { " *" } else { "" };

        crate::debug_log("draw_with_bottom_window: formatting title");
        let title = if file_name == "[No Name]" {
            format!("No Name{}", modified_indicator)
        } else {
            format!("{}{}", file_name, modified_indicator)
        };

        if title != self.last_title {
            execute!(self.stdout, SetTitle(&title))?;
            self.last_title = title;
        }

        crate::debug_log("draw_with_bottom_window: getting terminal size");
        let (width, height) = terminal::size()?;

        // Handle resize
        if (width, height) != self.last_size {
            crate::debug_log("draw_with_bottom_window: handling resize");
            self.last_size = (width, height);
            self.last_screen = vec![String::new(); height as usize];
            self.last_status.clear();
            self.last_cursor_style = CursorStyle::Block; // Force cursor style refresh on resize
            // Maintain consistent background on resize
            write!(self.stdout, "\x1b[48;2;30;30;30m")?; // Background color RGB(30,30,30)
            execute!(self.stdout, Clear(ClearType::All))?;
            write!(self.stdout, "\x1b[0m")?; // Reset after clear
            #[cfg(target_os = "windows")]
            {
                self.needs_full_redraw = true;
            }
        }

        crate::debug_log("draw_with_bottom_window: calculating content_height");
        // Get viewport dimensions for rendering
        let content_height = height.saturating_sub(1 + bottom_window_height as u16) as usize; // Reserve for status and bottom window
        // Note: viewport is only updated when cursor moves, not on every render

        crate::debug_log("draw_with_bottom_window: about to update_syntax_viewport");
        // Process syntax highlighting first (requires mutable borrow)
        // Update viewport for large files
        let viewport_height = content_height;
        editor.update_syntax_viewport(viewport_height);
        crate::debug_log("draw_with_bottom_window: update_syntax_viewport complete");
        // Only update syntax highlighting if we have work to do
        crate::debug_log(&format!("draw_with_bottom_window: has_syntax_work = {}", editor.has_syntax_work()));
        if editor.has_syntax_work() {
            crate::debug_log("draw_with_bottom_window: about to update_syntax_highlighting");
            editor.update_syntax_highlighting();
            crate::debug_log("draw_with_bottom_window: update_syntax_highlighting complete");
        }

        crate::debug_log("draw_with_bottom_window: getting viewport_offset");
        // Now get all the data we need with immutable borrows
        let viewport_offset = editor.viewport_offset();
        crate::debug_log("draw_with_bottom_window: getting selection");
        let selection = editor.selection();
        crate::debug_log("draw_with_bottom_window: getting buffer");
        let buffer = editor.buffer();
        crate::debug_log("draw_with_bottom_window: getting matching_brackets");
        let matching_brackets = editor.get_matching_brackets();
        crate::debug_log("draw_with_bottom_window: getting matching_text_positions");
        let matching_text_positions = editor.get_matching_text_positions();
        crate::debug_log("draw_with_bottom_window: getting find_matches");
        let find_matches = editor.get_find_matches();
        crate::debug_log("draw_with_bottom_window: getting current_find_match");
        let current_find_match = editor.get_current_find_match();

        crate::debug_log("draw_with_bottom_window: hiding cursor");
        // Hide cursor while drawing
        #[cfg(target_os = "windows")]
        write!(self.stdout, "\x1b[?25l")?;

        #[cfg(not(target_os = "windows"))]
        execute!(self.stdout, Hide)?;

        crate::debug_log(&format!("draw_with_bottom_window: starting line drawing loop, content_height = {}", content_height));
        // Draw all lines
        for screen_row in 0..content_height {
            if screen_row == 0 || screen_row == content_height - 1 || screen_row % 10 == 0 {
                crate::debug_log(&format!("draw_with_bottom_window: drawing screen_row {}", screen_row));
            }
            let mut line_content = String::with_capacity(width as usize);
            
            // Calculate which logical line we're displaying
            // Logical lines: 0 and 1 are virtual, 2+ map to buffer lines 0+
            let logical_line = viewport_offset.0 + screen_row;
            
            if logical_line < 2 {
                // Virtual lines before the buffer - respect horizontal scrolling
                if viewport_offset.1 == 0 {
                    // Only show the ~ if we're not horizontally scrolled
                    line_content.push_str("\x1b[48;2;30;30;30m"); // Consistent background
                    line_content.push_str("\x1b[38;2;110;110;110m"); // Dim grey like comments
                    line_content.push('~');
                    line_content.push_str("\x1b[39m"); // Reset foreground color
                    for _ in 1..width {
                        line_content.push(' ');
                    }
                    line_content.push_str("\x1b[0m");
                } else {
                    // If horizontally scrolled, show all spaces
                    line_content.push_str("\x1b[48;2;30;30;30m"); // Consistent background
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
                    
                    // Get syntax highlighting for this line
                    let syntax_spans = editor.get_syntax_spans(file_row);
                    
                    // Build line with selection and syntax highlighting
                    let mut formatted_line = String::new();
                    // Start with the background color for the entire line
                    // Check if this is the current line
                    let is_current_line = file_row == editor.cursor_position().0;
                    let line_bg_color = if is_current_line {
                        "\x1b[48;2;40;40;40m" // Current line background RGB(40,40,40)
                    } else {
                        "\x1b[48;2;30;30;30m" // Normal background RGB(30,30,30)
                    };
                    formatted_line.push_str(line_bg_color); // Set line background
                    let mut byte_pos = line_byte_start;
                    let mut display_col = 0;  // Display column position (accounts for wide chars)
                    let mut screen_col = 0;    // Screen column position after horizontal scroll
                    let mut line_byte_offset = 0;  // Byte offset within the line
                    
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

                                // Check if this character is a matching bracket
                                let is_matching_bracket = matching_brackets.map_or(false, |(pos1, pos2)| {
                                    byte_pos == pos1 || byte_pos == pos2
                                });

                                // Check if this character is part of matching text
                                let is_matching_text = matching_text_positions.iter().any(|(start, end)| {
                                    byte_pos >= *start && byte_pos < *end
                                });

                                // Check if this character is part of a find match
                                let (is_find_match, is_current_find_match) = find_matches.iter().enumerate()
                                    .find(|(_, (start, end))| byte_pos >= *start && byte_pos < *end)
                                    .map(|(idx, _)| (true, current_find_match == Some(idx)))
                                    .unwrap_or((false, false));

                                // Check syntax highlighting for this character
                                let mut syntax_state = SyntaxState::Normal;
                                if let Some(spans) = syntax_spans {
                                    for span in spans {
                                        if line_byte_offset >= span.start && line_byte_offset < span.end {
                                            syntax_state = span.state;
                                            break;
                                        }
                                    }
                                }
                                
                                #[cfg(target_os = "windows")]
                                {
                                    if is_selected {
                                        // Use same cadet blue as cursor (#5F9EA0) with black text
                                        formatted_line.push_str("\x1b[48;2;95;158;160m\x1b[38;2;0;0;0m"); // RGB background + black foreground
                                    } else if is_current_find_match {
                                        // Bright orange background for current find match
                                        formatted_line.push_str("\x1b[48;2;200;150;100m\x1b[38;2;0;0;0m"); // Bright orange + black text
                                    } else if is_find_match {
                                        // Dimmer orange background for other find matches
                                        formatted_line.push_str("\x1b[48;2;120;90;60m"); // Dim orange background
                                    } else if is_matching_bracket {
                                        // Bright yellow for matching brackets
                                        formatted_line.push_str("\x1b[38;2;220;220;120m\x1b[1m"); // Bright yellow + bold
                                    } else if is_matching_text {
                                        // Dim version of selection background for matching text
                                        formatted_line.push_str("\x1b[48;2;50;80;82m"); // Dimmer version of selection color
                                    } else {
                                        // Apply syntax highlighting colors - autumn/earthy for Python, spring/green for SQL
                                        match syntax_state {
                                            SyntaxState::StringDouble | SyntaxState::StringSingle | SyntaxState::StringTriple | SyntaxState::StringTripleSingle => {
                                                // Warm tan-brown for Python strings
                                                formatted_line.push_str("\x1b[38;2;170;145;120m"); // #AA9178
                                            }
                                            SyntaxState::LineComment | SyntaxState::BlockComment => {
                                                // Neutral grey for comments
                                                formatted_line.push_str("\x1b[38;2;120;120;120m"); // #787878
                                            }
                                            SyntaxState::Keyword => {
                                                // Warm brown for Python keywords
                                                formatted_line.push_str("\x1b[38;2;180;140;110m"); // #B48C6E
                                            }
                                            SyntaxState::Type => {
                                                // Rich earth brown for types
                                                formatted_line.push_str("\x1b[38;2;160;125;95m"); // #A07D5F
                                            }
                                            SyntaxState::Function => {
                                                // Muted golden-brown for functions
                                                formatted_line.push_str("\x1b[38;2;190;160;120m"); // #BEA078
                                            }
                                            SyntaxState::Number => {
                                                // Clay brown for numbers
                                                formatted_line.push_str("\x1b[38;2;175;135;105m"); // #AF8769
                                            }
                                            SyntaxState::Operator => {
                                                // Medium tan for operators
                                                formatted_line.push_str("\x1b[38;2;155;140;120m"); // #9B8C78
                                            }
                                            SyntaxState::Punctuation => {
                                                // Light brown for punctuation
                                                formatted_line.push_str("\x1b[38;2;165;150;130m"); // #A59682
                                            }
                                            SyntaxState::MacroOrDecorator => {
                                                // Dusty brown-red for decorators
                                                formatted_line.push_str("\x1b[38;2;170;120;105m"); // #AA7869
                                            }
                                            SyntaxState::SqlKeyword => {
                                                // Light sage green for SQL keywords
                                                formatted_line.push_str("\x1b[38;2;140;180;150m"); // #8CB496
                                            }
                                            SyntaxState::SqlFunction => {
                                                // Fresh spring green for SQL functions
                                                formatted_line.push_str("\x1b[38;2;130;200;140m"); // #82C88C
                                            }
                                            SyntaxState::SqlNumber => {
                                                // Mint green for SQL numbers
                                                formatted_line.push_str("\x1b[38;2;150;210;170m"); // #96D2AA
                                            }
                                            SyntaxState::SqlText => {
                                                // Pale spring green for SQL text/identifiers
                                                formatted_line.push_str("\x1b[38;2;160;190;170m"); // #A0BEAA
                                            }
                                            SyntaxState::Normal => {
                                                // Light tan for normal text
                                                formatted_line.push_str("\x1b[38;2;200;185;165m"); // #C8B9A5
                                            }
                                        }
                                    }
                                    formatted_line.push(ch);
                                    if is_selected || is_current_find_match || is_find_match {
                                        formatted_line.push_str("\x1b[0m");
                                        formatted_line.push_str(line_bg_color); // Reset and restore line background
                                    } else if is_matching_bracket {
                                        // Reset bold and color
                                        formatted_line.push_str("\x1b[0m");
                                        formatted_line.push_str(line_bg_color); // Restore line background
                                    } else if is_matching_text {
                                        // Reset background
                                        formatted_line.push_str("\x1b[49m");
                                        formatted_line.push_str(line_bg_color); // Restore line background
                                    } else if syntax_state != SyntaxState::Normal && syntax_state != SyntaxState::Punctuation && syntax_state != SyntaxState::SqlText {
                                        // Reset color after syntax-highlighted character
                                        formatted_line.push_str("\x1b[39m"); // Reset foreground only
                                    }
                                }
                                
                                #[cfg(not(target_os = "windows"))]
                                {
                                    if is_selected {
                                        // Use same cadet blue as cursor (#5F9EA0) with black text
                                        formatted_line.push_str("\x1b[48;2;95;158;160m\x1b[38;2;0;0;0m"); // RGB background + black foreground
                                    } else if is_current_find_match {
                                        // Bright orange background for current find match
                                        formatted_line.push_str("\x1b[48;2;200;150;100m\x1b[38;2;0;0;0m"); // Bright orange + black text
                                    } else if is_find_match {
                                        // Dimmer orange background for other find matches
                                        formatted_line.push_str("\x1b[48;2;120;90;60m"); // Dim orange background
                                    } else if is_matching_bracket {
                                        // Bright yellow for matching brackets
                                        formatted_line.push_str("\x1b[38;2;220;220;120m\x1b[1m"); // Bright yellow + bold
                                    } else if is_matching_text {
                                        // Dim version of selection background for matching text
                                        formatted_line.push_str("\x1b[48;2;50;80;82m"); // Dimmer version of selection color
                                    } else {
                                        // Apply syntax highlighting colors - autumn/earthy for Python, spring/green for SQL
                                        match syntax_state {
                                            SyntaxState::StringDouble | SyntaxState::StringSingle | SyntaxState::StringTriple | SyntaxState::StringTripleSingle => {
                                                // Warm tan-brown for Python strings
                                                formatted_line.push_str("\x1b[38;2;170;145;120m"); // #AA9178
                                            }
                                            SyntaxState::LineComment | SyntaxState::BlockComment => {
                                                // Neutral grey for comments
                                                formatted_line.push_str("\x1b[38;2;120;120;120m"); // #787878
                                            }
                                            SyntaxState::Keyword => {
                                                // Warm brown for Python keywords
                                                formatted_line.push_str("\x1b[38;2;180;140;110m"); // #B48C6E
                                            }
                                            SyntaxState::Type => {
                                                // Rich earth brown for types
                                                formatted_line.push_str("\x1b[38;2;160;125;95m"); // #A07D5F
                                            }
                                            SyntaxState::Function => {
                                                // Muted golden-brown for functions
                                                formatted_line.push_str("\x1b[38;2;190;160;120m"); // #BEA078
                                            }
                                            SyntaxState::Number => {
                                                // Clay brown for numbers
                                                formatted_line.push_str("\x1b[38;2;175;135;105m"); // #AF8769
                                            }
                                            SyntaxState::Operator => {
                                                // Medium tan for operators
                                                formatted_line.push_str("\x1b[38;2;155;140;120m"); // #9B8C78
                                            }
                                            SyntaxState::Punctuation => {
                                                // Light brown for punctuation
                                                formatted_line.push_str("\x1b[38;2;165;150;130m"); // #A59682
                                            }
                                            SyntaxState::MacroOrDecorator => {
                                                // Dusty brown-red for decorators
                                                formatted_line.push_str("\x1b[38;2;170;120;105m"); // #AA7869
                                            }
                                            SyntaxState::SqlKeyword => {
                                                // Light sage green for SQL keywords
                                                formatted_line.push_str("\x1b[38;2;140;180;150m"); // #8CB496
                                            }
                                            SyntaxState::SqlFunction => {
                                                // Fresh spring green for SQL functions
                                                formatted_line.push_str("\x1b[38;2;130;200;140m"); // #82C88C
                                            }
                                            SyntaxState::SqlNumber => {
                                                // Mint green for SQL numbers
                                                formatted_line.push_str("\x1b[38;2;150;210;170m"); // #96D2AA
                                            }
                                            SyntaxState::SqlText => {
                                                // Pale spring green for SQL text/identifiers
                                                formatted_line.push_str("\x1b[38;2;160;190;170m"); // #A0BEAA
                                            }
                                            SyntaxState::Normal => {
                                                // Light tan for normal text
                                                formatted_line.push_str("\x1b[38;2;200;185;165m"); // #C8B9A5
                                            }
                                        }
                                    }
                                    formatted_line.push(ch);
                                    if is_selected || is_current_find_match || is_find_match {
                                        formatted_line.push_str("\x1b[0m");
                                        formatted_line.push_str(line_bg_color); // Reset and restore line background
                                    } else if is_matching_bracket {
                                        // Reset bold and color
                                        formatted_line.push_str("\x1b[0m");
                                        formatted_line.push_str(line_bg_color); // Restore line background
                                    } else if is_matching_text {
                                        // Reset background
                                        formatted_line.push_str("\x1b[49m");
                                        formatted_line.push_str(line_bg_color); // Restore line background
                                    } else if syntax_state != SyntaxState::Normal && syntax_state != SyntaxState::Punctuation && syntax_state != SyntaxState::SqlText {
                                        // Reset color after syntax-highlighted character
                                        formatted_line.push_str("\x1b[39m"); // Reset foreground only
                                    }
                                }
                                
                                screen_col += char_width;
                            }
                        }
                        
                        byte_pos += ch.len_utf8();
                        line_byte_offset += ch.len_utf8();
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
                        line_content.push_str("\x1b[48;2;30;30;30m"); // Consistent background
                        line_content.push_str("\x1b[38;2;110;110;110m"); // Dim grey like comments
                        line_content.push('~');
                        line_content.push_str("\x1b[39m"); // Reset foreground color
                        for _ in 1..width {
                            line_content.push(' ');
                        }
                        line_content.push_str("\x1b[0m");
                    } else {
                        // If horizontally scrolled, show all spaces
                        line_content.push_str("\x1b[48;2;30;30;30m"); // Consistent background
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

        crate::debug_log("draw_with_bottom_window: drawing loop completed, building status line");
        // Build status line - position it above any bottom window
        let status_row = (height - 1 - bottom_window_height as u16) as usize;
        crate::debug_log("draw_with_bottom_window: calling is_modified");
        let modified_indicator = if editor.is_modified() { "*" } else { "" };
        crate::debug_log("draw_with_bottom_window: calling is_read_only");
        let read_only_indicator = if editor.is_read_only() { " [RO]" } else { "" };
        crate::debug_log("draw_with_bottom_window: calling file_name");
        let file_name = editor.file_name();
        crate::debug_log("draw_with_bottom_window: calling cursor_position");
        let (line, col) = editor.cursor_position();
        crate::debug_log("draw_with_bottom_window: calling buffer.len_lines");
        let total_lines = buffer.len_lines();
        
        // Check for status messages (errors)
        let (status_msg, is_error) = if let Some((msg, is_err)) = &editor.status_message {
            (msg.as_str(), *is_err)
        } else {
            ("", false)
        };
        
        crate::debug_log("draw_with_bottom_window: formatting left_status");
        let left_status = if !status_msg.is_empty() {
            // Show error message instead of filename
            format!(" {} ", status_msg)
        } else {
            format!(" {}{}{} ", file_name, modified_indicator, read_only_indicator)
        };

        crate::debug_log("draw_with_bottom_window: calling is_repl_mode");
        // Add kernel info if in REPL mode
        let mut kernel_info = if editor.is_repl_mode() {
            crate::debug_log("draw_with_bottom_window: in REPL mode, calling get_kernel_info");
            if let Some(kernel_name) = editor.get_kernel_info() {
                format!(" [{}] ", kernel_name)
            } else {
                " [No kernel] ".to_string()
            }
        } else {
            String::new()
        };
        crate::debug_log("draw_with_bottom_window: kernel_info formatted");

        crate::debug_log("draw_with_bottom_window: formatting right_status");
        // Format the right status with fixed-width fields
        // Right-align the entire row/total as one unit (19 chars) and column (4 chars)
        // This accommodates up to 999,999,999 lines (9 digits + "/" + 9 digits)
        let row_info = format!("{}/{}", line + 1, total_lines);
        let right_status = format!(" {:>19}  {:>4} ",
            row_info,
            col + 1
        );

        crate::debug_log("draw_with_bottom_window: building full status_line");
        // Calculate available space and truncate kernel_info if needed
        let min_width = left_status.len() + right_status.len();
        let max_kernel_width = if min_width < width as usize {
            (width as usize).saturating_sub(min_width)
        } else {
            0
        };

        // Truncate kernel_info if it's too long
        if kernel_info.len() > max_kernel_width {
            if max_kernel_width > 4 {
                // Truncate and add "..."
                let truncate_to = max_kernel_width.saturating_sub(3);
                kernel_info = kernel_info.chars().take(truncate_to).collect::<String>() + "...";
            } else {
                kernel_info.clear();
            }
        }

        let mut status_line = String::with_capacity(width as usize);
        status_line.push_str(&left_status);
        status_line.push_str(&kernel_info);
        // Calculate padding - ensure we never exceed width
        let used_width = left_status.chars().count() + kernel_info.chars().count() + right_status.chars().count();
        let padding = if used_width < width as usize {
            width as usize - used_width
        } else {
            0
        };
        for _ in 0..padding {
            status_line.push(' ');
        }
        status_line.push_str(&right_status);

        // Final safety check: ensure status line doesn't exceed width
        let status_chars: Vec<char> = status_line.chars().collect();
        if status_chars.len() > width as usize {
            status_line = status_chars.iter().take(width as usize).collect();
        }

        crate::debug_log("draw_with_bottom_window: about to write status line to stdout");
        // Only update status if it changed
        #[cfg(target_os = "windows")]
        {
            if self.needs_full_redraw || status_line != self.last_status {
                if is_error {
                    // Red background for errors
                    write!(self.stdout,
                        "\x1b[{};1H\x1b[48;5;196m\x1b[38;5;15m{}\x1b[0m",
                        height, status_line)?;
                } else {
                    // Normal dark grey background
                    write!(self.stdout,
                        "\x1b[{};1H\x1b[48;5;238m\x1b[38;5;15m{}\x1b[0m",
                        height, status_line)?;
                }
                self.last_status = status_line;
            }
            self.needs_full_redraw = false;
        }

        #[cfg(not(target_os = "windows"))]
        {
            if status_line != self.last_status {
                crate::debug_log("draw_with_bottom_window: status line changed, executing crossterm commands");
                if is_error {
                    // Red background for errors
                    execute!(
                        self.stdout,
                        MoveTo(0, status_row as u16),
                        crossterm::style::SetBackgroundColor(crossterm::style::Color::Red),
                        crossterm::style::SetForegroundColor(crossterm::style::Color::White),
                        crossterm::style::Print(&status_line),
                        crossterm::style::ResetColor
                    )?;
                } else {
                    // Normal dark grey background
                    execute!(
                        self.stdout,
                        MoveTo(0, status_row as u16),
                        crossterm::style::SetBackgroundColor(crossterm::style::Color::DarkGrey),
                        crossterm::style::SetForegroundColor(crossterm::style::Color::White),
                        crossterm::style::Print(&status_line),
                        crossterm::style::ResetColor
                    )?;
                }
                crate::debug_log("draw_with_bottom_window: status line written");
                self.last_status = status_line;
            } else {
                crate::debug_log("draw_with_bottom_window: status line unchanged, skipping write");
            }
        }
        crate::debug_log("draw_with_bottom_window: status line complete");
        
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
            } else {
                // Cursor is outside viewport - hide it
                #[cfg(target_os = "windows")]
                write!(self.stdout, "\x1b[?25l")?;

                #[cfg(not(target_os = "windows"))]
                execute!(self.stdout, Hide)?;
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

    /// Reposition and show cursor at editor position (call after drawing output pane)
    pub fn reposition_cursor(&mut self, editor: &Editor, bottom_window_height: usize) -> io::Result<()> {
        let (width, height) = terminal::size()?;
        let (cursor_line, cursor_col) = editor.cursor_position();
        let (viewport_row, viewport_col) = editor.viewport_offset();

        // Update cursor style based on editor selection
        let desired_style = if editor.selection().is_some() {
            CursorStyle::Underline
        } else {
            CursorStyle::Block
        };

        if self.last_cursor_style != desired_style {
            match desired_style {
                CursorStyle::Block => write!(self.stdout, "\x1b[2 q")?,
                CursorStyle::Underline => write!(self.stdout, "\x1b[4 q")?,
            }
            self.last_cursor_style = desired_style;
        }

        // Calculate content height (excluding status bar and bottom window)
        let content_height = height.saturating_sub(1 + bottom_window_height as u16) as usize;

        // Calculate logical cursor position (add 2 for virtual lines before buffer)
        let logical_cursor_line = cursor_line + 2;

        // Check if cursor is within viewport bounds BEFORE calculating screen position
        // This prevents saturating_sub from hiding out-of-bounds positions as 0
        if logical_cursor_line >= viewport_row &&
           logical_cursor_line < viewport_row + content_height &&
           cursor_col >= viewport_col &&
           cursor_col < viewport_col + width as usize {

            let screen_row = logical_cursor_line - viewport_row;
            let screen_col = cursor_col - viewport_col;

            #[cfg(target_os = "windows")]
            write!(self.stdout, "\x1b[{};{}H\x1b[?25h",
                screen_row + 1, screen_col + 1)?;

            #[cfg(not(target_os = "windows"))]
            execute!(
                self.stdout,
                MoveTo(screen_col as u16, screen_row as u16),
                Show
            )?;
        } else {
            // Cursor is outside viewport - hide it
            #[cfg(target_os = "windows")]
            write!(self.stdout, "\x1b[?25l")?;

            #[cfg(not(target_os = "windows"))]
            execute!(self.stdout, Hide)?;
        }

        self.stdout.flush()?;
        Ok(())
    }
}
