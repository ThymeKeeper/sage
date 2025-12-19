# Sage

A terminal-based Python notebook editor that brings Jupyter-style interactive coding to your command line.

## What is Sage?

Sage lets you write and execute Python code in cells, just like Jupyter notebooks, but entirely in your terminal. Work with the speed and efficiency of a text editor while getting the interactivity of a notebook environment.

## Features

### Interactive Notebook Experience

- **Cell-based execution**: Organize code into executable cells using `##$$` delimiters
- **Live output display**: View execution results in a dedicated output pane
- **Multiple kernels**: Connect to different Python environments and switch between them
- **Execution state tracking**: See which cells have been executed and their output history

### Powerful Editing

- **Syntax highlighting**: Python code is highlighted for better readability
- **Smart autocomplete**: Get code suggestions as you type
- **Bracket matching**: Automatically highlights matching brackets and parentheses
- **Find and replace**: Search through code with regex support
- **Undo/redo**: Full history of all edits
- **Multiple selection modes**: Click, double-click to select words, triple-click for lines

### Seamless Workflow

- **Mouse support**: Click to position cursor, drag to select, scroll with mouse wheel
- **System clipboard**: Copy and paste between Sage and other applications
- **Auto-save indicators**: Know when your work is saved
- **Smart indentation**: Tab/Shift+Tab to indent/unindent selections
- **Word-level navigation**: Jump between words with Ctrl+Arrow keys

### Execution Modes

- **Interactive mode**: Edit and execute cells in a live session
- **Headless execution**: Run notebook files from the command line without opening the editor
- **Error handling**: Execution stops on errors with clear traceback information
- **Output persistence**: Cell outputs remain visible until cleared or re-executed

## How to Use

### Opening Files

```bash
sage myfile.py    # Open an existing file or create a new one
sage              # Start with an empty file
```

### Running Scripts

Execute a Python notebook file without opening the editor:

```bash
sage --execute myfile.py
sage --execute myfile.py --python /path/to/python3  # Use specific Python
```

Headless mode will execute each cell in order, print outputs, and stop if any cell encounters an error.

## Key Bindings

### File operations
- `Ctrl+S`: Save file
- `Ctrl+Q`: Quit

### Editing
- `Ctrl+Z`: Undo
- `Ctrl+C`: Copy
- `Ctrl+X`: Cut
- `Ctrl+V`: Paste
- `Ctrl+A`: Select all
- `Ctrl+Backspace`: Delete word backward
- `Tab`: Indent selection (or insert tab)
- `Shift+Tab`: Unindent selection

### Navigation
- Arrow keys: Move cursor
- `Home`/`End`: Move to start/end of line
- `Page Up`/`Page Down`: Scroll viewport
- `Ctrl+Home`: Move to start of file
- `Ctrl+End`: Move to end of file
- `Ctrl+Left`/`Ctrl+Right`: Move by word

### Search
- `Ctrl+F`: Find
- `Ctrl+H`: Find and replace
- `Ctrl+Shift+F`: Find next
- `Ctrl+Shift+H`: Find previous

### Notebook operations
- `Ctrl+E`: Execute current cell
- `Ctrl+K`: Select/change Python kernel
- `Ctrl+L`: Clear cell outputs
- `Ctrl+O`: Toggle focus between editor and output pane

### Mouse
- Left click: Position cursor
- Click and drag: Select text
- Double click: Select word
- Triple click: Select line
- Scroll wheel: Scroll viewport

## Working with Cells

Cells are the building blocks of your notebook. Sage uses `##$$` as a cell delimiter:

```python
# Cell 1: Import libraries
import numpy as np
import matplotlib.pyplot as plt

##$$

# Cell 2: Generate data
x = np.linspace(0, 10, 100)
y = np.sin(x)

##$$

# Cell 3: Plot results
plt.plot(x, y)
plt.show()
```

Each cell can be executed independently with `Ctrl+E`. Without delimiters, the entire file is treated as a single cell.

## Python Kernel Selection

Sage automatically discovers Python interpreters on your system. Press `Ctrl+K` to:
- View available Python environments
- Switch between different Python versions
- Connect to virtual environments

You can also specify a Python interpreter via shebang in your file:
```python
#!/usr/bin/env python3
```

Or use the `--python` flag when running in headless mode.

## Tips

- **Double-click** any word to see all occurrences highlighted throughout your file
- Use **Ctrl+O** to toggle focus between the editor and output pane for easy scrolling through results
- **Ctrl+L** clears all cell outputs when you want a fresh start
- Press **Esc** to cancel find/replace or other dialogs
- Sage auto-launches in a terminal if you open it from a file manager
