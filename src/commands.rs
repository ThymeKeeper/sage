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
    MoveWordLeft,
    MoveWordRight,
    MoveParagraphUp,
    MoveParagraphDown,
    
    // Selection movement
    SelectUp,
    SelectDown,
    SelectLeft,
    SelectRight,
    SelectHome,
    SelectEnd,
    SelectAll,
    SelectWordLeft,
    SelectWordRight,
    SelectParagraphUp,
    SelectParagraphDown,
    
    // Editing
    InsertChar(char),
    InsertNewline,
    InsertTab,
    Indent,    // Indent line(s)
    Dedent,    // Dedent line(s)
    Backspace,
    Delete,
    
    // Clipboard operations
    Copy,
    Cut,
    Paste,
    
    // File operations
    Save,
    SaveAs,
    
    // Find and Replace
    FindReplace,
    FindNext,
    FindPrev,
    Replace,
    ReplaceAll,
    
    // Undo/Redo
    Undo,
    Redo,
    
    // No operation
    None,
}