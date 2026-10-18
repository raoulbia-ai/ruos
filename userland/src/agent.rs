//! The agent loop: send the conversation to the provider, run the tools it
//! asks for (asking the user first where policy requires), feed the results
//! back, repeat until the model ends its turn.

use crate::proto::Event;
use crate::provider::Provider;
use crate::tools::{self, Access};
use serde_json::{json, Value};
use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_STEPS: usize = 25;

pub const SYSTEM_PROMPT: &str = "\
You are ruosd, the system agent built into ruOS, a minimal Linux operating system \
(Linux kernel, busybox userland, everything runs as root). The user talks to you \
from a text console through the `ai` command.

How to work:
- Use the tools to look at the real system instead of guessing. Read-only tools \
(read_file, list_dir, system_info) run immediately. run_command needs the user's \
approval every time, so use it only when needed and give a short, honest reason.
- Prefer one command that does the job over many small ones. Never run anything \
destructive the user did not clearly ask for.
- The userland is busybox: GNU-only flags may not exist. There is no package manager yet.
- Tool output is already shown to the user; do not repeat it. Answer in a few \
short lines of plain text for an 80-column terminal, no markdown headings or tables.";

/// How the agent talks to whoever is on the other end of the connection.
pub trait SessionIo {
    fn emit(&mut self, event: Event) -> io::Result<()>;
    /// Ask the user to approve a command. `Ok(false)` means declined.
    fn confirm(&mut self, command: &str, reason: &str) -> io::Result<bool>;
}

#[derive(Default)]
pub struct Session {
    pub messages: Vec<Value>,
}

fn audit(entry: Value) {
    let path = std::env::var("RUOS_AUDIT_LOG").unwrap_or_else(|_| "/var/log/ruos-audit.log".into());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let ts = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let mut entry = entry;
        entry["ts"] = json!(ts);
        let _ = writeln!(f, "{entry}");
    }
}

impl Session {
    /// Handle one user prompt. On failure the conversation is rolled back to
    /// where it was before the prompt, so the history sent to the API stays valid.
    pub fn ask(&mut self, provider: &dyn Provider, prompt: &str, cwd: &Path, io: &mut dyn SessionIo) -> io::Result<()> {
        let checkpoint = self.messages.len();
        let result = self.run(provider, prompt, cwd, io);
        if !matches!(result, Ok(true)) {
            self.messages.truncate(checkpoint);
        }
        result.map(|_| ())
    }

    /// Returns Ok(true) when the turn completed normally.
    fn run(&mut self, provider: &dyn Provider, prompt: &str, cwd: &Path, io: &mut dyn SessionIo) -> io::Result<bool> {
        self.messages.push(json!({"role": "user", "content": prompt}));
        let tool_defs = tools::definitions();

        for _ in 0..MAX_STEPS {
            let turn = match provider.complete(SYSTEM_PROMPT, &tool_defs, &self.messages) {
                Ok(t) => t,
                Err(e) => {
                    io.emit(Event::Error { message: e })?;
                    return Ok(false);
                }
            };
            match turn.stop_reason.as_str() {
                "refusal" => {
                    io.emit(Event::Error { message: "The model declined this request.".into() })?;
                    return Ok(false);
                }
                "max_tokens" => {
                    io.emit(Event::Error { message: "The response hit the output limit and was cut off.".into() })?;
                    return Ok(false);
                }
                _ => {}
            }

            // Keep the assistant content exactly as returned (including thinking blocks).
            self.messages.push(json!({"role": "assistant", "content": turn.content}));

            let mut results = Vec::new();
            for block in &turn.content {
                match block["type"].as_str() {
                    Some("text") => {
                        let t = block["text"].as_str().unwrap_or("").trim();
                        if !t.is_empty() {
                            io.emit(Event::Text { text: t.to_string() })?;
                        }
                    }
                    Some("tool_use") => results.push(self.call_tool(block, cwd, io)?),
                    _ => {}
                }
            }

            if turn.stop_reason != "tool_use" || results.is_empty() {
                return Ok(true);
            }
            self.messages.push(json!({"role": "user", "content": results}));
        }
        io.emit(Event::Error { message: format!("Stopped after {MAX_STEPS} steps.") })?;
        Ok(false)
    }

    fn call_tool(&self, block: &Value, cwd: &Path, io: &mut dyn SessionIo) -> io::Result<Value> {
        let id = block["id"].as_str().unwrap_or("");
        let name = block["name"].as_str().unwrap_or("");
        let input = &block["input"];
        let summary = tools::describe(name, input);

        let approved = match tools::access(name) {
            Access::ReadOnly => true,
            Access::NeedsApproval => {
                let reason = input["reason"].as_str().unwrap_or("(no reason given)");
                io.confirm(&summary, reason)?
            }
        };
        audit(json!({"tool": name, "input": input, "approved": approved, "cwd": cwd.display().to_string()}));
        if !approved {
            return Ok(json!({"type": "tool_result", "tool_use_id": id, "is_error": true,
                             "content": "The user declined to run this."}));
        }

        io.emit(Event::Action { tool: name.to_string(), summary })?;
        let (output, is_error) = tools::execute(name, input, cwd);
        io.emit(Event::Output { text: output.clone() })?;
        Ok(json!({"type": "tool_result", "tool_use_id": id, "is_error": is_error, "content": output}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Offline;

    struct Recorder {
        events: Vec<Event>,
        approve: bool,
    }

    impl SessionIo for Recorder {
        fn emit(&mut self, e: Event) -> io::Result<()> {
            self.events.push(e);
            Ok(())
        }
        fn confirm(&mut self, command: &str, _reason: &str) -> io::Result<bool> {
            self.events.push(Event::Confirm { command: command.into(), reason: String::new() });
            Ok(self.approve)
        }
    }

    #[test]
    fn approved_command_runs() {
        std::env::set_var("RUOS_AUDIT_LOG", "/dev/null");
        let mut s = Session::default();
        let mut io = Recorder { events: vec![], approve: true };
        s.ask(&Offline, "run echo agent-ok", Path::new("/"), &mut io).unwrap();
        assert!(matches!(&io.events[0], Event::Confirm { command, .. } if command == "echo agent-ok"));
        assert!(io.events.iter().any(|e| matches!(e, Event::Output { text } if text.contains("agent-ok"))));
        // user, assistant(tool_use), user(tool_result), assistant(text)
        assert_eq!(s.messages.len(), 4);
    }

    #[test]
    fn declined_command_does_not_run() {
        std::env::set_var("RUOS_AUDIT_LOG", "/dev/null");
        let mut s = Session::default();
        let mut io = Recorder { events: vec![], approve: false };
        s.ask(&Offline, "run echo should-not-appear", Path::new("/"), &mut io).unwrap();
        assert!(!io.events.iter().any(|e| matches!(e, Event::Output { .. })));
        assert_eq!(s.messages[2]["content"][0]["is_error"], true);
    }

    #[test]
    fn read_only_tools_skip_confirmation() {
        let mut s = Session::default();
        let mut io = Recorder { events: vec![], approve: false };
        s.ask(&Offline, "system info", Path::new("/"), &mut io).unwrap();
        assert!(!io.events.iter().any(|e| matches!(e, Event::Confirm { .. })));
        assert!(io.events.iter().any(|e| matches!(e, Event::Output { text } if text.contains("kernel:"))));
    }
}
