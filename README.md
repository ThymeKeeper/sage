# Sage

A terminal-based Python notebook editor that brings Jupyter-style interactive coding to your command line with intelligent autocomplete for Python, SQL, and more.

## What is Sage?

Sage lets you write and execute Python code in cells, just like Jupyter notebooks, but entirely in your terminal. Work with the speed and efficiency of a text editor while getting the interactivity of a notebook environment, complete with context-aware autocomplete that understands your code.

## Key Features

### 🧠 Intelligent Autocomplete

- **Context-aware SQL autocomplete**: Get table, column, and function suggestions when typing SQL queries
  - Automatically activates inside `db.sql("...")`, `spark.sql("...")`, and similar functions
  - Works with DuckDB and Spark SQL
  - Supports f-strings: `db.sql(f"SELECT {var} FROM ...")`
  - Dynamically updates as you create new tables
  - Case-insensitive matching with 80+ SQL keywords

- **Method chain completion**: Smart suggestions for chained methods
  - Type-aware: `df.groupby(...).agg(...)` shows relevant methods
  - Works with DuckDB relations: `db.sql("...").pl()` suggests `.pl()`, `.show()`, etc.

- **Python autocomplete**: Keywords, built-ins, and your defined variables
  - Introspects your namespace in real-time
  - Suggests module attributes and methods

### 📓 Interactive Notebook Experience

- **Cell-based execution**: Organize code with optional `##$$` delimiters
- **Live output display**: View execution results in a dedicated pane
- **Multiple kernels**: Connect to different Python environments
- **Execution state tracking**: See cell history and outputs
- **No delimiters required**: Works with plain Python files too

### ✏ Powerful Editing

- **Syntax highlighting**: Python code with clear visual structure
- **Bracket matching**: Highlights matching brackets and parentheses
- **Find and replace**: Search with regex support
- **Undo/redo**: Full edit history
- **Multiple selection modes**: Word, line, or custom selections
- **Smart indentation**: Tab/Shift+Tab for blocks

### 🖱 Seamless Workflow

- **Mouse support**: Click, drag, scroll - works like a GUI editor
- **System clipboard**: Copy/paste between applications
- **Auto-save indicators**: Always know your save status
- **Word-level navigation**: Ctrl+Arrow keys to jump between words
- **Output pane navigation**: Scroll through long outputs easily

### 🚀 Execution Modes

- **Interactive mode**: Edit and execute cells in a live session
- **Headless execution**: Run notebooks from the command line
- **Error handling**: Clear tracebacks, stops on errors
- **Output persistence**: Results stay visible until cleared

## Quick Start

### Installation

```bash
cargo build --release
./target/release/sage
```

### Opening Files

```bash
sage myfile.py    # Open existing file or create new
sage              # Start with empty file
```

### Running Scripts

Execute without opening the editor:

```bash
sage --execute myfile.py
sage --execute myfile.py --python /path/to/python3
```

Autocomplete automatically shows:
- **Tables**: `users`, `orders`, etc.
- **Columns**: Both qualified (`users.name`) and unqualified (`name`)
- **SQL Keywords**: `SELECT`, `FROM`, `WHERE`, `JOIN`, `GROUP BY`, etc.
- **Functions**: `COUNT`, `SUM`, `AVG`, database-specific functions

## Key Bindings

### File Operations
- `Ctrl+S`: Save file
- `Ctrl+Q`: Quit

### Editing
- `Ctrl+Z`: Undo
- `Ctrl+C`: Copy
- `Ctrl+X`: Cut
- `Ctrl+V`: Paste
- `Ctrl+A`: Select all
- `Ctrl+Backspace`: Delete word backward
- `Tab`: Indent selection (or autocomplete)
- `Shift+Tab`: Unindent selection

### Navigation
- Arrow keys: Move cursor
- `Ctrl+Left/Right`: Move by word
- `Ctrl+Home/End`: Jump to start/end of file
- `Page Up/Down`: Scroll viewport
- `Shift+Page Up/Down`: Scroll output pane

### Search
- `Ctrl+F`: Find
- `Ctrl+H`: Find and replace
- `Ctrl+Shift+F`: Find next
- `Ctrl+Shift+H`: Find previous

### Notebook Operations
- `Ctrl+E`: Execute current cell
- `Ctrl+K`: Select/change Python kernel
- `Ctrl+L`: Clear cell outputs
- `Ctrl+O`: Toggle focus (editor ↔ output pane)

### Mouse
- Left click: Position cursor
- Click and drag: Select text
- Double click: Select word (highlights all occurrences)
- Triple click: Select line
- Scroll wheel: Scroll viewport

## Working with Cells

Cells let you organize code into logical sections. Use `##$$` as a delimiter:

```python
##$$ Cell1: import libraries
import pandas as pd
import duckdb as db

##$$ Cell 2: Load data
df = pd.read_csv("data.csv")
db.register("data", df)

##$$ Cell 3: Query with SQL autocomplete
result = db.sql("SELECT * FROM data WHERE amount > 100")
result.pl()  # Method chain autocomplete works here!
```

**Pro tip**: Cell delimiters are optional! Without them, the entire file runs as one cell.

## Python Kernel Selection

Sage auto-discovers Python interpreters. Press `Ctrl+K` to:
- View available Python environments
- Switch between Python versions
- Connect to virtual environments

Specify a kernel via shebang:
```python
#!/usr/bin/env python3
```

Or use the `--python` flag in headless mode.

## SQL Support

### DuckDB
```python
import duckdb as db

# Module usage (default connection)
db.sql("SELECT * FROM table")

# Explicit connection
conn = duckdb.connect("mydb.duckdb")
conn.sql("SELECT * FROM table")
```

### Spark
```python
from pyspark.sql import SparkSession

spark = SparkSession.builder.appName("app").getOrCreate()
spark.sql("SELECT * FROM table")
```

Autocomplete works automatically with both! Create tables dynamically and they'll appear in suggestions immediately.

## Tips & Tricks

- **Double-click** any word to highlight all occurrences
- **Ctrl+O** switches focus to the output pane for scrolling long results
- **Ctrl+L** clears outputs for a fresh start
- **Esc** cancels dialogs and operations
- SQL autocomplete is **case-insensitive**: type `sel` → get `SELECT`
- Use **method chains** with confidence: `.sql(...).pl()` knows what methods are available
- **No delimiters needed**: Just write Python and execute with Ctrl+E

## Project Goals

Sage aims to combine:
- 🚀 The speed of terminal-based editing
- 📊 The interactivity of Jupyter notebooks
- 🧠 The intelligence of modern IDEs
- 🎯 The simplicity of Python scripts

Perfect for data science, SQL exploration, quick experiments, and interactive development.

## License

MIT License - see LICENSE file for details.
