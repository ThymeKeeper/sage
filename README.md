# thyme

A terminal text editor that won't make you cry. Much.

## Overview

thyme is a terminal-based text editor written in Rust. It's the editor you use when nano feels too bloated and vim's learning curve looks like a wall. Built with the revolutionary idea that text editors should let you edit text.

## Features

### Core Editing
- Open and save files
- Create new files with automatic parent directory creation
- Full undo/redo support
- Text selection with keyboard and mouse
- Find and replace functionality
- Unicode support

### Input & Navigation
- Arrow keys for navigation
- Home/End key support
- Page Up/Page Down for quick scrolling
- Mouse support for:
  - Click to position cursor
  - Drag to select text
  - Scroll wheel for vertical scrolling
  - Shift+scroll for horizontal scrolling
  - Double-click to select word
  - Triple-click to select line

### Clipboard Operations
- Copy (Ctrl+C)
- Cut (Ctrl+X)
- Paste (Ctrl+V)
- Works with system clipboard

### Text Normalization
- Automatically converts CRLF to LF
- Converts tabs to spaces (4 spaces)
- Filters out zero-width and invisible Unicode characters
- Handles various text encodings gracefully

## Installation

Requires Rust 1.70 or later.

```bash
git clone https://github.com/yourusername/thyme.git
cd thyme
cargo build --release
```

The binary will be in `target/release/thyme` (or `thyme.exe` on Windows).

## Usage

### Basic usage
```bash
# Open a file
thyme filename.txt

# Create a new file
thyme
```

### Keyboard Shortcuts

| Action | Shortcut |
|--------|----------|
| Save | Ctrl+S |
| Save As | Ctrl+Shift+S |
| Quit | Ctrl+Q |
| Undo | Ctrl+Z |
| Redo | Ctrl+Shift+Z |
| Find/Replace | Ctrl+F |
| Find Next | F3 or Ctrl+F (when find is open) |
| Find Previous | Shift+F3 or Ctrl+Shift+F |
| Replace | Ctrl+H |
| Replace All | Ctrl+Shift+H |
| Select All | Ctrl+A |
| Copy | Ctrl+C |
| Cut | Ctrl+X |
| Paste | Ctrl+V |

### Selection

Hold Shift while using arrow keys, Home, or End to select text. Or just use your mouse like a normal person.

## Technical Details

Built with:
- **ropey** - Efficient rope data structure for text manipulation
- **crossterm** - Cross-platform terminal manipulation
- **arboard** - System clipboard integration
- **unicode-width** - Proper Unicode character width handling

The editor uses a rope data structure for efficient text operations on large files and maintains a complete undo/redo history.

## Contributing

This is a personal project, but if you find bugs, feel free to judge silently.

## Author

A developer who got tired of configuring text editors and decided to write their own. The irony is not lost.