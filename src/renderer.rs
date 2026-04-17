use crate::editor::Editor;
use crate::syntax::SyntaxState;
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{poll, read, Event},
    execute,
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, SetTitle},
};
use std::io::{self, Write};
use std::time::Duration;
use unicode_width::UnicodeWidthChar;

/// Query the terminal for the RGB value of an ANSI color using OSC 4
/// Returns None if the terminal doesn't respond or doesn't support the query
fn query_terminal_color(ansi_code: u8) -> Option<(u8, u8, u8)> {
    // Convert ANSI code to palette index
    let palette_index = match ansi_code {
        30..=37 => ansi_code - 30,      // Standard colors 0-7
        90..=97 => ansi_code - 90 + 8,  // Bright colors 8-15
        _ => return None,
    };

    // Send OSC 4 query: \x1b]4;{index};?\x1b\\
    let query = format!("\x1b]4;{};?\x1b\\", palette_index);
    print!("{}", query);
    std::io::stdout().flush().ok()?;

    // Read response with timeout
    let mut response = String::new();
    let start = std::time::Instant::now();

    while start.elapsed() < Duration::from_millis(100) {
        if poll(Duration::from_millis(10)).ok()? {
            if let Event::Key(key_event) = read().ok()? {
                // Collect characters from the response
                if let crossterm::event::KeyCode::Char(c) = key_event.code {
                    response.push(c);
                }
            }
        }

        // Check if we have a complete response
        // Format: \x1b]4;{index};rgb:{rrrr}/{gggg}/{bbbb}\x1b\\
        if response.contains("rgb:") && (response.ends_with('\x07') || response.ends_with('\\')) {
            break;
        }
    }

    // Parse the response: rgb:rrrr/gggg/bbbb (16-bit hex values)
    if let Some(rgb_start) = response.find("rgb:") {
        let rgb_part = &response[rgb_start + 4..];
        let parts: Vec<&str> = rgb_part.split('/').collect();
        if parts.len() >= 3 {
            // Parse 16-bit hex values and convert to 8-bit
            let r = u16::from_str_radix(parts[0].trim_end_matches(|c: char| !c.is_ascii_hexdigit()), 16).ok()? / 256;
            let g = u16::from_str_radix(parts[1].trim_end_matches(|c: char| !c.is_ascii_hexdigit()), 16).ok()? / 256;
            let b = u16::from_str_radix(parts[2].trim_end_matches(|c: char| !c.is_ascii_hexdigit()), 16).ok()? / 256;
            return Some((r as u8, g as u8, b as u8));
        }
    }

    None
}

/// Cache for terminal colors to avoid repeated queries
use std::sync::OnceLock;
static TERMINAL_COLORS: OnceLock<std::collections::HashMap<u8, (u8, u8, u8)>> = OnceLock::new();

/// Cached blended SQL colors (computed once at startup)
struct SqlBlendedColors {
    keyword: String,
    function: String,
    number: String,
    text: String,
    comment: String,
    string: String,
}

static SQL_BLENDED_COLORS: OnceLock<SqlBlendedColors> = OnceLock::new();

fn get_sql_colors() -> &'static SqlBlendedColors {
    SQL_BLENDED_COLORS.get_or_init(|| {
        let string_rgb = ansi_to_rgb(parse_ansi_code(syntax_colors::STRING));
        SqlBlendedColors {
            keyword: {
                let syntax_rgb = ansi_to_rgb(parse_ansi_code(syntax_colors::KEYWORD));
                let blended = blend_colors(syntax_rgb, string_rgb, 0.35);
                format!("\x1b[38;2;{};{};{}m", blended.0, blended.1, blended.2)
            },
            function: {
                let syntax_rgb = ansi_to_rgb(parse_ansi_code(syntax_colors::FUNCTION));
                let blended = blend_colors(syntax_rgb, string_rgb, 0.35);
                format!("\x1b[38;2;{};{};{}m", blended.0, blended.1, blended.2)
            },
            number: {
                let syntax_rgb = ansi_to_rgb(parse_ansi_code(syntax_colors::NUMBER));
                let blended = blend_colors(syntax_rgb, string_rgb, 0.35);
                format!("\x1b[38;2;{};{};{}m", blended.0, blended.1, blended.2)
            },
            text: {
                let syntax_rgb = ansi_to_rgb(parse_ansi_code(syntax_colors::NORMAL));
                let blended = blend_colors(syntax_rgb, string_rgb, 0.35);
                format!("\x1b[38;2;{};{};{}m", blended.0, blended.1, blended.2)
            },
            comment: {
                let syntax_rgb = ansi_to_rgb(parse_ansi_code(syntax_colors::COMMENT));
                let blended = blend_colors(syntax_rgb, string_rgb, 0.35);
                format!("\x1b[38;2;{};{};{}m", blended.0, blended.1, blended.2)
            },
            string: {
                let syntax_rgb = ansi_to_rgb(parse_ansi_code(syntax_colors::STRING));
                let blended = blend_colors(syntax_rgb, string_rgb, 0.35);
                format!("\x1b[38;2;{};{};{}m", blended.0, blended.1, blended.2)
            },
        }
    })
}

/// Get RGB value for an ANSI color, querying terminal if not cached
fn ansi_to_rgb(ansi_code: u8) -> (u8, u8, u8) {
    let colors = TERMINAL_COLORS.get_or_init(|| {
        let mut map = std::collections::HashMap::new();
        // Query all 16 colors at startup
        for code in [30, 31, 32, 33, 34, 35, 36, 37, 90, 91, 92, 93, 94, 95, 96, 97] {
            if let Some(rgb) = query_terminal_color(code) {
                map.insert(code, rgb);
            }
        }
        map
    });

    // Return cached color or fallback to a neutral gray
    *colors.get(&ansi_code).unwrap_or(&(128, 128, 128))
}

/// Blend two RGB colors with a given ratio (0.0 = all color1, 1.0 = all color2)
fn blend_colors(color1: (u8, u8, u8), color2: (u8, u8, u8), ratio: f32) -> (u8, u8, u8) {
    let r = (color1.0 as f32 * (1.0 - ratio) + color2.0 as f32 * ratio) as u8;
    let g = (color1.1 as f32 * (1.0 - ratio) + color2.1 as f32 * ratio) as u8;
    let b = (color1.2 as f32 * (1.0 - ratio) + color2.2 as f32 * ratio) as u8;
    (r, g, b)
}

/// Syntax color escape sequences (single source of truth)
mod syntax_colors {
    pub const STRING: &str = "\x1b[33m";       // Yellow
    pub const COMMENT: &str = "\x1b[90m";      // Dark gray
    pub const KEYWORD: &str = "\x1b[95m";      // Bright magenta
    pub const TYPE: &str = "\x1b[36m";         // Cyan
    pub const FUNCTION: &str = "\x1b[94m";     // Bright blue
    pub const NUMBER: &str = "\x1b[93m";       // Bright yellow
    pub const OPERATOR: &str = "\x1b[37m";     // White
    pub const NORMAL: &str = "\x1b[37m";       // White
}

/// Extract ANSI color code from an escape sequence like "\x1b[95m"
fn parse_ansi_code(escape_seq: &str) -> u8 {
    escape_seq
        .trim_start_matches("\x1b[")
        .trim_end_matches('m')
        .parse()
        .unwrap_or(37)
}

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
        write!(stdout, "\x1b[48;5;234m")?; // Background color RGB(30,30,30)
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
        self.draw_with_bottom_window(editor, 0, false)
    }

    pub fn draw_with_bottom_window(&mut self, editor: &mut Editor, bottom_window_height: usize, bottom_window_focused: bool) -> io::Result<()> {
        if editor.is_spreadsheet_mode() {
            return self.draw_spreadsheet(editor);
        }
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
            write!(self.stdout, "\x1b[48;5;234m")?; // Background color RGB(30,30,30)
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

        // Process syntax highlighting first (requires mutable borrow)
        // Update viewport for large files
        let viewport_height = content_height;
        editor.update_syntax_viewport(viewport_height);
        // Only update syntax highlighting if we have work to do
        if editor.has_syntax_work() {
            editor.update_syntax_highlighting();
        }

        // Now get all the data we need with immutable borrows
        let viewport_offset = editor.viewport_offset();
        let selection = editor.selection();
        let buffer = editor.buffer();
        let matching_brackets = editor.get_matching_brackets();
        let matching_text_positions = editor.get_matching_text_positions();
        let find_matches = editor.get_find_matches();
        let current_find_match = editor.get_current_find_match();

        // Hide cursor while drawing (but not if bottom window is focused - it handles its own cursor)
        // Only hide cursor for full redraws to avoid blinking on differential updates (e.g., mouse clicks)
        #[cfg(target_os = "windows")]
        let should_hide_cursor = !bottom_window_focused && self.needs_full_redraw;
        #[cfg(not(target_os = "windows"))]
        let should_hide_cursor = !bottom_window_focused;

        if should_hide_cursor {
            #[cfg(target_os = "windows")]
            write!(self.stdout, "\x1b[?25l")?;

            #[cfg(not(target_os = "windows"))]
            execute!(self.stdout, Hide)?;
        }

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
                    line_content.push_str("\x1b[48;5;234m"); // Consistent background
                    line_content.push_str("\x1b[90m"); // Dim grey (ANSI dark gray) like comments
                    line_content.push('~');
                    line_content.push_str("\x1b[39m"); // Reset foreground color
                    for _ in 1..width {
                        line_content.push(' ');
                    }
                    line_content.push_str("\x1b[0m");
                } else {
                    // If horizontally scrolled, show all spaces
                    line_content.push_str("\x1b[48;5;234m"); // Consistent background
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
                        "\x1b[48;5;237m" // Current line background RGB(40,40,40)
                    } else {
                        "\x1b[48;5;234m" // Normal background RGB(30,30,30)
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
                                        // Selection: use ANSI 256 color for teal background with black text
                                        formatted_line.push_str("\x1b[48;5;30m\x1b[30m"); // Teal background + black foreground
                                    } else if is_current_find_match {
                                        // Bright orange background for current find match
                                        formatted_line.push_str("\x1b[48;5;172m\x1b[30m"); // Orange + black text
                                    } else if is_find_match {
                                        // Dimmer orange background for other find matches
                                        formatted_line.push_str("\x1b[48;5;94m"); // Dim orange background
                                    } else if is_matching_bracket {
                                        // Bright yellow for matching brackets
                                        formatted_line.push_str("\x1b[93m\x1b[1m"); // Bright yellow + bold
                                    } else if is_matching_text {
                                        // Dim version of selection background for matching text
                                        formatted_line.push_str("\x1b[48;5;23m"); // Dimmer teal
                                    } else {
                                        // Apply syntax highlighting colors - ANSI 16/256 colors
                                        match syntax_state {
                                            SyntaxState::StringDouble | SyntaxState::StringSingle | SyntaxState::StringTriple | SyntaxState::StringTripleSingle => {
                                                formatted_line.push_str(syntax_colors::STRING);
                                            }
                                            SyntaxState::LineComment | SyntaxState::BlockComment => {
                                                formatted_line.push_str(syntax_colors::COMMENT);
                                            }
                                            SyntaxState::Keyword => {
                                                formatted_line.push_str(syntax_colors::KEYWORD);
                                            }
                                            SyntaxState::Type => {
                                                formatted_line.push_str(syntax_colors::TYPE);
                                            }
                                            SyntaxState::Function => {
                                                formatted_line.push_str(syntax_colors::FUNCTION);
                                            }
                                            SyntaxState::Number => {
                                                formatted_line.push_str(syntax_colors::NUMBER);
                                            }
                                            SyntaxState::Operator => {
                                                formatted_line.push_str(syntax_colors::OPERATOR);
                                            }
                                            SyntaxState::Punctuation => {
                                                formatted_line.push_str(syntax_colors::COMMENT);
                                            }
                                            SyntaxState::SqlKeyword => {
                                                let sql_colors = get_sql_colors();
                                                formatted_line.push_str(&sql_colors.keyword);
                                            }
                                            SyntaxState::SqlFunction => {
                                                let sql_colors = get_sql_colors();
                                                formatted_line.push_str(&sql_colors.function);
                                            }
                                            SyntaxState::SqlNumber => {
                                                let sql_colors = get_sql_colors();
                                                formatted_line.push_str(&sql_colors.number);
                                            }
                                            SyntaxState::SqlText => {
                                                let sql_colors = get_sql_colors();
                                                formatted_line.push_str(&sql_colors.text);
                                            }
                                            SyntaxState::SqlComment => {
                                                let sql_colors = get_sql_colors();
                                                formatted_line.push_str(&sql_colors.comment);
                                            }
                                            SyntaxState::SqlString => {
                                                let sql_colors = get_sql_colors();
                                                formatted_line.push_str(&sql_colors.string);
                                            }
                                            SyntaxState::Normal => {
                                                formatted_line.push_str(syntax_colors::NORMAL);
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
                                    } else if syntax_state != SyntaxState::Normal && syntax_state != SyntaxState::Punctuation {
                                        // Reset color after syntax-highlighted character
                                        formatted_line.push_str("\x1b[39m"); // Reset foreground only
                                    }
                                }

                                #[cfg(not(target_os = "windows"))]
                                {
                                    if is_selected {
                                        // Selection: use ANSI 256 color for teal background with black text
                                        formatted_line.push_str("\x1b[48;5;30m\x1b[30m"); // Teal background + black foreground
                                    } else if is_current_find_match {
                                        // Bright orange background for current find match
                                        formatted_line.push_str("\x1b[48;5;172m\x1b[30m"); // Orange + black text
                                    } else if is_find_match {
                                        // Dimmer orange background for other find matches
                                        formatted_line.push_str("\x1b[48;5;94m"); // Dim orange background
                                    } else if is_matching_bracket {
                                        // Bright yellow for matching brackets
                                        formatted_line.push_str("\x1b[93m\x1b[1m"); // Bright yellow + bold
                                    } else if is_matching_text {
                                        // Dim version of selection background for matching text
                                        formatted_line.push_str("\x1b[48;5;23m"); // Dimmer teal
                                    } else {
                                        // Apply syntax highlighting colors - ANSI 16/256 colors
                                        match syntax_state {
                                            SyntaxState::StringDouble | SyntaxState::StringSingle | SyntaxState::StringTriple | SyntaxState::StringTripleSingle => {
                                                formatted_line.push_str(syntax_colors::STRING);
                                            }
                                            SyntaxState::LineComment | SyntaxState::BlockComment => {
                                                formatted_line.push_str(syntax_colors::COMMENT);
                                            }
                                            SyntaxState::Keyword => {
                                                formatted_line.push_str(syntax_colors::KEYWORD);
                                            }
                                            SyntaxState::Type => {
                                                formatted_line.push_str(syntax_colors::TYPE);
                                            }
                                            SyntaxState::Function => {
                                                formatted_line.push_str(syntax_colors::FUNCTION);
                                            }
                                            SyntaxState::Number => {
                                                formatted_line.push_str(syntax_colors::NUMBER);
                                            }
                                            SyntaxState::Operator => {
                                                formatted_line.push_str(syntax_colors::OPERATOR);
                                            }
                                            SyntaxState::Punctuation => {
                                                formatted_line.push_str(syntax_colors::COMMENT);
                                            }
                                            SyntaxState::SqlKeyword => {
                                                let sql_colors = get_sql_colors();
                                                formatted_line.push_str(&sql_colors.keyword);
                                            }
                                            SyntaxState::SqlFunction => {
                                                let sql_colors = get_sql_colors();
                                                formatted_line.push_str(&sql_colors.function);
                                            }
                                            SyntaxState::SqlNumber => {
                                                let sql_colors = get_sql_colors();
                                                formatted_line.push_str(&sql_colors.number);
                                            }
                                            SyntaxState::SqlText => {
                                                let sql_colors = get_sql_colors();
                                                formatted_line.push_str(&sql_colors.text);
                                            }
                                            SyntaxState::SqlComment => {
                                                let sql_colors = get_sql_colors();
                                                formatted_line.push_str(&sql_colors.comment);
                                            }
                                            SyntaxState::SqlString => {
                                                let sql_colors = get_sql_colors();
                                                formatted_line.push_str(&sql_colors.string);
                                            }
                                            SyntaxState::Normal => {
                                                formatted_line.push_str(syntax_colors::NORMAL);
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
                                    } else if syntax_state != SyntaxState::Normal && syntax_state != SyntaxState::Punctuation {
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

                    // If line is empty and selected, show a visual indicator
                    if screen_col == 0 {
                        // Check if this empty line is within the selection
                        if let Some((sel_start, sel_end)) = selection {
                            let line_end_byte = if file_row < buffer.len_lines() - 1 {
                                buffer.line_to_byte(file_row + 1)
                            } else {
                                buffer.len_bytes()
                            };

                            // Line is selected if selection overlaps with this line's byte range
                            if sel_start < line_end_byte && sel_end > line_byte_start {
                                // Draw a vertical bar with selection color to indicate empty line is selected
                                formatted_line.push_str("\x1b[36m"); // Cyan for selection indicator
                                formatted_line.push('│'); // Vertical bar
                                formatted_line.push_str("\x1b[39m"); // Reset foreground
                                screen_col += 1;
                            }
                        }
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
                        line_content.push_str("\x1b[48;5;234m"); // Consistent background
                        line_content.push_str("\x1b[90m"); // Dim grey (ANSI dark gray) like comments
                        line_content.push('~');
                        line_content.push_str("\x1b[39m"); // Reset foreground color
                        for _ in 1..width {
                            line_content.push(' ');
                        }
                        line_content.push_str("\x1b[0m");
                    } else {
                        // If horizontally scrolled, show all spaces
                        line_content.push_str("\x1b[48;5;234m"); // Consistent background
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
        let read_only_indicator = if editor.is_read_only() { " [RO]" } else { "" };
        let file_name = editor.file_name();
        let (line, col) = editor.cursor_position();
        let total_lines = buffer.len_lines();

        // Check for status messages (errors)
        let (status_msg, is_error) = if let Some((msg, is_err)) = &editor.status_message {
            (msg.as_str(), *is_err)
        } else {
            ("", false)
        };

        let left_status = if !status_msg.is_empty() {
            // Show error message instead of filename
            format!(" {} ", status_msg)
        } else {
            format!(" {}{}{} ", file_name, modified_indicator, read_only_indicator)
        };

        // Add language indicator
        let language = editor.get_language();
        let language_name = match language {
            crate::syntax::Language::PlainText => "Plain",
            crate::syntax::Language::Python => "Python",
            crate::syntax::Language::Sql => "SQL",
            crate::syntax::Language::Rust => "Rust",
            crate::syntax::Language::R => "R",
            crate::syntax::Language::Yaml => "YAML",
            crate::syntax::Language::Markdown => "Markdown",
            crate::syntax::Language::Json => "JSON",
            crate::syntax::Language::Shell => "Shell",
            crate::syntax::Language::Toml => "TOML",
            crate::syntax::Language::Csv => "CSV",
            crate::syntax::Language::Tsv => "TSV",
        };
        let language_info = format!(" [{}] ", language_name);

        // Add kernel info if in REPL mode
        let mut kernel_info = if editor.is_repl_mode() {
            if let Some(kernel_name) = editor.get_kernel_info() {
                format!(" [{}] ", kernel_name)
            } else {
                " [No kernel] ".to_string()
            }
        } else {
            String::new()
        };
        // Format the right status with fixed-width fields
        // Right-align the entire row/total as one unit (19 chars) and column (4 chars)
        // This accommodates up to 999,999,999 lines (9 digits + "/" + 9 digits)
        let row_info = format!("{}/{}", line + 1, total_lines);
        let right_status = format!(" {:>19}  {:>4} ",
            row_info,
            col + 1
        );

        // Calculate available space and truncate kernel_info if needed
        let min_width = left_status.len() + language_info.len() + right_status.len();
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
        status_line.push_str(&language_info);
        status_line.push_str(&kernel_info);
        // Calculate padding - ensure we never exceed width
        let used_width = left_status.chars().count() + language_info.chars().count() + kernel_info.chars().count() + right_status.chars().count();
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

        // Only update status if it changed
        #[cfg(target_os = "windows")]
        {
            if self.needs_full_redraw || status_line != self.last_status {
                if is_error {
                    // Red background for errors
                    write!(self.stdout,
                        "\x1b[{};1H\x1b[48;5;196m\x1b[38;5;15m{}\x1b[0m",
                        status_row + 1, status_line)?;
                } else {
                    // Normal dark grey background
                    write!(self.stdout,
                        "\x1b[{};1H\x1b[48;5;238m\x1b[38;5;15m{}\x1b[0m",
                        status_row + 1, status_line)?;
                }
                self.last_status = status_line;
            }
            self.needs_full_redraw = false;
        }

        #[cfg(not(target_os = "windows"))]
        {
            if status_line != self.last_status {
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

    pub fn draw_spreadsheet(&mut self, editor: &mut Editor) -> io::Result<()> {
        use crate::spreadsheet::{col_letter, render_cell_text, FORMULA_BAR_HEIGHT, MIN_COL_WIDTH, ROW_NUM_WIDTH};
        use unicode_width::UnicodeWidthChar;

        // Update title
        let file_name = editor.file_name().to_string();
        let modified = editor.is_modified();
        let title = if file_name == "[No Name]" {
            format!("No Name{}", if modified { " *" } else { "" })
        } else {
            format!("{}{}", file_name, if modified { " *" } else { "" })
        };
        if title != self.last_title {
            execute!(self.stdout, SetTitle(&title))?;
            self.last_title = title;
        }

        let (width, height) = terminal::size()?;
        if (width, height) != self.last_size {
            self.last_size = (width, height);
            self.last_screen = vec![String::new(); height as usize];
            self.last_status.clear();
            write!(self.stdout, "\x1b[48;5;234m")?;
            execute!(self.stdout, Clear(ClearType::All))?;
            write!(self.stdout, "\x1b[0m")?;
            #[cfg(target_os = "windows")]
            {
                self.needs_full_redraw = true;
            }
        }

        // Hide text cursor while painting
        #[cfg(target_os = "windows")]
        write!(self.stdout, "\x1b[?25l")?;
        #[cfg(not(target_os = "windows"))]
        execute!(self.stdout, Hide)?;

        let total_rows = height as usize;
        if total_rows < 6 {
            self.stdout.flush()?;
            return Ok(());
        }
        let status_row = total_rows - 1;
        let formula_bar_rows = FORMULA_BAR_HEIGHT;
        let divider_row = formula_bar_rows;
        let header_row = divider_row + 1;
        let data_start = header_row + 1;
        let data_end_exclusive = status_row;
        let visible_data_rows = data_end_exclusive.saturating_sub(data_start);

        let _ = visible_data_rows; // cursor visibility is managed by the event loop now
        let ss = editor.spreadsheet().expect("spreadsheet mode");
        let editing = ss.is_editing();
        let (cur_row, cur_col) = ss.cursor;
        let ((sel_r0, sel_c0), (sel_r1, sel_c1)) = ss.selected_range();
        let has_multi_selection = ss.has_selection();

        // --- Formula bar ---
        let label = if editing {
            format!(" {} (editing) ", ss.cursor_label())
        } else {
            format!(" {} ", ss.cursor_label())
        };
        let label_width = label.chars().count();

        let detail_source: String = if let Some(edit) = ss.editing.as_ref() {
            edit.text.clone()
        } else {
            ss.focused_cell_text().to_string()
        };

        let edit_selection: Option<(usize, usize)> = ss.editing.as_ref().and_then(|edit| {
            let start = edit.selection_start?;
            if start == edit.cursor {
                None
            } else if start < edit.cursor {
                Some((start, edit.cursor))
            } else {
                Some((edit.cursor, start))
            }
        });

        // Split detail into visual lines and track their byte offsets
        let mut detail_lines: Vec<String> = detail_source.split('\n').map(|s| s.to_string()).collect();
        let mut line_byte_starts: Vec<usize> = vec![0];
        for (i, b) in detail_source.bytes().enumerate() {
            if b == b'\n' {
                line_byte_starts.push(i + 1);
            }
        }
        while detail_lines.len() < formula_bar_rows {
            detail_lines.push(String::new());
        }

        let fb_bg = "\x1b[48;5;236m";
        let fb_fg = "\x1b[38;5;252m";
        let sel_bg = "\x1b[48;5;24m";
        let sel_fg = "\x1b[38;5;230m";

        for fb_row in 0..formula_bar_rows {
            let mut line = String::new();
            line.push_str(fb_bg);
            if fb_row == 0 {
                line.push_str("\x1b[38;5;117m\x1b[1m");
                line.push_str(&label);
                line.push_str("\x1b[0m");
                line.push_str(fb_bg);
                line.push_str(fb_fg);
            } else {
                for _ in 0..label_width {
                    line.push(' ');
                }
                line.push_str(fb_fg);
            }

            let text_width = (width as usize).saturating_sub(label_width);
            let content = detail_lines.get(fb_row).map(|s| s.as_str()).unwrap_or("");
            let line_start_byte = line_byte_starts.get(fb_row).copied().unwrap_or(detail_source.len());

            let mut rendered_width = 0usize;
            let mut byte_in_line = 0usize;
            let mut in_selection = false;
            for ch in content.chars() {
                let cw = ch.width().unwrap_or(1);
                if rendered_width + cw > text_width {
                    break;
                }
                let abs_byte = line_start_byte + byte_in_line;
                let char_selected = match edit_selection {
                    Some((a, b)) => abs_byte >= a && abs_byte < b,
                    None => false,
                };
                if char_selected && !in_selection {
                    line.push_str(sel_bg);
                    line.push_str(sel_fg);
                    in_selection = true;
                } else if !char_selected && in_selection {
                    line.push_str("\x1b[0m");
                    line.push_str(fb_bg);
                    line.push_str(fb_fg);
                    in_selection = false;
                }
                line.push(ch);
                rendered_width += cw;
                byte_in_line += ch.len_utf8();
            }

            // Extend selection highlight to newline marker if selection crosses this line boundary
            if rendered_width < text_width {
                let line_end_byte = line_start_byte + byte_in_line;
                let newline_selected = match edit_selection {
                    Some((a, b)) => line_end_byte >= a && line_end_byte < b,
                    None => false,
                };
                if newline_selected && !in_selection {
                    line.push_str(sel_bg);
                    line.push_str(sel_fg);
                    in_selection = true;
                }
                if newline_selected {
                    line.push(' ');
                    rendered_width += 1;
                }
            }

            if in_selection {
                line.push_str("\x1b[0m");
                line.push_str(fb_bg);
                line.push_str(fb_fg);
            }

            while rendered_width < text_width {
                line.push(' ');
                rendered_width += 1;
            }
            line.push_str("\x1b[0m");

            self.write_spreadsheet_row(fb_row, &line)?;
        }

        // --- Divider ---
        {
            let mut line = String::new();
            line.push_str("\x1b[48;5;234m\x1b[38;5;240m");
            for _ in 0..width {
                line.push('─');
            }
            line.push_str("\x1b[0m");
            self.write_spreadsheet_row(divider_row, &line)?;
        }

        // --- Column header row ---
        {
            let mut line = String::new();
            line.push_str("\x1b[48;5;238m\x1b[38;5;252m\x1b[1m");
            for _ in 0..ROW_NUM_WIDTH {
                line.push(' ');
            }
            line.push_str("\x1b[38;5;240m│\x1b[38;5;252m");

            let mut used: usize = ROW_NUM_WIDTH + 1;
            let num_cols = ss.num_cols();
            let mut col_idx = ss.scroll_col;
            while col_idx < num_cols {
                let col_width = ss
                    .column_widths
                    .get(col_idx)
                    .copied()
                    .unwrap_or(MIN_COL_WIDTH);
                let remaining = (width as usize).saturating_sub(used);
                if remaining == 0 {
                    break;
                }
                let label = col_letter(col_idx);
                let is_focused = col_idx == cur_col;
                if is_focused {
                    line.push_str("\x1b[48;5;24m\x1b[38;5;230m");
                } else {
                    line.push_str("\x1b[48;5;238m\x1b[38;5;252m");
                }
                if remaining >= col_width + 1 {
                    line.push_str(&render_centered(&label, col_width));
                    line.push_str("\x1b[48;5;238m\x1b[38;5;240m│\x1b[38;5;252m");
                    used += col_width + 1;
                    col_idx += 1;
                } else {
                    // Partial column at the right edge: fill remaining space with the cell
                    // content, no trailing separator.
                    line.push_str(&render_centered(&label, remaining));
                    used += remaining;
                    break;
                }
            }
            line.push_str("\x1b[48;5;238m");
            while used < width as usize {
                line.push(' ');
                used += 1;
            }
            line.push_str("\x1b[0m");
            self.write_spreadsheet_row(header_row, &line)?;
        }

        // --- Data rows ---
        for offset in 0..visible_data_rows {
            let row_idx = ss.scroll_row + offset;
            let screen_row = data_start + offset;
            let mut line = String::new();

            if row_idx >= ss.num_rows() {
                line.push_str("\x1b[48;5;234m");
                for _ in 0..width {
                    line.push(' ');
                }
                line.push_str("\x1b[0m");
                self.write_spreadsheet_row(screen_row, &line)?;
                continue;
            }

            let is_current_row = row_idx == cur_row;
            // Row-number column
            if is_current_row {
                line.push_str("\x1b[48;5;24m\x1b[38;5;230m\x1b[1m");
            } else {
                line.push_str("\x1b[48;5;238m\x1b[38;5;250m");
            }
            let row_label = format!("{:>width$} ", row_idx + 1, width = ROW_NUM_WIDTH - 1);
            line.push_str(&row_label);
            line.push_str("\x1b[0m\x1b[48;5;234m\x1b[38;5;240m│\x1b[0m");

            let mut used: usize = ROW_NUM_WIDTH + 1;
            let num_cols = ss.num_cols();
            let mut col_idx = ss.scroll_col;
            while col_idx < num_cols {
                let col_width = ss
                    .column_widths
                    .get(col_idx)
                    .copied()
                    .unwrap_or(MIN_COL_WIDTH);
                let remaining = (width as usize).saturating_sub(used);
                if remaining == 0 {
                    break;
                }
                let is_focused_cell = row_idx == cur_row && col_idx == cur_col;
                let in_selection = has_multi_selection
                    && row_idx >= sel_r0
                    && row_idx <= sel_r1
                    && col_idx >= sel_c0
                    && col_idx <= sel_c1;

                if is_focused_cell {
                    if editing {
                        line.push_str("\x1b[48;5;22m\x1b[38;5;230m");
                    } else {
                        line.push_str("\x1b[48;5;30m\x1b[38;5;230m");
                    }
                } else if in_selection {
                    line.push_str("\x1b[48;5;23m\x1b[38;5;252m");
                } else if is_current_row {
                    line.push_str("\x1b[48;5;237m\x1b[38;5;252m");
                } else {
                    line.push_str("\x1b[48;5;234m\x1b[38;5;252m");
                }

                let text = ss.cell(row_idx, col_idx);
                if remaining >= col_width + 1 {
                    let rendered = render_cell_text(text, col_width);
                    line.push_str(&rendered);
                    line.push_str("\x1b[0m\x1b[48;5;234m\x1b[38;5;240m│\x1b[0m");
                    used += col_width + 1;
                    col_idx += 1;
                } else {
                    // Partial column at the right edge: fill remaining space with truncated
                    // cell content, no trailing separator.
                    let rendered = render_cell_text(text, remaining);
                    line.push_str(&rendered);
                    line.push_str("\x1b[0m");
                    used += remaining;
                    break;
                }
            }

            // Pad remaining width
            if is_current_row {
                line.push_str("\x1b[48;5;237m");
            } else {
                line.push_str("\x1b[48;5;234m");
            }
            while used < width as usize {
                line.push(' ');
                used += 1;
            }
            line.push_str("\x1b[0m");
            self.write_spreadsheet_row(screen_row, &line)?;
        }

        // --- Status bar ---
        let pos_label = ss.cursor_label();
        let num_rows = ss.num_rows();
        let num_cols = ss.num_cols();
        let ro = editor.is_read_only();
        let (status_msg, is_error) = if let Some((msg, is_err)) = &editor.status_message {
            (msg.clone(), *is_err)
        } else {
            (String::new(), false)
        };
        let left_status = if !status_msg.is_empty() {
            format!(" {} ", status_msg)
        } else {
            let mod_ind = if modified { "*" } else { "" };
            let ro_ind = if ro { " [RO]" } else { "" };
            format!(" {}{}{} ", file_name, mod_ind, ro_ind)
        };
        let lang_label = ss.delimiter_name();
        let middle = format!(" [{}] ", lang_label);
        let metrics = ss.selection_metrics().format();
        let metrics_display = if metrics.is_empty() {
            String::new()
        } else {
            format!(" {} ", metrics)
        };
        let right_status = format!(
            " {:>10}  {:>4}×{:<4} ",
            pos_label,
            num_rows,
            num_cols
        );
        let used = left_status.chars().count()
            + middle.chars().count()
            + metrics_display.chars().count()
            + right_status.chars().count();
        let mut status_line = String::new();
        status_line.push_str(&left_status);
        status_line.push_str(&middle);
        status_line.push_str(&metrics_display);
        if used < width as usize {
            for _ in 0..(width as usize - used) {
                status_line.push(' ');
            }
        }
        status_line.push_str(&right_status);
        let status_chars: Vec<char> = status_line.chars().collect();
        if status_chars.len() > width as usize {
            status_line = status_chars.iter().take(width as usize).collect();
        }

        if is_error {
            write!(
                self.stdout,
                "\x1b[{};1H\x1b[48;5;196m\x1b[38;5;15m{}\x1b[0m",
                status_row + 1,
                status_line
            )?;
        } else {
            write!(
                self.stdout,
                "\x1b[{};1H\x1b[48;5;238m\x1b[38;5;15m{}\x1b[0m",
                status_row + 1,
                status_line
            )?;
        }
        self.last_status = status_line;

        // --- Position text cursor in formula bar if editing ---
        if editing {
            if let Some(edit) = ss.editing.as_ref() {
                let (edit_line, edit_col) = cursor_line_col_chars(&edit.text, edit.cursor);
                let fb_row = edit_line.min(formula_bar_rows - 1);
                let text_start_col = label_width;
                let text_width = (width as usize).saturating_sub(text_start_col);
                let visible_col = edit_col.min(text_width.saturating_sub(1));
                let screen_row = fb_row;
                let screen_col = text_start_col + visible_col;
                #[cfg(target_os = "windows")]
                write!(
                    self.stdout,
                    "\x1b[{};{}H\x1b[?25h",
                    screen_row + 1,
                    screen_col + 1
                )?;
                #[cfg(not(target_os = "windows"))]
                execute!(
                    self.stdout,
                    MoveTo(screen_col as u16, screen_row as u16),
                    Show
                )?;
                if self.last_cursor_style != CursorStyle::Underline {
                    write!(self.stdout, "\x1b[6 q")?; // steady bar
                    self.last_cursor_style = CursorStyle::Underline;
                }
            }
        } else {
            // Keep cursor hidden in navigation mode
            #[cfg(target_os = "windows")]
            write!(self.stdout, "\x1b[?25l")?;
            #[cfg(not(target_os = "windows"))]
            execute!(self.stdout, Hide)?;
        }

        self.stdout.flush()?;
        #[cfg(target_os = "windows")]
        {
            self.needs_full_redraw = false;
        }
        Ok(())
    }

    fn write_spreadsheet_row(&mut self, screen_row: usize, line: &str) -> io::Result<()> {
        let should_write = {
            #[cfg(target_os = "windows")]
            {
                self.needs_full_redraw || self.last_screen.get(screen_row).map(|s| s.as_str()) != Some(line)
            }
            #[cfg(not(target_os = "windows"))]
            {
                self.last_screen.get(screen_row).map(|s| s.as_str()) != Some(line)
            }
        };
        if should_write {
            write!(self.stdout, "\x1b[{};1H{}", screen_row + 1, line)?;
            if screen_row < self.last_screen.len() {
                self.last_screen[screen_row] = line.to_string();
            }
        }
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

    /// Update only the status bar without redrawing the rest of the screen
    /// This is used during execution to show elapsed time without causing flicker
    pub fn update_status_bar_only(&mut self, editor: &Editor, bottom_window_height: usize) -> io::Result<()> {
        let (width, height) = terminal::size()?;
        let buffer = editor.buffer();

        // Calculate status row position (same as in draw_with_bottom_window)
        let status_row = (height - 1 - bottom_window_height as u16) as usize;

        let modified_indicator = if editor.is_modified() { "*" } else { "" };
        let read_only_indicator = if editor.is_read_only() { " [RO]" } else { "" };
        let file_name = editor.file_name();
        let (line, col) = editor.cursor_position();
        let total_lines = buffer.len_lines();

        // Check for status messages (errors)
        let (status_msg, is_error) = if let Some((msg, is_err)) = &editor.status_message {
            (msg.as_str(), *is_err)
        } else {
            ("", false)
        };

        let left_status = if !status_msg.is_empty() {
            format!(" {} ", status_msg)
        } else {
            format!(" {}{}{} ", file_name, modified_indicator, read_only_indicator)
        };

        // Add language indicator
        let language = editor.get_language();
        let language_name = match language {
            crate::syntax::Language::PlainText => "Plain",
            crate::syntax::Language::Python => "Python",
            crate::syntax::Language::Sql => "SQL",
            crate::syntax::Language::Rust => "Rust",
            crate::syntax::Language::R => "R",
            crate::syntax::Language::Yaml => "YAML",
            crate::syntax::Language::Markdown => "Markdown",
            crate::syntax::Language::Json => "JSON",
            crate::syntax::Language::Shell => "Shell",
            crate::syntax::Language::Toml => "TOML",
            crate::syntax::Language::Csv => "CSV",
            crate::syntax::Language::Tsv => "TSV",
        };
        let language_info = format!(" [{}] ", language_name);

        // Add kernel info if in REPL mode
        let mut kernel_info = if editor.is_repl_mode() {
            if let Some(kernel_name) = editor.get_kernel_info() {
                format!(" [{}] ", kernel_name)
            } else {
                " [No kernel] ".to_string()
            }
        } else {
            String::new()
        };

        // Format the right status
        let row_info = format!("{}/{}", line + 1, total_lines);
        let right_status = format!(" {:>19}  {:>4} ", row_info, col + 1);

        // Calculate available space and truncate kernel_info if needed
        let min_width = left_status.len() + language_info.len() + right_status.len();
        let max_kernel_width = if min_width < width as usize {
            (width as usize).saturating_sub(min_width)
        } else {
            0
        };

        if kernel_info.len() > max_kernel_width {
            if max_kernel_width > 4 {
                let truncate_to = max_kernel_width.saturating_sub(3);
                kernel_info = kernel_info.chars().take(truncate_to).collect::<String>() + "...";
            } else {
                kernel_info.clear();
            }
        }

        let mut status_line = String::with_capacity(width as usize);
        status_line.push_str(&left_status);
        status_line.push_str(&language_info);
        status_line.push_str(&kernel_info);

        let used_width = left_status.chars().count() + language_info.chars().count() + kernel_info.chars().count() + right_status.chars().count();
        let padding = if used_width < width as usize {
            width as usize - used_width
        } else {
            0
        };
        for _ in 0..padding {
            status_line.push(' ');
        }
        status_line.push_str(&right_status);

        // Final safety check
        let status_chars: Vec<char> = status_line.chars().collect();
        if status_chars.len() > width as usize {
            status_line = status_chars.iter().take(width as usize).collect();
        }

        // Only update if status changed
        if status_line != self.last_status {
            if is_error {
                write!(self.stdout,
                    "\x1b[{};1H\x1b[48;5;196m\x1b[38;5;15m{}\x1b[0m",
                    status_row + 1, status_line)?;
            } else {
                write!(self.stdout,
                    "\x1b[{};1H\x1b[48;5;238m\x1b[38;5;15m{}\x1b[0m",
                    status_row + 1, status_line)?;
            }
            self.last_status = status_line;
            self.stdout.flush()?;
        }

        Ok(())
    }
}

fn render_centered(label: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let label_width: usize = label.chars().map(|c| c.width().unwrap_or(1)).sum();
    if label_width >= width {
        let mut out = String::new();
        let mut used = 0;
        for ch in label.chars() {
            let cw = ch.width().unwrap_or(1);
            if used + cw > width {
                break;
            }
            out.push(ch);
            used += cw;
        }
        while used < width {
            out.push(' ');
            used += 1;
        }
        out
    } else {
        let pad = width - label_width;
        let left_pad = pad / 2;
        let right_pad = pad - left_pad;
        let mut out = String::new();
        for _ in 0..left_pad {
            out.push(' ');
        }
        out.push_str(label);
        for _ in 0..right_pad {
            out.push(' ');
        }
        out
    }
}

fn cursor_line_col_chars(text: &str, cursor_byte: usize) -> (usize, usize) {
    let cursor_byte = cursor_byte.min(text.len());
    let before = &text[..cursor_byte];
    let line = before.bytes().filter(|&b| b == b'\n').count();
    let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let col = text[line_start..cursor_byte].chars().count();
    (line, col)
}
