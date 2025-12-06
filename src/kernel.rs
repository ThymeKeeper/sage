use serde::{Deserialize, Serialize};
use std::error::Error;

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

/// Execution result with combined output
#[derive(Debug, Clone)]
pub struct ExecutionResult {
    pub outputs: Vec<ExecutionOutput>,
    pub execution_count: Option<usize>,
    pub success: bool,
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

    // Add Jupyter kernels
    kernels.extend(discover_jupyter_kernels());

    kernels
}

/// Find Python interpreters on the system
fn discover_python_interpreters() -> Vec<KernelInfo> {
    let mut interpreters = Vec::new();
    let mut seen_paths = std::collections::HashSet::new();

    // Check common Python executable names
    let python_names = vec!["python3", "python", "python3.12", "python3.11", "python3.10", "python3.9"];

    for name in python_names {
        // Use command -v instead of which (more portable and faster)
        if let Ok(output) = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {}", name))
            .output()
        {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !path.is_empty() && !seen_paths.contains(&path) {
                    seen_paths.insert(path.clone());

                    // Get Python version
                    let version = std::process::Command::new(&path)
                        .arg("--version")
                        .output()
                        .ok()
                        .and_then(|o| String::from_utf8(o.stdout).ok())
                        .unwrap_or_default()
                        .trim()
                        .to_string();

                    // Detect if in virtualenv
                    let is_venv = path.contains("/venv/") ||
                                  path.contains("/.venv/") ||
                                  path.contains("/virtualenv/") ||
                                  path.contains("/env/");

                    let location_hint = if is_venv {
                        " [venv]"
                    } else if path.starts_with("/usr/local") {
                        " [local]"
                    } else if path.starts_with("/usr") {
                        " [system]"
                    } else {
                        ""
                    };

                    let display_name = if version.is_empty() {
                        format!("{}{} ({})", name, location_hint, path)
                    } else {
                        format!("{}{} - {} ({})", name, location_hint, version, path)
                    };

                    interpreters.push(KernelInfo {
                        name: name.to_string(),
                        display_name,
                        python_path: path,
                        kernel_type: KernelType::Direct,
                    });
                }
            }
        }
    }

    // Also check for python in current directory's venv
    let venv_paths = vec![".venv/bin/python", ".venv/bin/python3", "venv/bin/python", "venv/bin/python3"];
    for venv_path in venv_paths {
        if let Ok(absolute_path) = std::fs::canonicalize(venv_path) {
            let path = absolute_path.to_string_lossy().to_string();
            if !seen_paths.contains(&path) {
                seen_paths.insert(path.clone());

                let version = std::process::Command::new(&path)
                    .arg("--version")
                    .output()
                    .ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .unwrap_or_default()
                    .trim()
                    .to_string();

                let display_name = format!("python [local venv] - {} ({})", version, path);

                interpreters.push(KernelInfo {
                    name: venv_path.to_string(),
                    display_name,
                    python_path: path,
                    kernel_type: KernelType::Direct,
                });
            }
        }
    }

    interpreters
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
