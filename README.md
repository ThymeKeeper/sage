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

- **Cell-based execution**: Organize code with optional `##--` delimiters (titled by the text after the marker)
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
- **Terminal lane**: Scripts that read stdin get their own terminal window
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

### Interactive Scripts

Sage holds the terminal in raw mode, so a cell that reads from stdin — `input()`,
`getpass`, a `termios`/`curses` key reader — can't be given a usable tty by the
session. Sage detects these and launches them in their own terminal window
instead, then returns immediately; the kernel is left untouched and the window
stays open on exit so you can read the output.

The terminal is autodetected (kitty, alacritty, ghostty, wezterm, foot,
gnome-terminal, konsole, xfce4-terminal, xterm; Terminal.app on macOS; a console
window on Windows). To pin one:

```bash
SAGE_TERMINAL=alacritty sage myscript.py
```

or in `~/.config/sage/config.toml`:

```toml
terminal = "alacritty"
```

With no display to open a window on (a bare console, a plain ssh session), the
script falls back to running with no stdin, and the output pane says why.

Scripts run out-of-process (in a terminal, or as a standalone app) keep the
identity they'd have under a plain `python yourfile.py`: `__file__`,
`sys.argv[0]`, and `sys.path[0]` point at the file you're editing, the working
directory is its folder — so sibling imports and relative data paths work — and
tracebacks name your file and show the lines that actually ran, even with
unsaved edits.

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

### Spreadsheet (CSV/TSV)
- `.csv` / `.tsv` files open as a grid. Any other text can be shown as one with `Ctrl+Y` → **Spreadsheet (CSV)** or **Spreadsheet (TSV)**; sage refuses (and says why) when the text isn't delimited data: no commas/tabs, an unclosed quote, or a row with more fields than the header row (row 1). Short rows are fine; their missing cells are nulls. Without unsaved edits the file's own bytes are read (the text editor turns tabs into spaces), so TSV works from a file but not from text typed into sage
- On a grid, `Ctrl+Y` → **Plain Text** (or any text language) shows the data as text you can edit. The text is kept exactly: typing and pasting insert what you type (tabs and curly quotes are data, not folded as elsewhere in sage), `Tab` types a real tab (shown as `→`), and `Ctrl+S` saves the text as it is. It opens as the file on disk, or, if the grid had unsaved edits, as Save would write them. Line ends become LF (CRLF and lone CR records read the same); sage stays in the grid, and says why, when a quoted value holds a carriage return or the data holds a line break the editor would split on (vertical tab, form feed, U+0085, U+2028, U+2029), since showing it as lines would change the value. `Ctrl+Y` → **Spreadsheet** reads the text back into a grid (or brings back the same grid if the text wasn't changed), and refuses with the reason if the edits broke it. On a grid, choosing the other of CSV/TSV re-reads the file with that delimiter (not while the grid has unsaved edits)
- Typing and moving work as in Excel. Type over a cell and `Enter` keeps the value and moves down (`Shift+Enter` up); `Tab` / `Shift+Tab` keep it and move right / left, and after a run of Tabs `Enter` goes to the start of the next row, so a table can be typed in row by row. On a cell you aren't editing, `Enter` and `Shift+Enter` just move down and up
- Typing over a cell, the arrow keys (and `Page Up/Down`) keep the value and move to the next cell. `F2`, a double-click, or a click in the cell's text or the formula bar edits the value instead: the arrow keys move the caret. `F2` switches between the two, and the formula bar says which (`typing` or `editing`). `Ctrl+Enter` keeps the value and stays on the cell; `Esc` drops the change. `Alt+Enter` starts a new line inside the cell; so does `Shift+Enter` while editing a value (`F2`), since Windows Terminal keeps `Alt+Enter` for full screen unless that binding is removed
- Keys typed ahead while sage is busy (a save, a commit on a huge file) land cell after cell as typed. On Windows a terminal paste arrives as keys too; sage tells it apart by comparing it with the clipboard
- What you type shows in the cell itself (and in the formula bar); a value longer than the column widens the cell over its neighbours to the right while you type, as Excel does
- Clearing a cell makes it a null (∅, saved as an empty field, `1,,`): `Delete` or `Backspace` on the selection, `Ctrl+X`, deleting all of a cell's text, or pasting an empty field over a value. A cell opened (`F2`, a double-click, a click in the formula bar) and left with its text as it was isn't changed at all: a null stays a null. The only empty strings (`""`) are the ones the file already had
- The status bar sums up the selected cells: `count` (cells selected), `unique` (distinct values, nulls and blanks aside, as SQL's `COUNT(DISTINCT)`), `null` (the share of cells that are ∅), and, when every value is a number or every value a date, `sum`/`avg`/`min`/`max`. Row 1 is the header, so it's left out when data rows are selected too (a column picked by its letter is summed without it). Under a filter only the rows shown count
- The grid holds every cell in memory, about twice the file's size plus 50 bytes a cell (a 53 MB file of 1M rows × 10 columns takes about 600 MB). A file that wouldn't fit in the memory the machine has free isn't opened: sage says how much it would need and how much is free. Files that size belong in DuckDB or pivot
- An empty buffer or file is an empty grid: every row and column is a ghost cell until you type
- Ghost cells: empty cells continue past the last row and column. Arrow or click into them and type a value to grow the data to that cell (other new cells are nulls, saved as empty fields); `Ctrl+Z` undoes it
- Save writes every row to the header's width, filling missing cells with empty fields
- `Alt+Down` or right-click: filter & sort menu for the column (Excel-style value checklist with search; row 1 is the header)
- The same menu offers **Convert dates to ISO 8601** on a date column: `25/04/26` becomes `2026-04-25` and `03-25-2026 02:15 PM` becomes `2026-03-25 14:15:00`. Day/month order is proven per column from the data (a part over 12), never guessed; a column that can't be settled asks, and a column mixing both orders is refused. Year-first dates are year-month-day unless a middle part over 12 proves year-day-month (`2023-31-12` becomes `2023-12-31`). Values that can't be a date in any reading (`20/20/2000`) are listed and left as they are. Hidden rows convert too; `Ctrl+Z` undoes it
- `Ctrl+Shift+L`: clear all filters
- `Ctrl+C` copies the selected cells as TSV; `Ctrl+V` (or the terminal's own paste) pastes TSV cells, as copied from sage or another spreadsheet, at the cursor (or the top-left of the selection). Like copy and `Delete`, a paste sees only the rows shown: under a filter, clipboard row 1 goes to the first shown row, row 2 to the next shown row, and so on, and rows the filter hides are never changed. Under a sort, rows fill in the order shown, so copying cells and pasting them into another column of the same view lines them up row for row. Rows or columns past the data are added as with ghost cells, and a paste is one undo step. One copied value fills a selection; a selection a whole number of blocks tall and wide is filled with copies of the block. An empty clipboard field clears a cell that has a value and leaves an empty one as it is. While a cell is being edited, a paste goes into that cell
- `Ctrl+=` inserts blank rows above the selection, as many as it covers; `Ctrl+-` deletes the rows it covers. With whole columns selected (click or drag across column letters, or a selection running the grid's full height but not its full width; `Ctrl+A` counts as rows), they insert columns to the left or delete the columns instead. Under a filter only the rows shown are deleted, and inserted rows go into the file just above the selection's top row and show where you put them. While a filter or sort is on, the header row stays put: it isn't deleted, and nothing can be inserted above it. Deleting a column drops its filter and sort. The keys need a terminal that passes them on with the Ctrl held: kitty does; WezTerm, foot, Ghostty and Windows Terminal use `Ctrl+=` and `Ctrl+-` to change the font size unless that binding is removed, and older terminals send `Ctrl+=` as a plain `=`
- `Ctrl+Z` / `Ctrl+Shift+Z`: undo / redo cell edits, range clears, pastes and row/column inserts and deletes (works while filtered or sorted)
- Filters and sorts change only what the grid shows: Save writes every row in the file's own order

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

Cells let you organize code into logical sections. Use `##--` as a delimiter.
Any text after the marker becomes the cell's title in the output pane:

```python
##-- Cell1: import libraries
import pandas as pd
import duckdb as db

##-- Cell 2: Load data
df = pd.read_csv("data.csv")
db.register("data", df)

##-- Cell 3: Query with SQL autocomplete
result = db.sql("SELECT * FROM data WHERE amount > 100")
result.pl()  # Method chain autocomplete works here!
```

**Pro tip**: Cell delimiters are optional! Without them, the entire file runs as one cell.

In SQL mode, statements are split on semicolons, and a leading `--## <title>`
comment titles that statement's output the same way.

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

### SQL mode autocomplete
In SQL mode (with or without a Snowflake connection) typing a name opens suggestions; `Tab` accepts, `Up`/`Down` choose, `Esc` closes. Nothing pops up inside a string or comment.
- **Snowflake's words:** keywords, commands, data types, date parts, common parameters and COPY/file-format options, every built-in function (taken from `SHOW FUNCTIONS`, in `src/sql_functions.rs`) and the table functions (`FLATTEN`, `RESULT_SCAN`, `QUERY_HISTORY`, ...).
- **Names from your queries:** each statement that runs without error adds every table, alias, CTE, column and other name in its text, and its result's column names. They are kept in memory until sage closes, never saved, and come first in the list. After a dot, the parts seen after that prefix come first (`DM_FPS_PRD.PRIV.SU` offers the tables used under it that start with `SU`).
- Suggestions start with the first letter of a name, never straight after a dot (this holds for Python completion too).
- **Case-sensitive:** every word is offered in upper, lower and title case, and matches what you type exactly: `sel` offers `select`, `Sel` offers `Select`, `SEL` offers `SELECT` (`date_t` offers `date_trunc`, `Date_T` offers `Date_Trunc`). A name you wrote in mixed case is also offered as written. A quoted name (`"Fare Class"`) is offered only as written, since its case is part of the name; type its letters without the quote to find it.

### SQL inside Python
With a Python kernel connected, the caret inside the SQL string of a call gets SQL suggestions, matched the same case-sensitive way:
- **`sf.sql("...")` and `sf.submit("...")`** (abp, Snowflake) get everything SQL mode gets, and share its list of names: a Python cell that runs without error adds the names in the SQL it passed to `sf.sql`/`sf.submit`, and a query run in SQL mode is offered here too (and the other way round).
- **`db.sql(...)`, `.execute(...)`, `.query(...)`, `read_sql*(...)`, `spark.sql(...)`** get the tables, columns and functions the kernel finds in DuckDB or Spark after each run, and the common SQL words.
- Only the string passed straight to the call counts (`sf.sql(q)` with `q` built elsewhere doesn't). In an f-string, the `{...}` fields are Python. Python comments, raw strings and triple quotes are read as Python reads them.

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
