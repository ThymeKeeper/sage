# sage

A Python REPL and pseudo Jupyter notebook in your terminal.

## Overview

sage is a terminal-based Python REPL and pseudo Jupyter notebook written in Rust. It combines a text editor with interactive Python execution, allowing you to write code in cells and execute them with live feedback - all in your terminal.

<img width="1219" height="628" alt="thyme" src="https://github.com/user-attachments/assets/e0d7138b-c2c5-446b-bec7-f8bb89d335e2" />

<img width="1219" height="628" alt="findreplace" src="https://github.com/user-attachments/assets/4388739b-1dea-4ec5-b7bb-3313ae0ab558" />

<img width="1219" height="629" alt="saveas" src="https://github.com/user-attachments/assets/1df2f3d1-31c1-4497-b50b-cafa2fa3340c" />

## Features

### Python REPL & Notebook
- **Cell-based execution**: Organize code into cells using `# %%` delimiters
- **Kernel selection**: Connect to any Python interpreter or Jupyter kernel
- **Interactive execution**: Execute cells with Shift+Enter and see results instantly
- **Multiple kernel support**: Switch between different Python environments
- **Persistent state**: Variables persist across cell executions within a session

### Core Editing
- Open and save files
- Create new files with automatic parent directory creation
- Full undo/redo support
- Text selection with keyboard and mouse
- Find and replace functionality
- Unicode support
- Syntax highlighting for Python, Rust, JavaScript, TypeScript, Bash, Markdown, and TOML

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

### Smart Editing
- Auto-indentation: new lines inherit indentation from the previous line
- Tab with selection: indents all selected lines by 4 spaces
- Shift+Tab: dedents current line or all selected lines by up to 4 spaces

## Installation

Requires Rust 1.70 or later.

```bash
git clone https://github.com/yourusername/sage.git
cd sage
cargo build --release
```

The binary will be in `target/release/sage` (or `sage.exe` on Windows).

## Usage

### Basic usage
```bash
# Start sage
sage

# Open a Python file
sage script.py
```

### Keyboard Shortcuts

#### REPL/Notebook Commands
| Action | Shortcut |
|--------|----------|
| Select Python Kernel | Ctrl+K |
| Execute Current Cell | Ctrl+Enter or Ctrl+E |

#### Editor Commands
| Action | Shortcut |
|--------|----------|
| Save | Ctrl+S |
| Save As | Ctrl+Shift+S |
| Quit | Ctrl+Q |
| Undo | Ctrl+Z |
| Redo | Ctrl+Shift+Z |
| Find/Replace | Ctrl+F |
| Find Next | Ctrl+F (when find is open) |
| Find Previous | Ctrl+Shift+F |
| Replace | Ctrl+H |
| Replace All | Ctrl+Shift+H |
| Select All | Ctrl+A |
| Copy | Ctrl+C |
| Cut | Ctrl+X |
| Paste | Ctrl+V |
| Indent | Tab (with selection) |
| Dedent | Shift+Tab |

### Using sage as a Python REPL

1. **Create cells** in your Python file using `# %%` as a delimiter:
   ```python
   # %% Cell 1
   x = 10
   y = 20
   x + y

   # %% Cell 2
   result = x * y
   print(result)
   ```

2. **Select a Python kernel** by pressing `Ctrl+K`. sage will discover available Python interpreters on your system.

3. **Execute cells** by placing your cursor in a cell and pressing `Ctrl+Enter` (or `Ctrl+E` as alternative). The result will be shown in the status bar.

4. **Variables persist** across cell executions, just like in Jupyter notebooks!

### Text Selection

Hold Shift while using arrow keys, Home, or End to select text. Or just use your mouse.

## Technical Details

Built with:
- **ropey** - Efficient rope data structure for text manipulation
- **crossterm** - Cross-platform terminal manipulation
- **arboard** - System clipboard integration
- **unicode-width** - Proper Unicode character width handling
- **zmq** - ZeroMQ for Jupyter kernel protocol
- **tokio** - Async runtime for kernel communication
- **serde/serde_json** - JSON serialization for kernel messages

sage uses a rope data structure for efficient text operations, maintains complete undo/redo history, and communicates with Python kernels using either direct subprocess communication or the Jupyter kernel protocol.

## Contributing

This is a personal project, but if you find bugs, feel free to judge silently.

## Author

A developer who got tired of configuring text editors and decided to write their own. The irony is not lost.
