//! A persistent shell session as a notebook kernel.
//!
//! Mirrors [`crate::direct_kernel::DirectKernel`]'s shape, but the protocol is
//! much smaller: each cell is written to a temp file and sourced (`.`) into a
//! long-lived shell, so variables, functions, and the working directory carry
//! from cell to cell just like a Python kernel's globals. Sourcing a file —
//! rather than piping the cell's text down stdin — is what keeps the protocol
//! safe: an unbalanced quote or open heredoc in the cell becomes a syntax
//! error confined to that file instead of swallowing the delimiter lines.
//!
//! The cell is sourced with stdin redirected from /dev/null, so a `read` that
//! slipped past the interactive-script detection fails on EOF immediately
//! instead of eating the next protocol line. Scripts that genuinely need a
//! user get routed to a real terminal window before they ever reach this
//! kernel — see [`crate::cell::is_interactive_shell`] and
//! [`crate::external_term::launch_interactive_shell`].

use crate::kernel::{
    CancelHandle, ExecutionOutput, ExecutionResult, Kernel, KernelInfo, SHELL_KERNEL_NAME,
};
use std::error::Error;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;

/// Process-global sequence for cell temp files. The pid alone isn't unique
/// enough: several kernels can live in one process (tests, future splits),
/// and per-kernel counters would collide on the same filename.
static CELL_SEQ: AtomicU64 = AtomicU64::new(0);

pub struct ShellKernel {
    info: KernelInfo,
    /// Directory the session starts in — the edited file's own directory, so
    /// relative paths in the script resolve the way `./script.sh` would.
    workdir: Option<PathBuf>,
    process: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: Option<BufReader<ChildStdout>>,
    /// Lines from the child's stderr, pumped by a dedicated thread so neither
    /// pipe can fill up and deadlock the other.
    stderr_rx: Option<Receiver<String>>,
    execution_count: usize,
}

impl ShellKernel {
    pub fn new(shell_path: String, display_name: String) -> Self {
        ShellKernel {
            info: KernelInfo {
                name: SHELL_KERNEL_NAME.to_string(),
                display_name,
                // The interpreter-path slot, same as DirectKernel uses it.
                python_path: shell_path,
            },
            workdir: None,
            process: None,
            stdin: None,
            stdout: None,
            stderr_rx: None,
            execution_count: 0,
        }
    }

    pub fn with_workdir(mut self, dir: Option<PathBuf>) -> Self {
        self.workdir = dir;
        self
    }

    /// Arguments that keep the session quiet and reproducible: no rc files, no
    /// prompts. Matched on the binary's basename so a full path still works.
    fn session_args(shell_path: &str) -> &'static [&'static str] {
        let base = std::path::Path::new(shell_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        match base {
            "bash" => &["--noprofile", "--norc"],
            "zsh" => &["-f"],
            _ => &[],
        }
    }

    fn spawn_session(&mut self) -> Result<(), Box<dyn Error>> {
        let mut cmd = Command::new(&self.info.python_path);
        cmd.args(Self::session_args(&self.info.python_path))
            .env("TERM", "dumb")
            .env("PS1", "")
            .env("PS2", "")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = &self.workdir {
            cmd.current_dir(dir);
        }
        let mut child = cmd.spawn()?;

        let stdin = child.stdin.take().ok_or("no stdin pipe")?;
        let stdout = BufReader::new(child.stdout.take().ok_or("no stdout pipe")?);
        let stderr = child.stderr.take().ok_or("no stderr pipe")?;

        let (tx, rx) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines() {
                match line {
                    Ok(l) => {
                        if tx.send(l).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        self.process = Some(child);
        self.stdin = Some(stdin);
        self.stdout = Some(stdout);
        self.stderr_rx = Some(rx);
        Ok(())
    }

    fn kill_session(&mut self) {
        self.stdin = None; // closing stdin lets a well-behaved shell exit
        self.stdout = None;
        self.stderr_rx = None;
        if let Some(mut child) = self.process.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// True if the child is still running. A cell that ran `exit` (or a shell
    /// that died on a sourced syntax error — some `sh` implementations treat
    /// those as fatal) leaves a corpse; execute() restarts before running.
    fn session_alive(&mut self) -> bool {
        match self.process.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }
}

impl Kernel for ShellKernel {
    fn connect(&mut self) -> Result<(), Box<dyn Error>> {
        if !std::path::Path::new(&self.info.python_path).exists() {
            return Err(format!("shell '{}' not found", self.info.python_path).into());
        }
        self.spawn_session()
    }

    fn execute(&mut self, code: &str) -> Result<ExecutionResult, Box<dyn Error>> {
        // A previous cell may have taken the session down (`exit`, a killed
        // process); start fresh rather than erroring at the user.
        let mut restarted = false;
        if !self.session_alive() {
            self.kill_session();
            self.spawn_session()?;
            restarted = self.execution_count > 0;
        }

        self.execution_count += 1;
        let nonce = format!(
            "__SAGE_SH_DONE_{}_{}__",
            std::process::id(),
            self.execution_count
        );

        // The cell goes into its own file so nothing in it can be mistaken for
        // protocol; the delimiter lines below are the only thing the session
        // reads from us afterwards.
        let cell_path = std::env::temp_dir().join(format!(
            "sage_cell_{}_{}.sh",
            std::process::id(),
            CELL_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let mut source = code.to_string();
        if !source.ends_with('\n') {
            source.push('\n');
        }
        std::fs::write(&cell_path, &source)?;

        {
            let stdin = self.stdin.as_mut().ok_or("shell session has no stdin")?;
            // stderr's delimiter goes out first so that once stdout's arrives,
            // stderr's is already in flight — the drain below never races.
            write!(
                stdin,
                ". {file} < /dev/null\n\
                 __sage_status=$?\n\
                 printf '%s\\n' '{nonce}' 1>&2\n\
                 printf '%s %s\\n' '{nonce}' \"$__sage_status\"\n",
                file = sh_quote(&cell_path.to_string_lossy()),
                nonce = nonce,
            )?;
            stdin.flush()?;
        }

        // Collect stdout until the delimiter carries the exit status back.
        let mut stdout_text = String::new();
        let mut status: Option<i32> = None;
        let mut died = false;
        {
            let stdout = self.stdout.as_mut().ok_or("shell session has no stdout")?;
            let mut line = String::new();
            loop {
                line.clear();
                match stdout.read_line(&mut line) {
                    Ok(0) => {
                        died = true;
                        break;
                    }
                    Ok(_) => {
                        let trimmed = line.trim_end_matches(['\r', '\n']);
                        if let Some(rest) = trimmed.strip_prefix(&nonce) {
                            status = rest.trim().parse::<i32>().ok();
                            break;
                        }
                        stdout_text.push_str(&line);
                    }
                    Err(e) => {
                        let _ = std::fs::remove_file(&cell_path);
                        return Err(e.into());
                    }
                }
            }
        }

        // Drain stderr up to its delimiter. When the session died the
        // delimiter never comes; take whatever the pump managed to read.
        let mut stderr_text = String::new();
        if let Some(rx) = &self.stderr_rx {
            let deadline = if died {
                Duration::from_millis(200)
            } else {
                Duration::from_secs(5)
            };
            loop {
                match rx.recv_timeout(deadline) {
                    Ok(l) if l.trim_end() == nonce => break,
                    Ok(l) => {
                        stderr_text.push_str(&l);
                        stderr_text.push('\n');
                    }
                    Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        }

        let _ = std::fs::remove_file(&cell_path);

        let mut outputs = Vec::new();
        if restarted {
            outputs.push(ExecutionOutput::Stderr(
                "[sage] previous cell ended the shell session — started a new one\n".to_string(),
            ));
        }
        if !stdout_text.is_empty() {
            outputs.push(ExecutionOutput::Stdout(stdout_text));
        }
        if !stderr_text.is_empty() {
            outputs.push(ExecutionOutput::Stderr(stderr_text));
        }

        let success = if died {
            // `exit 0` at the end of a script is normal; report the session's
            // own exit status rather than calling it an error.
            let code = self
                .process
                .as_mut()
                .and_then(|c| c.try_wait().ok().flatten())
                .and_then(|s| s.code());
            self.kill_session();
            match code {
                Some(0) | None => true,
                Some(c) => {
                    outputs.push(ExecutionOutput::Error {
                        ename: "exit status".to_string(),
                        evalue: c.to_string(),
                        traceback: Vec::new(),
                    });
                    false
                }
            }
        } else {
            match status {
                Some(0) | None => true,
                Some(c) => {
                    outputs.push(ExecutionOutput::Error {
                        ename: "exit status".to_string(),
                        evalue: c.to_string(),
                        traceback: Vec::new(),
                    });
                    false
                }
            }
        };

        Ok(ExecutionResult {
            outputs,
            execution_count: Some(self.execution_count),
            success,
            completions: Vec::new(),
            type_relationships: Default::default(),
            sql_metadata: Default::default(),
            result_columns: Vec::new(),
        })
    }

    fn disconnect(&mut self) -> Result<(), Box<dyn Error>> {
        self.kill_session();
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.process.is_some()
    }

    fn info(&self) -> KernelInfo {
        self.info.clone()
    }

    fn cancel_handle(&self) -> Option<Arc<dyn CancelHandle>> {
        self.process.as_ref().map(|c| {
            let h: Arc<dyn CancelHandle> =
                Arc::new(crate::direct_kernel::ProcessKillHandle::new(c.id()));
            h
        })
    }
}

impl Drop for ShellKernel {
    fn drop(&mut self) {
        self.kill_session();
    }
}

/// Single-quote `s` for the shell, same scheme as the external-terminal
/// wrapper's: close, escape the quote, reopen.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The shells we know how to drive, preferred first, as selectable kernels.
pub fn discover_shell_kernels() -> Vec<KernelInfo> {
    let mut kernels = Vec::new();
    for name in ["bash", "zsh", "sh"] {
        if let Some(path) = find_in_path(name) {
            kernels.push(KernelInfo {
                name: SHELL_KERNEL_NAME.to_string(),
                display_name: format!("Shell ({})", name),
                python_path: path,
            });
        }
    }
    kernels
}

/// The best shell for running a script when nothing more specific was asked
/// for: first entry of the discovery list.
pub fn default_shell_path() -> Option<String> {
    discover_shell_kernels().into_iter().next().map(|k| k.python_path)
}

/// Resolve `bin` against $PATH (and PATHEXT-style `.exe` on Windows),
/// returning the full path of the first executable hit.
fn find_in_path(bin: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(bin);
        if is_executable(&candidate) {
            return Some(candidate.to_string_lossy().into_owned());
        }
        #[cfg(windows)]
        {
            let exe = dir.join(format!("{}.exe", bin));
            if is_executable(&exe) {
                return Some(exe.to_string_lossy().into_owned());
            }
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn bash() -> Option<String> {
        find_in_path("bash").or_else(|| find_in_path("sh"))
    }

    #[cfg(unix)]
    #[test]
    fn state_persists_across_cells() {
        let Some(shell) = bash() else { return };
        let mut k = ShellKernel::new(shell, "Shell (test)".to_string());
        k.connect().unwrap();

        let r = k.execute("GREETING=hello\ncd /\n").unwrap();
        assert!(r.success, "setup cell failed: {:?}", r.outputs);

        let r = k.execute("echo \"$GREETING from $PWD\"").unwrap();
        assert!(r.success);
        let stdout = r
            .outputs
            .iter()
            .find_map(|o| match o {
                ExecutionOutput::Stdout(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_default();
        assert_eq!(stdout.trim(), "hello from /");
        k.disconnect().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn nonzero_exit_marks_the_cell_failed() {
        let Some(shell) = bash() else { return };
        let mut k = ShellKernel::new(shell, "Shell (test)".to_string());
        k.connect().unwrap();
        let r = k.execute("false").unwrap();
        assert!(!r.success);
        assert!(r.outputs.iter().any(|o| matches!(
            o,
            ExecutionOutput::Error { evalue, .. } if evalue == "1"
        )));
        k.disconnect().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn stderr_is_captured_separately() {
        let Some(shell) = bash() else { return };
        let mut k = ShellKernel::new(shell, "Shell (test)".to_string());
        k.connect().unwrap();
        let r = k.execute("echo out; echo err 1>&2").unwrap();
        assert!(r.success);
        let err = r
            .outputs
            .iter()
            .find_map(|o| match o {
                ExecutionOutput::Stderr(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_default();
        assert_eq!(err.trim(), "err");
        k.disconnect().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn exit_in_a_cell_restarts_the_session() {
        let Some(shell) = bash() else { return };
        let mut k = ShellKernel::new(shell, "Shell (test)".to_string());
        k.connect().unwrap();
        let r = k.execute("exit 0").unwrap();
        assert!(r.success, "clean exit is not an error: {:?}", r.outputs);
        // The next cell runs in a fresh session, with a note saying so.
        let r = k.execute("echo alive").unwrap();
        assert!(r.success);
        assert!(r.outputs.iter().any(|o| matches!(
            o,
            ExecutionOutput::Stdout(s) if s.trim() == "alive"
        )));
        assert!(r.outputs.iter().any(|o| matches!(
            o,
            ExecutionOutput::Stderr(s) if s.contains("new one")
        )));
        k.disconnect().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn read_fails_fast_instead_of_wedging_the_protocol() {
        let Some(shell) = bash() else { return };
        let mut k = ShellKernel::new(shell, "Shell (test)".to_string());
        k.connect().unwrap();
        // stdin is /dev/null for the sourced cell: `read` sees EOF at once
        // (leaving the variable empty) rather than eating the delimiter
        // lines. What matters is that execute() returns promptly at all —
        // a wedged protocol would hang this test forever.
        let r = k.execute("read answer\necho \"got:$answer\"").unwrap();
        assert!(r.outputs.iter().any(|o| matches!(
            o,
            ExecutionOutput::Stdout(s) if s.trim() == "got:"
        )));
        // A read that ends the cell makes the EOF visible as a failure.
        let r = k.execute("read answer").unwrap();
        assert!(!r.success);
        k.disconnect().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unbalanced_quote_is_a_syntax_error_not_a_hang() {
        let Some(shell) = bash() else { return };
        let mut k = ShellKernel::new(shell, "Shell (test)".to_string());
        k.connect().unwrap();
        let r = k.execute("echo 'oops").unwrap();
        assert!(!r.success);
        // And the session (or its replacement) still works.
        let r = k.execute("echo fine").unwrap();
        assert!(r.success);
        k.disconnect().unwrap();
    }
}
