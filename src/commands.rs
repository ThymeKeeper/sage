/// All possible editor commands
#[derive(Debug, Clone)]
pub enum Command {
    // Movement
    MoveUp,
    MoveDown,
    MoveLeft,
    MoveRight,
    MoveHome,
    MoveEnd,
    PageUp,
    PageDown,
    
    // Selection movement
    SelectUp,
    SelectDown,
    SelectLeft,
    SelectRight,
    SelectHome,
    SelectEnd,
    SelectAll,
    
    // Editing
    InsertChar(char),
    InsertNewline,
    InsertTab,
    Backspace,
    Delete,
    
    // Clipboard operations
    Copy,
    Cut,
    Paste,
    
    // File operations
    Save,
    SaveAs,
    
    // Undo/Redo
    Undo,
    Redo,
    
    // No operation
    None,
}