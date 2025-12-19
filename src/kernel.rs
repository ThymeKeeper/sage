use serde::{Deserialize, Serialize};
use std::error::Error;
use std::os::unix::fs::MetadataExt;

/// Represents the output from code execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExecutionOutput {
    /// Standard output text
    Stdout(String),
    /// Standard error text
    Stderr(String),
    /// Execution result (return value)
    Result(String),
    /// Error with traceback
    Error { ename: String, evalue: String, traceback: Vec<String> },
    /// Rich output (images, HTML, etc.) - for future enhancement
    Display { data: String, mime_type: String },
}

/// Completion item for autocomplete
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionItem {
    pub name: String,
    #[serde(rename = "type")]
    pub item_type: String,
}

/// Type relationship data for intelligent autocomplete
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeRelationships {
    /// Maps callable names to their return types (e.g., "duckdb.sql" -> "DuckDBPyConnection")
    #[serde(default)]
    pub return_types: std::collections::HashMap<String, String>,

    /// Maps type names to their methods (e.g., "DataFrame" -> ["head", "tail", ...])
    #[serde(default)]
    pub type_methods: std::collections::HashMap<String, Vec<String>>,
}

impl Default for TypeRelationships {
    fn default() -> Self {
        TypeRelationships {
            return_types: std::collections::HashMap::new(),
            type_methods: std::collections::HashMap::new(),
        }
    }
}

/// SQL metadata for SQL autocomplete
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SqlMetadata {
    /// Table names available in the SQL database
    #[serde(default)]
    pub tables: Vec<String>,

    /// Column names with their table names (format: "table.column")
    #[serde(default)]
    pub columns: Vec<String>,

    /// SQL function names
    #[serde(default)]
    pub functions: Vec<String>,
}

impl Default for SqlMetadata {
    fn default() -> Self {
        SqlMetadata {
            tables: Vec::new(),
            columns: Vec::new(),
            functions: Vec::new(),
        }
    }
}

/// Execution result with combined output
#[derive(Debug, Clone)]
pub struct ExecutionResult {
    pub outputs: Vec<ExecutionOutput>,
    pub execution_count: Option<usize>,
    pub success: bool,
    pub completions: Vec<CompletionItem>,
    pub type_relationships: TypeRelationships,
    pub sql_metadata: SqlMetadata,
}

/// Information about an available Python kernel
#[derive(Debug, Clone)]
pub struct KernelInfo {
    pub name: String,
    pub display_name: String,
    pub python_path: String,
    pub kernel_type: KernelType,
}

/// Type of kernel connection
#[derive(Debug, Clone, PartialEq)]
pub enum KernelType {
    /// Direct Python subprocess
    Direct,
    /// Jupyter kernel via ZMQ
    Jupyter,
}

/// Trait for Python kernel implementations
pub trait Kernel: Send {
    /// Start/connect to the kernel
    fn connect(&mut self) -> Result<(), Box<dyn Error>>;

    /// Execute code and return the result
    fn execute(&mut self, code: &str) -> Result<ExecutionResult, Box<dyn Error>>;

    /// Disconnect/shutdown the kernel
    fn disconnect(&mut self) -> Result<(), Box<dyn Error>>;

    /// Check if kernel is connected
    fn is_connected(&self) -> bool;

    /// Get kernel information
    fn info(&self) -> KernelInfo;
}

/// Discover available Python kernels on the system
pub fn discover_kernels() -> Vec<KernelInfo> {
    let mut kernels = Vec::new();

    // Add direct Python interpreters
    kernels.extend(discover_python_interpreters());

    // Add Jupyter kernels (optional - comment out if not needed)
    // Jupyter kernels are registered ipykernel installations
    // Only needed if you want to connect to Jupyter kernel infrastructure
    // kernels.extend(discover_jupyter_kernels());

    kernels
}

/// Find Python interpreters on the system
fn discover_python_interpreters() -> Vec<KernelInfo> {
    let mut interpreters = Vec::new();
    // Track seen files by (device, inode) to avoid duplicates from hardlinks/symlinks
    let mut seen_inodes = std::collections::HashSet::new();

    // 1. Check environment variable VIRTUAL_ENV (currently active venv)
    if let Ok(venv_path) = std::env::var("VIRTUAL_ENV") {
        let python_path = format!("{}/bin/python", venv_path);
        if std::path::Path::new(&python_path).exists() {
            // Don't canonicalize - preserve venv path for detection
            add_interpreter(&mut interpreters, &mut seen_inodes, python_path, "python [active venv]");
        }
    }

    // 2. Check workspace venvs (current directory and parents)
    check_workspace_venvs(&mut interpreters, &mut seen_inodes);

    // 3. Check virtualenvwrapper environments
    check_virtualenvwrapper(&mut interpreters, &mut seen_inodes);

    // 4. Check pyenv installations
    check_pyenv(&mut interpreters, &mut seen_inodes);

    // 5. Check conda environments
    check_conda(&mut interpreters, &mut seen_inodes);

    // 6. Check common system directories
    check_system_paths(&mut interpreters, &mut seen_inodes);

    // 7. Check PATH environment variable
    check_path_pythons(&mut interpreters, &mut seen_inodes);

    interpreters
}

/// Helper to add an interpreter with deduplication and metadata
fn add_interpreter(
    interpreters: &mut Vec<KernelInfo>,
    seen_inodes: &mut std::collections::HashSet<(u64, u64)>,
    path: String,
    name_hint: &str,
) {
    // Check for exact path duplicates first (happens with venvs checked multiple times)
    if interpreters.iter().any(|k| k.python_path == path) {
        return;
    }

    // Get file metadata to check for duplicates by inode
    let metadata = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(_) => return, // Skip if we can't access the file
    };

    // Check if this is a venv - always include venvs even if they share inodes
    let is_venv = path.contains("/venv/") || path.contains("/.venv/") ||
                  path.contains("/virtualenv/") || path.contains("/.virtualenv/") ||
                  (path.contains("/env/bin/") && !path.starts_with("/usr") && !path.starts_with("/bin"));

    let file_id = (metadata.dev(), metadata.ino());

    // Only deduplicate non-venv paths by inode
    if !is_venv {
        if seen_inodes.contains(&file_id) {
            return; // Already seen this file (via symlink or hardlink)
        }
        seen_inodes.insert(file_id);
    }

    // Get Python version
    let version = std::process::Command::new(&path)
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| {
            let stdout = String::from_utf8(o.stdout).ok()?;
            let stderr = String::from_utf8(o.stderr).ok()?;
            Some(if !stdout.is_empty() { stdout } else { stderr })
        })
        .unwrap_or_default()
        .trim()
        .to_string();

    // Detect environment type based on path, not name_hint
    let is_venv = path.contains("/venv/") || path.contains("/.venv/") ||
                  path.contains("/virtualenv/") || path.contains("/.virtualenv/") ||
                  path.contains("/env/bin/") && !path.starts_with("/usr") && !path.starts_with("/bin");

    let env_type = if is_venv {
        "venv"
    } else if path.contains("/conda") || path.contains("/anaconda") || path.contains("/miniconda") {
        "conda"
    } else if path.contains("/.pyenv/") {
        "pyenv"
    } else if path.starts_with("/usr/local") {
        "local"
    } else if path.starts_with("/usr") || path.starts_with("/bin") {
        "global"
    } else if path.contains("/opt/homebrew") || path.contains("/usr/local/Cellar") {
        "homebrew"
    } else {
        ""
    };

    // Format display name to match VS Code style: "Python X.Y.Z (env_type) path"
    let display_name = if version.is_empty() {
        if env_type.is_empty() {
            format!("{} {}", name_hint, path)
        } else {
            format!("{} ({}) {}", name_hint, env_type, path)
        }
    } else {
        if env_type.is_empty() {
            format!("{} {}", version, path)
        } else {
            format!("{} ({}) {}", version, env_type, path)
        }
    };

    interpreters.push(KernelInfo {
        name: name_hint.to_string(),
        display_name,
        python_path: path,
        kernel_type: KernelType::Direct,
    });
}

/// Check workspace venvs in current directory and parent directories
fn check_workspace_venvs(
    interpreters: &mut Vec<KernelInfo>,
    seen_inodes: &mut std::collections::HashSet<(u64, u64)>,
) {
    let venv_names = vec![".venv", "venv", ".virtualenv", "env"];
    // Only check "python" for venvs (not "python3") to avoid duplicates
    let python_names = vec!["python"];

    // Check VS Code settings first
    check_vscode_settings(interpreters, seen_inodes);

    // Check home directory explicitly for common venv locations
    if let Ok(home) = std::env::var("HOME") {
        let home_path = std::path::Path::new(&home);
        check_venvs_in_directory(interpreters, seen_inodes, home_path, &venv_names, &python_names);

        // Also check common project directories in home
        let common_project_dirs = vec!["code", "projects", "workspace", "dev", "Documents"];
        for project_dir in common_project_dirs {
            let project_path = home_path.join(project_dir);
            if project_path.exists() && project_path.is_dir() {
                // Check for venvs directly in the project directory
                check_venvs_in_directory(interpreters, seen_inodes, &project_path, &venv_names, &python_names);

                // Check one level deeper (e.g., ~/code/myproject/.venv)
                if let Ok(entries) = std::fs::read_dir(&project_path) {
                    for entry in entries.flatten().take(20) {  // Limit to 20 subdirs to avoid slowdown
                        if entry.file_type().ok().map(|t| t.is_dir()).unwrap_or(false) {
                            check_venvs_in_directory(interpreters, seen_inodes, &entry.path(), &venv_names, &python_names);
                        }
                    }
                }
            }
        }
    }

    // Check current directory
    let current_dir = std::env::current_dir().ok();
    if let Some(dir) = &current_dir {
        check_venvs_in_directory(interpreters, seen_inodes, dir, &venv_names, &python_names);
    }

    // Check parent directories (up to 5 levels to reach home from deeper paths)
    if let Some(mut dir) = current_dir {
        for _ in 0..5 {
            if let Some(parent) = dir.parent() {
                check_venvs_in_directory(interpreters, seen_inodes, parent, &venv_names, &python_names);
                dir = parent.to_path_buf();
            } else {
                break;
            }
        }
    }
}

/// Check for venvs in a specific directory
fn check_venvs_in_directory(
    interpreters: &mut Vec<KernelInfo>,
    seen_inodes: &mut std::collections::HashSet<(u64, u64)>,
    dir: &std::path::Path,
    venv_names: &[&str],
    python_names: &[&str],
) {
    for venv_name in venv_names {
        for python_name in python_names {
            let venv_path = dir.join(venv_name).join("bin").join(python_name);
            if venv_path.exists() {
                // Don't canonicalize - we need to preserve the venv path for detection
                // canonicalize would resolve symlinks and lose the "/venv/" part of the path
                add_interpreter(
                    interpreters,
                    seen_inodes,
                    venv_path.to_string_lossy().to_string(),
                    "python [workspace venv]",
                );
            }
        }
    }
}

/// Check VS Code settings for configured Python interpreter
fn check_vscode_settings(
    interpreters: &mut Vec<KernelInfo>,
    seen_inodes: &mut std::collections::HashSet<(u64, u64)>,
) {
    // Check .vscode/settings.json in current directory and parents
    if let Ok(current_dir) = std::env::current_dir() {
        let mut dir = Some(current_dir);
        while let Some(d) = dir {
            let settings_path = d.join(".vscode").join("settings.json");
            if settings_path.exists() {
                if let Ok(contents) = std::fs::read_to_string(&settings_path) {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents) {
                        // Check for python.defaultInterpreterPath or python.pythonPath
                        for key in &["python.defaultInterpreterPath", "python.pythonPath"] {
                            if let Some(path) = json.get(key).and_then(|v| v.as_str()) {
                                // Expand tilde and resolve relative paths
                                let expanded_path = if path.starts_with("~/") {
                                    std::env::var("HOME")
                                        .ok()
                                        .map(|home| path.replacen("~", &home, 1))
                                        .unwrap_or_else(|| path.to_string())
                                } else if path.starts_with("./") || !path.starts_with('/') {
                                    d.join(path).to_string_lossy().to_string()
                                } else {
                                    path.to_string()
                                };

                                if std::path::Path::new(&expanded_path).exists() {
                                    // Don't canonicalize - preserve VS Code configured path
                                    add_interpreter(
                                        interpreters,
                                        seen_inodes,
                                        expanded_path,
                                        "python [vscode]",
                                    );
                                }
                            }
                        }
                    }
                }
                break; // Only check the first .vscode/settings.json found
            }
            dir = d.parent().map(|p| p.to_path_buf());
        }
    }
}

/// Check virtualenvwrapper environments
fn check_virtualenvwrapper(
    interpreters: &mut Vec<KernelInfo>,
    seen_inodes: &mut std::collections::HashSet<(u64, u64)>,
) {
    // Check WORKON_HOME environment variable
    let workon_home = std::env::var("WORKON_HOME")
        .ok()
        .or_else(|| {
            // Default location for virtualenvwrapper
            std::env::var("HOME").ok().map(|h| format!("{}/.virtualenvs", h))
        });

    if let Some(workon_home) = workon_home {
        if let Ok(entries) = std::fs::read_dir(&workon_home) {
            for entry in entries.flatten() {
                if entry.file_type().ok().map(|t| t.is_dir()).unwrap_or(false) {
                    let python_path = entry.path().join("bin/python");
                    if python_path.exists() {
                        // Don't canonicalize - preserve virtualenvwrapper path
                        add_interpreter(
                            interpreters,
                            seen_inodes,
                            python_path.to_string_lossy().to_string(),
                            &format!("python [{}]", entry.file_name().to_string_lossy()),
                        );
                    }
                }
            }
        }
    }
}

/// Check pyenv installations
fn check_pyenv(
    interpreters: &mut Vec<KernelInfo>,
    seen_inodes: &mut std::collections::HashSet<(u64, u64)>,
) {
    let pyenv_root = std::env::var("PYENV_ROOT")
        .ok()
        .or_else(|| std::env::var("HOME").ok().map(|h| format!("{}/.pyenv", h)));

    if let Some(pyenv_root) = pyenv_root {
        let versions_dir = format!("{}/versions", pyenv_root);
        if let Ok(entries) = std::fs::read_dir(&versions_dir) {
            for entry in entries.flatten() {
                if entry.file_type().ok().map(|t| t.is_dir()).unwrap_or(false) {
                    let python_path = entry.path().join("bin/python");
                    if python_path.exists() {
                        // Don't canonicalize - preserve pyenv path
                        add_interpreter(
                            interpreters,
                            seen_inodes,
                            python_path.to_string_lossy().to_string(),
                            &format!("python [pyenv:{}]", entry.file_name().to_string_lossy()),
                        );
                    }
                }
            }
        }
    }
}

/// Check conda environments
fn check_conda(
    interpreters: &mut Vec<KernelInfo>,
    seen_inodes: &mut std::collections::HashSet<(u64, u64)>,
) {
    // Try to run conda env list
    if let Ok(output) = std::process::Command::new("conda")
        .args(&["env", "list", "--json"])
        .output()
    {
        if output.status.success() {
            if let Ok(data) = serde_json::from_slice::<serde_json::Value>(&output.stdout) {
                if let Some(envs) = data.get("envs").and_then(|e| e.as_array()) {
                    for env_path in envs {
                        if let Some(env_str) = env_path.as_str() {
                            let python_path = format!("{}/bin/python", env_str);
                            if std::path::Path::new(&python_path).exists() {
                                let env_name = std::path::Path::new(env_str)
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("conda");
                                // Don't canonicalize - preserve conda path
                                add_interpreter(
                                    interpreters,
                                    seen_inodes,
                                    python_path,
                                    &format!("python [conda:{}]", env_name),
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Check common system directories
fn check_system_paths(
    interpreters: &mut Vec<KernelInfo>,
    seen_inodes: &mut std::collections::HashSet<(u64, u64)>,
) {
    let system_dirs = vec![
        "/usr/bin",
        "/usr/local/bin",
        "/usr/sbin",
        "/sbin",
        "/bin",
        "/opt/homebrew/bin",
        "/usr/local/Cellar/python*/*/bin",
    ];

    // Check common names first, then dynamically discover versioned pythons
    let priority_names = vec!["python3", "python"];

    for dir in system_dirs {
        // Handle glob patterns (e.g., homebrew cellar)
        if dir.contains('*') {
            if let Ok(paths) = glob::glob(dir) {
                for path in paths.flatten() {
                    check_pythons_in_dir(&path, interpreters, seen_inodes);
                }
            }
        } else {
            check_pythons_in_dir(std::path::Path::new(dir), interpreters, seen_inodes);
        }
    }
}

/// Check for Python interpreters in a specific directory
fn check_pythons_in_dir(
    dir: &std::path::Path,
    interpreters: &mut Vec<KernelInfo>,
    seen_inodes: &mut std::collections::HashSet<(u64, u64)>,
) {
    if !dir.exists() {
        return;
    }

    // Collect all python* executables first
    let mut python_paths = Vec::new();

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let name_str = file_name.to_string_lossy();

            // Match actual Python interpreters, not tools like python3-config, python3-coverage
            let is_python = if let Some(rest) = name_str.strip_prefix("python") {
                // After "python", must be: empty OR only digits and dots (version number)
                // NOT dashes, letters, or other chars (e.g., python3.12-config has dash)
                rest.is_empty() ||
                rest.chars().all(|c| c.is_ascii_digit() || c == '.')
            } else if let Some(rest) = name_str.strip_prefix("pypy") {
                // Same for pypy
                rest.is_empty() ||
                rest.chars().all(|c| c.is_ascii_digit() || c == '.')
            } else {
                false
            };

            if is_python {
                let python_path = entry.path();
                // Check it's an executable file (or symlink to one)
                if python_path.is_file() || python_path.is_symlink() {
                    python_paths.push((python_path, name_str.to_string()));
                }
            }
        }
    }

    // Sort to prioritize common names first: python3, python, then versioned, then others
    python_paths.sort_by_key(|(_, name)| {
        match name.as_str() {
            "python3" => 0,
            "python" => 1,
            "python2" => 2,
            s if s.starts_with("python3.") => 3,
            s if s.starts_with("python4.") => 4,
            s if s.starts_with("python2.") => 5,
            "pypy3" => 6,
            "pypy" => 7,
            _ => 8,
        }
    });

    // Add them in priority order
    for (python_path, name) in python_paths {
        add_interpreter(
            interpreters,
            seen_inodes,
            python_path.to_string_lossy().to_string(),
            &name,
        );
    }
}

/// Check PATH environment variable for Python interpreters
fn check_path_pythons(
    interpreters: &mut Vec<KernelInfo>,
    seen_inodes: &mut std::collections::HashSet<(u64, u64)>,
) {
    // Check common python names in PATH (system dirs already scanned all python* files)
    // This catches pythons in non-standard locations that are in PATH
    let priority_names = vec!["python3", "python", "python2", "pypy3", "pypy"];

    for name in priority_names {
        if let Ok(output) = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {}", name))
            .output()
        {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !path.is_empty() {
                    add_interpreter(interpreters, seen_inodes, path, name);
                }
            }
        }
    }
}

/// Find Jupyter kernels
fn discover_jupyter_kernels() -> Vec<KernelInfo> {
    let mut kernels = Vec::new();

    // Try to run `jupyter kernelspec list`
    if let Ok(output) = std::process::Command::new("jupyter")
        .args(&["kernelspec", "list", "--json"])
        .output()
    {
        if output.status.success() {
            if let Ok(data) = serde_json::from_slice::<serde_json::Value>(&output.stdout) {
                if let Some(kernelspecs) = data.get("kernelspecs").and_then(|k| k.as_object()) {
                    for (name, spec) in kernelspecs {
                        if let Some(spec_obj) = spec.as_object() {
                            if let Some(resource_dir) = spec_obj.get("resource_dir")
                                .and_then(|v| v.as_str())
                            {
                                // Read kernel.json to get display name
                                let kernel_json_path = format!("{}/kernel.json", resource_dir);
                                if let Ok(kernel_json) = std::fs::read_to_string(&kernel_json_path) {
                                    if let Ok(kernel_data) = serde_json::from_str::<serde_json::Value>(&kernel_json) {
                                        let display_name = kernel_data.get("display_name")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or(name)
                                            .to_string();

                                        kernels.push(KernelInfo {
                                            name: name.clone(),
                                            display_name: format!("{} (Jupyter)", display_name),
                                            python_path: resource_dir.to_string(),
                                            kernel_type: KernelType::Jupyter,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    kernels
}
