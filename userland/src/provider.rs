//! Model backends.
//!
//! Both backends speak the Claude Messages API shape (content blocks,
//! `stop_reason`), so the agent loop does not care which one is active.
//! - `Anthropic`: the Claude API over HTTPS.
//! - `Offline`: a small rule-based stand-in used when no API key is
//!   configured. It understands a handful of phrases and lets the whole OS,
//!   including the tool loop and approval flow, run and be tested offline.

use crate::config::Config;
use serde_json::{json, Value};
use std::time::Duration;

pub struct Turn {
    pub content: Vec<Value>,
    pub stop_reason: String,
}

pub trait Provider: Send + Sync {
    fn name(&self) -> String;
    fn complete(&self, system: &str, tools: &Value, messages: &[Value]) -> Result<Turn, String>;
}

pub fn from_config(cfg: &Config) -> Box<dyn Provider> {
    match (&cfg.provider, &cfg.api_key) {
        (crate::config::ProviderKind::Anthropic, Some(key)) => Box::new(Anthropic::new(cfg, key.clone())),
        (crate::config::ProviderKind::Anthropic, None) => {
            eprintln!("ruosd: RUOS_PROVIDER=anthropic but no ANTHROPIC_API_KEY; using offline provider");
            Box::new(Offline)
        }
        (crate::config::ProviderKind::Offline, _) => Box::new(Offline),
    }
}

pub struct Anthropic {
    agent: ureq::Agent,
    url: String,
    api_key: String,
    model: String,
    effort: String,
}

const MAX_ATTEMPTS: u32 = 3;

impl Anthropic {
    pub fn new(cfg: &Config, api_key: String) -> Anthropic {
        let mut builder = ureq::AgentBuilder::new().timeout(Duration::from_secs(600));
        if let Some(proxy) = &cfg.https_proxy {
            match ureq::Proxy::new(proxy) {
                Ok(p) => builder = builder.proxy(p),
                Err(e) => eprintln!("ruosd: ignoring HTTPS_PROXY: {e}"),
            }
        }
        Anthropic {
            agent: builder.build(),
            url: format!("{}/v1/messages", cfg.base_url.trim_end_matches('/')),
            api_key,
            model: cfg.model.clone(),
            effort: cfg.effort.clone(),
        }
    }

    pub fn request_body(&self, system: &str, tools: &Value, messages: &[Value]) -> Value {
        json!({
            "model": self.model,
            "max_tokens": 16000,
            "system": system,
            "tools": tools,
            "messages": messages,
            "output_config": {"effort": self.effort},
            // On a safety decline, let the API retry on its recommended fallback model.
            "fallbacks": "default",
            "cache_control": {"type": "ephemeral"},
        })
    }
}

impl Provider for Anthropic {
    fn name(&self) -> String {
        format!("anthropic ({})", self.model)
    }

    fn complete(&self, system: &str, tools: &Value, messages: &[Value]) -> Result<Turn, String> {
        let body = self.request_body(system, tools, messages);
        let mut attempt = 0;
        loop {
            attempt += 1;
            let result = self
                .agent
                .post(&self.url)
                .set("x-api-key", &self.api_key)
                .set("anthropic-version", "2023-06-01")
                .set("anthropic-beta", "server-side-fallback-2026-07-01")
                .set("content-type", "application/json")
                .send_json(body.clone());
            let wait = match result {
                Ok(resp) => {
                    let v: Value = resp.into_json().map_err(|e| format!("bad API response: {e}"))?;
                    return Ok(Turn {
                        content: v["content"].as_array().cloned().unwrap_or_default(),
                        stop_reason: v["stop_reason"].as_str().unwrap_or("").to_string(),
                    });
                }
                Err(ureq::Error::Status(code, resp)) if (code == 429 || code >= 500) && attempt < MAX_ATTEMPTS => {
                    let after = resp.header("retry-after").and_then(|s| s.parse::<u64>().ok());
                    Duration::from_secs(after.unwrap_or(2u64.pow(attempt)).min(30))
                }
                Err(ureq::Error::Status(code, resp)) => {
                    let text = resp.into_string().unwrap_or_default();
                    let msg = serde_json::from_str::<Value>(&text)
                        .ok()
                        .and_then(|v| v["error"]["message"].as_str().map(String::from))
                        .unwrap_or(text);
                    return Err(format!("Claude API returned HTTP {code}: {msg}"));
                }
                Err(e) if attempt < MAX_ATTEMPTS => {
                    eprintln!("ruosd: transport error (attempt {attempt}): {e}");
                    Duration::from_secs(2u64.pow(attempt))
                }
                Err(e) => return Err(format!("cannot reach the Claude API: {e}")),
            };
            std::thread::sleep(wait);
        }
    }
}

pub struct Offline;

pub const OFFLINE_HELP: &str = "Offline mode: no API key is configured, so I only understand a few phrases:\n  list files [path] | system info | read <path> | run <command>\nTo use Claude, pass ANTHROPIC_API_KEY to the VM (see README: `make run CONFIG=...`).";

fn tool_use(name: &str, input: Value) -> Turn {
    Turn {
        content: vec![json!({"type": "tool_use", "id": "offline_0", "name": name, "input": input})],
        stop_reason: "tool_use".into(),
    }
}

fn text(t: &str) -> Turn {
    Turn { content: vec![json!({"type": "text", "text": t})], stop_reason: "end_turn".into() }
}

impl Provider for Offline {
    fn name(&self) -> String {
        "offline (rule-based)".into()
    }

    fn complete(&self, _system: &str, _tools: &Value, messages: &[Value]) -> Result<Turn, String> {
        let last = messages.last().ok_or("empty conversation")?;
        let content = &last["content"];
        // After a tool result, the output was already shown to the user.
        if content.as_array().is_some_and(|blocks| blocks.iter().any(|b| b["type"] == "tool_result")) {
            let declined = content[0]["is_error"] == true;
            return Ok(text(if declined { "Not done." } else { "Done." }));
        }
        let prompt = content.as_str().unwrap_or("").trim();
        let lower = prompt.to_lowercase();
        let rest = |n: usize| prompt[n..].trim().to_string();

        Ok(if let Some(cmd) = lower.strip_prefix("run ").map(|_| rest(4)) {
            tool_use("run_command", json!({"command": cmd, "reason": "You asked me to run it."}))
        } else if lower.starts_with("read ") || lower.starts_with("cat ") {
            let path = prompt.split_whitespace().last().unwrap_or(".").to_string();
            tool_use("read_file", json!({"path": path}))
        } else if lower.starts_with("list") || lower.starts_with("ls") || lower.contains("files") {
            let path = prompt.split_whitespace().skip(1).find(|w| w.contains('/') || *w == "." || *w == "..");
            tool_use("list_dir", json!({"path": path.unwrap_or(".")}))
        } else if ["system", "memory", "uptime", "info", "kernel"].iter().any(|w| lower.contains(w)) {
            tool_use("system_info", json!({}))
        } else {
            text(OFFLINE_HELP)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn ask(p: &str) -> Turn {
        Offline.complete("", &json!([]), &[json!({"role": "user", "content": p})]).unwrap()
    }

    #[test]
    fn offline_maps_phrases_to_tools() {
        assert_eq!(ask("list files in /etc").content[0]["input"]["path"], "/etc");
        assert_eq!(ask("ls").content[0]["name"], "list_dir");
        assert_eq!(ask("how much memory is free?").content[0]["name"], "system_info");
        let run = ask("run echo Hello World");
        assert_eq!(run.content[0]["input"]["command"], "echo Hello World");
        assert_eq!(ask("read /etc/hostname").content[0]["input"]["path"], "/etc/hostname");
        assert_eq!(ask("write me a poem").stop_reason, "end_turn");
    }

    #[test]
    fn anthropic_request_shape() {
        let mut kv = HashMap::new();
        kv.insert("ANTHROPIC_API_KEY".to_string(), "k".to_string());
        let cfg = Config::from_map(&kv);
        let a = Anthropic::new(&cfg, "k".into());
        let body = a.request_body("sys", &json!([]), &[json!({"role": "user", "content": "hi"})]);
        assert_eq!(a.url, "https://api.anthropic.com/v1/messages");
        assert_eq!(body["model"], "claude-opus-5-5");
        assert_eq!(body["output_config"]["effort"], "medium");
        assert_eq!(body["fallbacks"], "default");
        assert!(body.get("thinking").is_none());
    }
}
