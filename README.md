# Rust Text Editor

A lightweight, terminal-based text editor written in Rust with modern features and robust text handling.

## Features

### Core Editing
- **Efficient text operations** using the Rope data structure for fast insertion/deletion in large files
- **Syntax-aware editing** with proper UTF-8 support
- **Unlimited undo/redo** with intelligent operation grouping (groups edits within 300ms)
- **Text selection** with Shift + Arrow keys
- **File operations** - Open, edit, and save files

### Clipboard Support
- **System clipboard integration** for cross-application copy/paste
- **Ctrl+C** - Copy selected text
- **Ctrl+X** - Cut selected text  
- **Ctrl+V** - Paste from clipboard
- **Ctrl+A** - Select all text

### Text Normalization
The editor automatically sanitizes text for consistency and security:
- **Line endings** - Converts CRLF to LF
- **Indentation** - Converts tabs to 4 spaces
- **Invisible characters** - Removes 40+ types of zero-width and invisible Unicode characters

### User Interface
- **Terminal title** - Shows filename and modified status in terminal title bar
- **Status bar** - Displays filename, modified indicator, and cursor position
- **Visual selection** - Highlighted text selection with blue background
- **Smooth scrolling** - Viewport follows cursor with configurable scroll offset
- **Clean display** - Virtual lines with `~` for empty space

## Installation

### Prerequisites
- Rust 1.70 or later
- Cargo (comes with Rust)

### Building from Source
```bash
git clone <repository-url>
cd te
cargo build --release
```

The compiled binary will be in `target/release/texteditor`

## Usage

### Running the Editor
```bash
# Open empty buffer
cargo run

# Open a file
cargo run -- filename.txt

# Using compiled binary
./target/release/texteditor filename.txt
```

### Keyboard Shortcuts

#### File Operations
| Shortcut | Action |
|----------|--------|
| **Ctrl+S** | Save file |
| **Ctrl+Q** | Quit editor |

#### Editing
| Shortcut | Action |
|----------|--------|
| **Ctrl+Z** | Undo |
| **Ctrl+Shift+Z** | Redo |
| **Ctrl+C** | Copy selection |
| **Ctrl+X** | Cut selection |
| **Ctrl+V** | Paste |
| **Ctrl+A** | Select all |
| **Tab** | Insert 4 spaces |
| **Backspace** | Delete previous character |
| **Delete** | Delete next character |

#### Navigation
| Shortcut | Action |
|----------|--------|
| **Arrow Keys** | Move cursor |
| **Home** | Move to line start |
| **End** | Move to line end |
| **Page Up** | Move up ~20 lines |
| **Page Down** | Move down ~20 lines |

#### Selection
| Shortcut | Action |
|----------|--------|
| **Shift+Arrows** | Select text |
| **Shift+Home** | Select to line start |
| **Shift+End** | Select to line end |

## Technical Details

### Architecture
- **Rope data structure** - Efficient for large files and complex operations
- **Grouped undo system** - Operations within 300ms are grouped into single undo actions
- **Delta-based undo** - Stores only changes, not complete document copies
- **Incremental rendering** - Only redraws changed lines for performance

### Text Sanitization
The editor automatically removes:
- Zero-width spaces (U+200B, U+200C, U+200D)
- Directional marks (U+200E, U+200F, U+202A-E)
- BOMs (U+FEFF)
- Soft hyphens (U+00AD)
- Variation selectors (U+FE00-F)
- 40+ other invisible Unicode characters

This prevents:
- Hidden text attacks
- Compilation errors from invisible characters
- File extension spoofing
- Copy-paste issues from web content

### Memory Efficiency
- Document stored once in memory
- Undo operations store only deltas
- Example: 100 edits on a 10MB file uses ~10MB + 2KB (not 1GB)

### Dependencies
- **ropey** - Rope data structure for efficient text manipulation
- **crossterm** - Cross-platform terminal manipulation
- **arboard** - System clipboard access

## Project Structure
```
te/
├── src/
│   ├── main.rs       # Entry point and input handling
│   ├── editor.rs     # Core editor logic
│   ├── buffer.rs     # Text buffer with undo/redo
│   ├── renderer.rs   # Terminal rendering
│   └── commands.rs   # Command definitions
├── Cargo.toml        # Project configuration
└── README.md         # This file
```

## Features in Detail

### Intelligent Undo Grouping
The editor groups rapid edits together. For example, typing "Hello world" quickly creates one undo group. After a 300ms pause, a new group starts. This creates natural undo boundaries that match how you think about your edits.

### Clipboard Integration
Full system clipboard support means you can:
- Copy from the editor and paste in other applications
- Copy from browsers/documents and paste in the editor
- All clipboard content is automatically sanitized

### Terminal Title
The terminal title shows:
- Filename (or "No Name" for new files)
- Modified indicator (`*`) when unsaved changes exist
- Updates automatically as you work

### Text Normalization
All text is normalized to ensure consistency:
- Windows (CRLF) → Unix (LF) line endings
- Tab characters → 4 spaces
- Invisible/zero-width characters → removed

This happens automatically when:
- Opening files
- Pasting text
- Typing

## Known Limitations
- No syntax highlighting (yet)
- No find/replace functionality
- No multi-cursor support
- Single file editing only (no tabs/splits)
- No mouse support

## Building and Testing
```bash
# Run tests
cargo test

# Build with optimizations
cargo build --release

# Run with logging
RUST_LOG=debug cargo run

# Check for issues
cargo clippy
```

## Performance
- Handles large files (tested with 100MB+ files)
- Instant operations on typical code files
- Memory efficient undo/redo
- Minimal CPU usage during idle

## Platform Support
- ✅ Windows (10/11, Windows Terminal, Command Prompt, PowerShell)
- ✅ Linux (Most terminals)
- ✅ macOS (Terminal.app, iTerm2)
- ✅ WSL/WSL2

---

*A personal project for learning Rust and building a practical text editor with modern features.*
