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
    
    // Editing
    InsertChar(char),
    InsertNewline,
    InsertTab,
    Backspace,
    Delete,
    
    // File operations
    Save,
    
    // Undo/Redo
    Undo,
    Redo,
    
    // No operation
    None,
}