//! System tools the agent can call.
//!
//! Policy: read-only tools run without asking. Anything that can change the
//! system (`run_command`) needs the user's approval for each call; `agent`
//! enforces that before calling `execute`.

use serde_json::{json, Value};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const MAX_OUTPUT: usize = 16_000;
const MAX_FILE_BYTES: u64 = 64 * 1024;
const MAX_DIR_ENTRIES: usize = 500;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, PartialEq)]
pub enum Access {
    ReadOnly,
    NeedsApproval,
}

pub fn access(name: &str) -> Access {
    match name {
        "read_file" | "list_dir" | "system_info" => Access::ReadOnly,
        _ => Access::NeedsApproval,
    }
}

/// Tool definitions in Claude API format.
pub fn definitions() -> Value {
    json!([
        {
            "name": "run_command",
            "description": "Run a shell command (/bin/sh -c) on this machine as root and return its exit code, stdout and stderr. The user must approve every call, so put the reason in `reason`. Times out after 60 s; stdin is empty, so interactive programs will not work. Output is truncated to 16000 characters.",
            "strict": true,
            "input_schema": {
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "The command line to run."},
                    "reason": {"type": "string", "description": "One short sentence, shown to the user, saying why this command is needed."}
                },
                "required": ["command", "reason"],
                "additionalProperties": false
            }
        },
        {
            "name": "read_file",
            "description": "Read a text file (first 64 KiB). Relative paths resolve against the user's working directory. Runs without approval.",
            "strict": true,
            "input_schema": {
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
                "additionalProperties": false
            }
        },
        {
            "name": "list_dir",
            "description": "List a directory: one entry per line with type (d/f/l), size in bytes and name. Relative paths resolve against the user's working directory. Runs without approval.",
            "strict": true,
            "input_schema": {
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
                "additionalProperties": false
            }
        },
        {
            "name": "system_info",
            "description": "Kernel version, hostname, uptime, load, memory and process count, read from /proc. Runs without approval.",
            "strict": true,
            "input_schema": {"type": "object", "properties": {}, "required": [], "additionalProperties": false}
        }
    ])
}

fn str_arg<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input.get(key).and_then(Value::as_str).ok_or_else(|| format!("missing string argument `{key}`"))
}

fn resolve(cwd: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) }
}

/// One-line, human-readable description of a call, for the console.
pub fn describe(name: &str, input: &Value) -> String {
    match name {
        "run_command" => input["command"].as_str().unwrap_or("?").to_string(),
        "read_file" | "list_dir" => input["path"].as_str().unwrap_or("?").to_string(),
        _ => String::new(),
    }
}

pub fn truncate(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut cut = max;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push_str("\n[output truncated]");
    }
    s
}

/// Run a tool. Returns the result text and whether it is an error.
pub fn execute(name: &str, input: &Value, cwd: &Path) -> (String, bool) {
    let result = match name {
        "run_command" => str_arg(input, "command").and_then(|c| run_command(c, cwd)),
        "read_file" => str_arg(input, "path").and_then(|p| read_file(&resolve(cwd, p))),
        "list_dir" => str_arg(input, "path").and_then(|p| list_dir(&resolve(cwd, p))),
        "system_info" => Ok(system_info()),
        other => Err(format!("unknown tool `{other}`")),
    };
    match result {
        Ok(text) => (truncate(text, MAX_OUTPUT), false),
        Err(e) => (e, true),
    }
}

fn run_command(command: &str, cwd: &Path) -> Result<String, String> {
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to start /bin/sh: {e}"))?;

    // Drain pipes on threads so a chatty command cannot block on a full pipe.
    let mut out = child.stdout.take().expect("piped");
    let mut err = child.stderr.take().expect("piped");
    let out_t = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b);
        b
    });
    let err_t = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = err.read_to_end(&mut b);
        b
    });

    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if start.elapsed() > COMMAND_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(format!("wait failed: {e}")),
        }
    };
    let stdout = String::from_utf8_lossy(&out_t.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&err_t.join().unwrap_or_default()).into_owned();
    let code = match status {
        Some(s) => s.code().map_or_else(|| "killed by signal".to_string(), |c| c.to_string()),
        None => format!("timed out after {} s, killed", COMMAND_TIMEOUT.as_secs()),
    };
    Ok(format!("exit: {code}\nstdout:\n{stdout}\nstderr:\n{stderr}"))
}

fn read_file(path: &Path) -> Result<String, String> {
    let f = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut buf = Vec::new();
    f.take(MAX_FILE_BYTES).read_to_end(&mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn list_dir(path: &Path) -> Result<String, String> {
    let mut rows = Vec::new();
    for entry in fs::read_dir(path).map_err(|e| format!("{}: {e}", path.display()))? {
        let Ok(entry) = entry else { continue };
        let Ok(meta) = entry.metadata() else { continue };
        let kind = if meta.is_dir() { 'd' } else if meta.file_type().is_symlink() { 'l' } else { 'f' };
        rows.push((entry.file_name().to_string_lossy().into_owned(), kind, meta.len()));
    }
    rows.sort();
    let total = rows.len();
    rows.truncate(MAX_DIR_ENTRIES);
    let mut text = rows.iter().map(|(name, kind, size)| format!("{kind} {size:>10} {name}")).collect::<Vec<_>>().join("\n");
    if total > MAX_DIR_ENTRIES {
        text.push_str(&format!("\n[{} more entries not shown]", total - MAX_DIR_ENTRIES));
    }
    if total == 0 {
        text = "(empty directory)".into();
    }
    Ok(text)
}

fn proc_read(path: &str) -> String {
    fs::read_to_string(path).map(|s| s.trim().to_string()).unwrap_or_else(|_| "unknown".into())
}

fn system_info() -> String {
    let meminfo = proc_read("/proc/meminfo");
    let mem = |key: &str| {
        meminfo
            .lines()
            .find(|l| l.starts_with(key))
            .map(|l| l[key.len()..].trim().to_string())
            .unwrap_or_else(|| "unknown".into())
    };
    let uptime = proc_read("/proc/uptime");
    let uptime_s = uptime.split_whitespace().next().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
    let procs = fs::read_dir("/proc")
        .map(|d| d.flatten().filter(|e| e.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit())).count())
        .unwrap_or(0);
    format!(
        "hostname: {}\nkernel: {}\nuptime: {:.0} s\nload (1/5/15 min, runnable/total, last pid): {}\nmemory total: {}\nmemory available: {}\nprocesses: {}",
        proc_read("/proc/sys/kernel/hostname"),
        proc_read("/proc/version"),
        uptime_s,
        proc_read("/proc/loadavg"),
        mem("MemTotal:"),
        mem("MemAvailable:"),
        procs
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy() {
        assert_eq!(access("read_file"), Access::ReadOnly);
        assert_eq!(access("run_command"), Access::NeedsApproval);
        assert_eq!(access("something_new"), Access::NeedsApproval);
    }

    #[test]
    fn runs_commands_and_reports_exit_code() {
        let (out, err) = execute("run_command", &json!({"command": "echo hi; echo oops >&2; exit 3", "reason": "t"}), Path::new("/"));
        assert!(!err);
        assert!(out.starts_with("exit: 3\n"), "{out}");
        assert!(out.contains("hi") && out.contains("oops"));
    }

    #[test]
    fn lists_and_reads_relative_to_cwd() {
        let dir = std::env::temp_dir().join(format!("ruos-tools-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("note.txt"), "hello").unwrap();
        let (listing, _) = execute("list_dir", &json!({"path": "."}), &dir);
        assert!(listing.contains("note.txt"), "{listing}");
        assert_eq!(execute("read_file", &json!({"path": "note.txt"}), &dir), ("hello".to_string(), false));
        assert!(execute("read_file", &json!({"path": "missing"}), &dir).1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn truncates_on_char_boundary() {
        let t = truncate("ééé".to_string(), 3);
        assert!(t.starts_with('é') && t.ends_with("[output truncated]"));
    }
}
