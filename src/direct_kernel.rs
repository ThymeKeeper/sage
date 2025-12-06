use crate::kernel::{ExecutionOutput, ExecutionResult, Kernel, KernelInfo, KernelType};
use std::error::Error;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// Direct Python kernel using subprocess communication
pub struct DirectKernel {
    info: KernelInfo,
    process: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: Option<BufReader<ChildStdout>>,
    execution_count: usize,
}

impl DirectKernel {
    pub fn new(python_path: String, name: String, display_name: String) -> Self {
        DirectKernel {
            info: KernelInfo {
                name,
                display_name,
                python_path,
                kernel_type: KernelType::Direct,
            },
            process: None,
            stdin: None,
            stdout: None,
            execution_count: 0,
        }
    }

    /// Create a Python REPL script that handles execution
    fn get_repl_script() -> &'static str {
        r#"
import sys
import traceback
import json
import os
import io
import contextlib

# Ensure we're not in interactive mode
sys.ps1 = sys.ps2 = ''

# Disable output buffering (handle older Python versions)
try:
    sys.stdout.reconfigure(line_buffering=True)
    sys.stderr.reconfigure(line_buffering=True)
except (AttributeError, OSError):
    # Python < 3.7 or when reconfigure fails
    pass

# Ensure TERM is set to dumb to avoid escape codes
os.environ['TERM'] = 'dumb'

print("SAGE_KERNEL_READY", flush=True)

while True:
    try:
        # Read delimiter
        line = input()
        if line != "SAGE_EXEC_START":
            continue

        # Read code until END delimiter
        code_lines = []
        while True:
            line = input()
            if line == "SAGE_EXEC_END":
                break
            code_lines.append(line)

        code = '\n'.join(code_lines)

        # Execute code with stdout capture
        # Use Jupyter-style execution: try eval, then try exec with last expression
        stdout_capture = io.StringIO()
        _sage_result = None

        try:
            # First, try to eval the entire code (for simple expressions)
            with contextlib.redirect_stdout(stdout_capture):
                _sage_result = eval(code, globals())
        except SyntaxError:
            # If eval fails, try exec with smart handling of last expression
            try:
                # Split code into statements
                lines = [line for line in code_lines if line.strip()]

                if lines:
                    # Try to exec all but last line, then eval the last line
                    if len(lines) > 1:
                        # Execute all but the last line
                        prefix_code = '\n'.join(lines[:-1])
                        with contextlib.redirect_stdout(stdout_capture):
                            exec(prefix_code, globals())

                        # Try to eval the last line to get result
                        try:
                            with contextlib.redirect_stdout(stdout_capture):
                                _sage_result = eval(lines[-1], globals())
                        except SyntaxError:
                            # Last line is not an expression, just exec it
                            with contextlib.redirect_stdout(stdout_capture):
                                exec(lines[-1], globals())
                    else:
                        # Single line that's not an expression, just exec it
                        with contextlib.redirect_stdout(stdout_capture):
                            exec(code, globals())
                else:
                    # Empty code
                    pass
            except Exception as e:
                # Re-raise to be caught by outer exception handler
                raise

        # Send captured stdout if any
        captured = stdout_capture.getvalue()
        if captured:
            print("SAGE_OUTPUT_START", flush=True)
            print(json.dumps({"type": "stdout", "data": captured}), flush=True)
            print("SAGE_OUTPUT_END", flush=True)

        # Send result (only if not None, matching Jupyter behavior)
        if _sage_result is not None:
            # Format result in a Jupyter-like way
            try:
                # Import pprint for better formatting
                import pprint

                # Use a more intelligent formatting strategy
                if isinstance(_sage_result, str):
                    # For strings, use repr to show quotes
                    formatted = repr(_sage_result)
                elif isinstance(_sage_result, (list, dict, tuple, set)):
                    # For collections, use pprint for nice formatting
                    formatted = pprint.pformat(_sage_result, width=80, compact=True)
                else:
                    # For other types, try repr first, fallback to str
                    formatted = repr(_sage_result)
            except Exception:
                # If formatting fails, use str as last resort
                formatted = str(_sage_result)

            print("SAGE_OUTPUT_START", flush=True)
            print(json.dumps({"type": "result", "data": formatted}), flush=True)
            print("SAGE_OUTPUT_END", flush=True)
        else:
            # No result to show (None result) - just signal success
            print("SAGE_OUTPUT_START", flush=True)
            print(json.dumps({"type": "success"}), flush=True)
            print("SAGE_OUTPUT_END", flush=True)
    except Exception as e:
        print("SAGE_OUTPUT_START", flush=True)
        error_data = {
            "type": "error",
            "ename": type(e).__name__,
            "evalue": str(e),
            "traceback": traceback.format_exc().split('\n')
        }
        print(json.dumps(error_data), flush=True)
        print("SAGE_OUTPUT_END", flush=True)
    except EOFError:
        break
    except Exception as e:
        print(f"REPL Error: {e}", file=sys.stderr, flush=True)
        break
"#
    }
}

impl Kernel for DirectKernel {
    fn connect(&mut self) -> Result<(), Box<dyn Error>> {
        if self.is_connected() {
            return Ok(());
        }

        // Start Python process with our REPL script
        // Set TERM to dumb to avoid escape codes, and clear terminal-related env vars
        let mut child = Command::new(&self.info.python_path)
            .arg("-u") // Unbuffered output
            .arg("-c")
            .arg(Self::get_repl_script())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("TERM", "dumb")  // Prevent terminal control codes
            .env_remove("TERM_PROGRAM")  // Remove any terminal program settings
            .spawn()
            .map_err(|e| format!("Failed to spawn Python process: {}", e))?;

        let stdin = child.stdin.take().ok_or("Failed to get stdin")?;
        let stdout = child.stdout.take().ok_or("Failed to get stdout")?;
        let mut stderr = child.stderr.take().ok_or("Failed to get stderr")?;

        // Wait for ready signal with timeout
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();

        // Try to read the ready signal
        match reader.read_line(&mut line) {
            Ok(0) => {
                // EOF - process probably died, check stderr
                let mut error_msg = String::new();
                std::io::Read::read_to_string(&mut stderr, &mut error_msg).ok();
                return Err(format!("Python process died immediately. Error: {}", error_msg).into());
            }
            Ok(_) => {
                if !line.trim().starts_with("SAGE_KERNEL_READY") {
                    // Got unexpected output, check stderr too
                    let mut error_msg = String::new();
                    std::io::Read::read_to_string(&mut stderr, &mut error_msg).ok();
                    return Err(format!(
                        "Kernel failed to start. Got: '{}'. Error: {}",
                        line.trim(),
                        error_msg
                    ).into());
                }
            }
            Err(e) => {
                let mut error_msg = String::new();
                std::io::Read::read_to_string(&mut stderr, &mut error_msg).ok();
                return Err(format!("Failed to read from Python: {}. Error: {}", e, error_msg).into());
            }
        }

        // Store process handle, stdin, and stdout reader
        self.stdin = Some(stdin);
        self.stdout = Some(reader);
        self.process = Some(child);

        Ok(())
    }

    fn execute(&mut self, code: &str) -> Result<ExecutionResult, Box<dyn Error>> {
        if !self.is_connected() {
            return Err("Kernel not connected".into());
        }

        self.execution_count += 1;

        let stdin = self.stdin.as_mut().ok_or("No stdin available")?;
        let reader = self.stdout.as_mut().ok_or("No stdout available")?;

        // Send execution delimiters and code
        writeln!(stdin, "SAGE_EXEC_START")?;
        for line in code.lines() {
            writeln!(stdin, "{}", line)?;
        }
        writeln!(stdin, "SAGE_EXEC_END")?;
        stdin.flush()?;

        // Read outputs - there can be multiple output blocks (stdout, result, etc)
        let mut outputs = Vec::new();
        let mut success = false;
        let mut finished = false;
        let mut line = String::new();

        while !finished {
            // Wait for output start marker
            loop {
                line.clear();
                reader.read_line(&mut line)?;
                if line.trim() == "SAGE_OUTPUT_START" {
                    break;
                }
            }

            // Read JSON output
            line.clear();
            reader.read_line(&mut line)?;

            let output_data: serde_json::Value = serde_json::from_str(line.trim())?;

            match output_data["type"].as_str() {
                Some("stdout") => {
                    if let Some(data) = output_data["data"].as_str() {
                        outputs.push(ExecutionOutput::Stdout(data.to_string()));
                    }
                }
                Some("result") => {
                    if let Some(data) = output_data["data"].as_str() {
                        outputs.push(ExecutionOutput::Result(data.to_string()));
                    }
                    success = true;
                    finished = true;
                }
                Some("success") => {
                    success = true;
                    finished = true;
                }
                Some("error") => {
                    let ename = output_data["ename"].as_str().unwrap_or("Error").to_string();
                    let evalue = output_data["evalue"].as_str().unwrap_or("").to_string();
                    let traceback = output_data["traceback"]
                        .as_array()
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();

                    outputs.push(ExecutionOutput::Error {
                        ename,
                        evalue,
                        traceback,
                    });
                    success = false;
                    finished = true;
                }
                _ => {
                    finished = true;
                }
            }

            // Wait for output end marker
            line.clear();
            reader.read_line(&mut line)?;
        }

        Ok(ExecutionResult {
            outputs,
            execution_count: Some(self.execution_count),
            success,
        })
    }

    fn disconnect(&mut self) -> Result<(), Box<dyn Error>> {
        // Drop stdin first to send EOF to the Python process
        self.stdin = None;
        self.stdout = None;

        if let Some(mut process) = self.process.take() {
            // Try a quick check if it exited
            if let Ok(Some(_)) = process.try_wait() {
                return Ok(()); // Already exited
            }

            // Otherwise kill it immediately (the EOF from closing stdin should have signaled it)
            let _ = process.kill();
            let _ = process.wait();
        }
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.process.is_some()
    }

    fn info(&self) -> KernelInfo {
        self.info.clone()
    }
}

impl Drop for DirectKernel {
    fn drop(&mut self) {
        let _ = self.disconnect();
    }
}
