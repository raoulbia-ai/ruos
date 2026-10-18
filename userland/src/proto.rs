//! Wire protocol between the `ai` client and the `ruosd` service.
//!
//! One JSON object per line over a Unix stream socket. The client sends
//! `Request`s; for each `Ask` the service streams `Event`s ending in `Done`.
//! A `Confirm` event blocks the service until the client answers with
//! `Request::Confirm`.

use serde::{Deserialize, Serialize};

pub const DEFAULT_SOCKET: &str = "/run/ruosd.sock";

/// Socket path, overridable with `RUOSD_SOCKET` (used for host-side tests).
pub fn socket_path() -> String {
    std::env::var("RUOSD_SOCKET").unwrap_or_else(|_| DEFAULT_SOCKET.to_string())
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Ask the system agent something. History is kept per connection.
    Ask { prompt: String, cwd: Option<String> },
    /// Answer to an `Event::Confirm`.
    Confirm { approve: bool },
    /// Forget this connection's conversation history.
    Reset,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Text from the model, to show the user.
    Text { text: String },
    /// The agent is calling a tool; `summary` is a one-line description.
    Action { tool: String, summary: String },
    /// Output of a tool call, to show the user.
    Output { text: String },
    /// The agent wants to run something that needs the user's approval.
    Confirm { command: String, reason: String },
    Error { message: String },
    /// End of the response to one `Ask` (or `Reset`).
    Done,
}

pub fn encode<T: Serialize>(msg: &T) -> String {
    let mut line = serde_json::to_string(msg).expect("protocol types always serialize");
    line.push('\n');
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let req = Request::Ask { prompt: "list files".into(), cwd: Some("/root".into()) };
        let line = encode(&req);
        assert_eq!(line, "{\"type\":\"ask\",\"prompt\":\"list files\",\"cwd\":\"/root\"}\n");
        assert_eq!(serde_json::from_str::<Request>(line.trim()).unwrap(), req);

        let ev = Event::Confirm { command: "rm x".into(), reason: "asked".into() };
        assert_eq!(serde_json::from_str::<Event>(encode(&ev).trim()).unwrap(), ev);
    }
}
