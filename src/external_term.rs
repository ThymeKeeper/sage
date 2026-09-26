//! Launching a program in its own terminal window.
//!
//! sage holds the terminal in raw mode for the whole session, so a script that
//! wants to talk to the user — `input()`, a `termios` key reader, `curses` —
//! cannot be given a usable tty by either execution lane (see
//! [`crate::cell::is_interactive_program`]). Rather than run it into a null
//! stdin and report the wreckage afterwards, the host hands it to a terminal
//! emulator, which gives it a real pty of its own.
//!
//! The program is written to a temp file and run through a small wrapper
//! script, so the window stays open on exit long enough to read the output (or
//! a traceback). The launch is fire-and-forget: sage never blocks on it, and
//! the session keeps its kernel.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Write `source` to temp files and return the path to hand the interpreter.
/// Shared by both out-of-process lanes so their scratch files are named alike.
///
/// `origin` is the buffer's own file, when it has one. Running the raw temp
/// copy would give the program the temp file's identity — `__file__` in
/// `/tmp`, `sys.path[0]` pointing at the temp dir, so sibling imports and
/// relative paths resolve against the wrong directory. With an origin we run a
/// small bootstrap instead, which gives the code the identity it would have
/// had under a plain `python <origin>`. Without one (an unsaved buffer) there
/// is no identity to restore and the temp file is run directly.
pub fn write_program_temp(source: &str, origin: Option<&Path>) -> io::Result<PathBuf> {
    let path = temp_path("sage_app", "py");
    std::fs::File::create(&path)?.write_all(source.as_bytes())?;
    match origin {
        Some(file) => write_bootstrap(&path, file),
        None => Ok(path),
    }
}

/// Run `source` under `python_path` in a new terminal window. `origin` is the
/// buffer's file, as for [`write_program_temp`].
///
/// Returns the name of the terminal used, for the status line. `Err` means no
/// terminal could be launched (none installed, or no display to open one on) —
/// the caller falls back to the in-process lane, which at least surfaces the
/// script's output even though it can't interact with it.
pub fn launch_interactive(
    python_path: &str,
    source: &str,
    origin: Option<&Path>,
) -> Result<String, String> {
    let program = write_program_temp(source, origin).map_err(|e| format!("temp file: {}", e))?;
    // The Python bootstrap chdirs on its own; the wrapper needs no workdir.
    let wrapper =
        write_wrapper(python_path, &program, None).map_err(|e| format!("wrapper: {}", e))?;
    launch_wrapper(&wrapper)
}

/// Run `source` as a shell script under `shell_path` in a new terminal window.
/// The shell counterpart of [`launch_interactive`]: same wrapper, same
/// terminal discovery, but no bootstrap — the wrapper cds into `origin`'s
/// directory instead, so relative paths resolve as `./script.sh` would.
pub fn launch_interactive_shell(
    shell_path: &str,
    source: &str,
    origin: Option<&Path>,
) -> Result<String, String> {
    let path = temp_path("sage_app", "sh");
    std::fs::File::create(&path)
        .and_then(|mut f| f.write_all(source.as_bytes()))
        .map_err(|e| format!("temp file: {}", e))?;
    let workdir = origin
        .and_then(|p| std::fs::canonicalize(p).ok())
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));
    let wrapper = write_wrapper(shell_path, &path, workdir.as_deref())
        .map_err(|e| format!("wrapper: {}", e))?;
    launch_wrapper(&wrapper)
}

/// Write the bootstrap that runs `program` (the temp copy) as if it were
/// `origin`, and return its path.
///
/// It restores the four things a script gets for free when python runs it by
/// path — `__file__`, `sys.argv[0]`, `sys.path[0]`, and (sage's own choice)
/// the working directory, so relative data paths in the script resolve against
/// the file the user is editing rather than wherever sage happened to be
/// started. `compile()` is given the real path, so tracebacks name the user's
/// file; line numbers are the temp copy's, which are the cell's — the same
/// numbering the session lane reports. The bootstrap's own frame is stripped
/// from any traceback it prints, so a crash looks like a plain script crash.
fn write_bootstrap(program: &Path, origin: &Path) -> io::Result<PathBuf> {
    // An absolute origin: the child gets its own cwd, so a relative path
    // recorded here would resolve differently once it lands.
    let origin = std::fs::canonicalize(origin).unwrap_or_else(|_| origin.to_path_buf());

    let path = temp_path("sage_boot", "py");
    let body = format!(
        r#"# Written by sage; safe to delete.
import os
import sys

__sage_src = {src}
__sage_file = {file}
__sage_dir = os.path.dirname(__sage_file) or "."

with open(__sage_src, "r", encoding="utf-8") as __sage_f:
    __sage_code = __sage_f.read()

# Show the code that actually ran. Tracebacks name the real file, so python
# would otherwise render source lines from whatever is on disk — the wrong text
# when the buffer has unsaved edits, or when this is one cell out of many. An
# mtime of None marks the entry permanent, so checkcache() won't drop it.
import linecache
linecache.cache[__sage_file] = (
    len(__sage_code), None, __sage_code.splitlines(True), __sage_file
)

sys.argv[0] = __sage_file
sys.path.insert(0, __sage_dir)
try:
    os.chdir(__sage_dir)
except OSError:
    pass

__sage_globals = {{
    "__name__": "__main__",
    "__file__": __sage_file,
    "__builtins__": __builtins__,
    "__doc__": None,
    "__package__": None,
    "__spec__": None,
}}

try:
    exec(compile(__sage_code, __sage_file, "exec"), __sage_globals)
except SystemExit:
    raise
except BaseException:
    import traceback
    __sage_type, __sage_value, __sage_tb = sys.exc_info()
    # tb_next drops this file's exec frame, leaving only the user's code.
    traceback.print_exception(
        __sage_type, __sage_value, __sage_tb.tb_next if __sage_tb else __sage_tb
    )
    sys.exit(1)
"#,
        src = py_str_literal(&program.to_string_lossy()),
        file = py_str_literal(&origin.to_string_lossy()),
    );
    std::fs::File::create(&path)?.write_all(body.as_bytes())?;
    Ok(path)
}

/// `s` as a Python string literal. Paths reach this from the kernel list and
/// the open buffer, so backslashes (Windows) and quotes have to survive.
fn py_str_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// A temp path unique to this sage process and call.
fn temp_path(stem: &str, ext: &str) -> PathBuf {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("{}_{}_{}.{}", stem, std::process::id(), seq, ext))
}

/// The terminal the user asked for, if any: `$SAGE_TERMINAL` wins, then the
/// `terminal` key in the config file.
fn configured_terminal() -> Option<String> {
    if let Ok(t) = std::env::var("SAGE_TERMINAL") {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    crate::config::Config::load()
        .terminal
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

// ---------------------------------------------------------------------------
// Unix (Linux, BSD): a /bin/sh wrapper run by a terminal emulator
// ---------------------------------------------------------------------------

#[cfg(all(unix, not(target_os = "macos")))]
fn write_wrapper(python_path: &str, program: &Path, workdir: Option<&Path>) -> io::Result<PathBuf> {
    let path = temp_path("sage_run", "sh");
    let cd = match workdir {
        Some(dir) => format!("cd {}\n", sh_quote(&dir.to_string_lossy())),
        None => String::new(),
    };
    let body = format!(
        "#!/bin/sh\n\
         # Written by sage; safe to delete.\n\
         {cd}{py} {prog}\n\
         __sage_status=$?\n\
         printf '\\n[sage] exited with status %s. Press Enter to close.\\n' \"$__sage_status\"\n\
         read __sage_done\n",
        py = sh_quote(python_path),
        prog = sh_quote(&program.to_string_lossy()),
    );
    std::fs::File::create(&path)?.write_all(body.as_bytes())?;
    Ok(path)
}

/// Terminal emulators we know how to hand a command to, best-supported first,
/// with the arguments that must precede that command. The command is always
/// `/bin/sh <wrapper>` — never the wrapper alone — so neither its execute bit
/// nor its shebang has to be honoured by the emulator.
#[cfg(all(unix, not(target_os = "macos")))]
const UNIX_TERMINALS: &[(&str, &[&str])] = &[
    ("kitty", &[]),
    ("alacritty", &["-e"]),
    ("ghostty", &["-e"]),
    ("wezterm", &["start", "--"]),
    ("foot", &[]),
    ("gnome-terminal", &["--"]),
    ("konsole", &["-e"]),
    ("xfce4-terminal", &["-x"]),
    ("x-terminal-emulator", &["-e"]),
    ("xterm", &["-e"]),
];

#[cfg(all(unix, not(target_os = "macos")))]
fn launch_wrapper(wrapper: &Path) -> Result<String, String> {
    // A terminal emulator needs something to open a window on. Under a bare
    // console or a plain ssh session there is nothing, and every candidate
    // below would fail the same way — say so once, clearly.
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return Err("no graphical display to open a terminal on".to_string());
    }

    let configured = configured_terminal();
    let candidates: Vec<(&str, &[&str])> = match configured.as_deref() {
        // An explicitly configured terminal is the only one tried: falling
        // back past someone's stated choice would just be confusing.
        Some(name) => {
            let args = UNIX_TERMINALS
                .iter()
                .find(|(bin, _)| *bin == name)
                .map(|(_, args)| *args)
                .unwrap_or(&["-e"]);
            vec![(name, args)]
        }
        None => UNIX_TERMINALS.to_vec(),
    };

    let mut last_err = None;
    for (bin, prefix) in candidates {
        if !in_path(bin) {
            continue;
        }
        let mut cmd = Command::new(bin);
        cmd.args(prefix)
            .arg("/bin/sh")
            .arg(wrapper)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        match cmd.spawn() {
            Ok(child) => {
                reap(child);
                return Ok(bin.to_string());
            }
            Err(e) => last_err = Some(format!("{}: {}", bin, e)),
        }
    }

    Err(match (configured, last_err) {
        (Some(name), None) => format!("terminal '{}' not found on PATH", name),
        (_, Some(e)) => format!("could not start a terminal ({})", e),
        (None, None) => "no supported terminal emulator found on PATH".to_string(),
    })
}

/// True if `bin` resolves to an executable file on `$PATH`.
#[cfg(all(unix, not(target_os = "macos")))]
fn in_path(bin: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join(bin);
        std::fs::metadata(&candidate)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    })
}

// ---------------------------------------------------------------------------
// macOS: a .command file opened by Terminal.app (or whatever `open -a` names)
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn write_wrapper(python_path: &str, program: &Path, workdir: Option<&Path>) -> io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    // Terminal.app runs the file itself, so it must be executable and carry a
    // .command extension.
    let path = temp_path("sage_run", "command");
    let cd = match workdir {
        Some(dir) => format!("cd {}\n", sh_quote(&dir.to_string_lossy())),
        None => String::new(),
    };
    let body = format!(
        "#!/bin/sh\n\
         # Written by sage; safe to delete.\n\
         {cd}{py} {prog}\n\
         __sage_status=$?\n\
         printf '\\n[sage] exited with status %s. Press Enter to close.\\n' \"$__sage_status\"\n\
         read __sage_done\n",
        py = sh_quote(python_path),
        prog = sh_quote(&program.to_string_lossy()),
    );
    std::fs::File::create(&path)?.write_all(body.as_bytes())?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    Ok(path)
}

#[cfg(target_os = "macos")]
fn launch_wrapper(wrapper: &Path) -> Result<String, String> {
    let app = configured_terminal().unwrap_or_else(|| "Terminal".to_string());
    let mut cmd = Command::new("open");
    cmd.arg("-a")
        .arg(&app)
        .arg(wrapper)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match cmd.spawn() {
        Ok(child) => {
            reap(child);
            Ok(app)
        }
        Err(e) => Err(format!("could not open {}: {}", app, e)),
    }
}

// ---------------------------------------------------------------------------
// Windows: a .bat wrapper started in its own console window
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn write_wrapper(python_path: &str, program: &Path, workdir: Option<&Path>) -> io::Result<PathBuf> {
    let path = temp_path("sage_run", "bat");
    let cd = match workdir {
        Some(dir) => format!("cd /d \"{}\"\r\n", dir.display()),
        None => String::new(),
    };
    let body = format!(
        "@echo off\r\n\
         rem Written by sage; safe to delete.\r\n\
         {cd}\"{py}\" \"{prog}\"\r\n\
         echo.\r\n\
         echo [sage] exited with status %ERRORLEVEL%. Press any key to close.\r\n\
         pause >nul\r\n",
        py = python_path,
        prog = program.display(),
    );
    std::fs::File::create(&path)?.write_all(body.as_bytes())?;
    Ok(path)
}

#[cfg(windows)]
fn launch_wrapper(wrapper: &Path) -> Result<String, String> {
    // `start` is a cmd builtin, so it needs a shell. Its first quoted argument
    // is the new window's title, not a command — omitting it would make the
    // wrapper path the title and nothing would run.
    let mut cmd = Command::new("cmd");
    cmd.arg("/C")
        .arg("start")
        .arg("sage")
        .arg("cmd")
        .arg("/C")
        .arg(wrapper)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match cmd.spawn() {
        Ok(child) => {
            reap(child);
            Ok("console window".to_string())
        }
        Err(e) => Err(format!("could not start a console window: {}", e)),
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Wait on the launcher process off-thread. It exits as soon as the terminal
/// is up (the window itself is not our child), so this is only about not
/// leaving a zombie behind for the rest of the session.
#[allow(dead_code)]
fn reap(mut child: std::process::Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

/// Single-quote `s` for /bin/sh, closing and reopening the quotes around any
/// embedded quote. Temp paths are tame, but the interpreter path comes from
/// the kernel list and can be anything on disk.
#[cfg(unix)]
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn sh_quote_survives_quotes_and_spaces() {
        assert_eq!(sh_quote("/usr/bin/python3"), "'/usr/bin/python3'");
        assert_eq!(sh_quote("/opt/my envs/py"), "'/opt/my envs/py'");
        assert_eq!(sh_quote("/it's/python"), r#"'/it'\''s/python'"#);
    }

    #[test]
    fn temp_paths_are_unique() {
        let a = temp_path("sage_test", "py");
        let b = temp_path("sage_test", "py");
        assert_ne!(a, b);
    }

    #[test]
    fn py_str_literal_escapes_backslashes_and_quotes() {
        assert_eq!(py_str_literal("/home/a/b.py"), "\"/home/a/b.py\"");
        assert_eq!(py_str_literal(r"C:\tmp\x.py"), r#""C:\\tmp\\x.py""#);
        assert_eq!(py_str_literal("say \"hi\""), r#""say \"hi\"""#);
    }

    #[test]
    fn bootstrap_restores_the_origin_file_identity() {
        let program = write_program_temp("print(__file__)\n", Some(Path::new("/home/a/real.py")))
            .unwrap();
        let body = std::fs::read_to_string(&program).unwrap();
        // The interpreter is handed the bootstrap, not the raw copy.
        assert!(program.to_string_lossy().contains("sage_boot"));
        assert!(body.contains("\"__file__\": __sage_file"));
        assert!(body.contains("/home/a/real.py"));
        assert!(body.contains("sys.path.insert(0, __sage_dir)"));
        assert!(body.contains("os.chdir(__sage_dir)"));
        // Unsaved edits mean the file on disk isn't what ran; the traceback
        // must still show the real lines.
        assert!(body.contains("linecache.cache[__sage_file]"));
        // Tracebacks must name the user's file, not the temp copy.
        assert!(body.contains(r#"compile(__sage_code, __sage_file, "exec")"#));
        let _ = std::fs::remove_file(program);
    }

    #[test]
    fn unsaved_buffer_runs_the_temp_copy_directly() {
        let program = write_program_temp("print('hi')\n", None).unwrap();
        assert!(program.to_string_lossy().contains("sage_app"));
        assert_eq!(std::fs::read_to_string(&program).unwrap(), "print('hi')\n");
        let _ = std::fs::remove_file(program);
    }

    #[test]
    fn wrapper_runs_the_program_under_the_interpreter() {
        let program = write_program_temp("print('hi')\n", None).unwrap();
        let wrapper = write_wrapper("/usr/bin/python3", &program, None).unwrap();
        let body = std::fs::read_to_string(&wrapper).unwrap();
        assert!(body.contains("/usr/bin/python3"));
        assert!(body.contains(&program.to_string_lossy().to_string()));
        // The window must outlive the program, or output flashes past.
        assert!(body.contains("Press Enter to close") || body.contains("Press any key to close"));
        let _ = std::fs::remove_file(program);
        let _ = std::fs::remove_file(wrapper);
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn wrapper_cds_into_the_workdir_when_given_one() {
        let program = temp_path("sage_test", "sh");
        std::fs::write(&program, "echo hi\n").unwrap();
        let wrapper =
            write_wrapper("/bin/bash", &program, Some(Path::new("/home/a b/proj"))).unwrap();
        let body = std::fs::read_to_string(&wrapper).unwrap();
        assert!(body.contains("cd '/home/a b/proj'"));
        assert!(body.contains("/bin/bash"));
        let _ = std::fs::remove_file(program);
        let _ = std::fs::remove_file(wrapper);
    }
}


