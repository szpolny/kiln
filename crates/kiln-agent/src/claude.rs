//! Claude Code adapter: one `claude -p` process per turn, resumed by session id.

use crate::AgentEvent;
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Parse one stream-json line into zero or more events.
pub fn parse_line(line: &str) -> Vec<AgentEvent> {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return vec![];
    };
    let mut out = vec![];
    match v["type"].as_str() {
        Some("system") if v["subtype"] == "init" => {
            if let Some(id) = v["session_id"].as_str() {
                out.push(AgentEvent::Session(id.to_string()));
            }
        }
        Some("assistant") => {
            for block in v["message"]["content"].as_array().into_iter().flatten() {
                match block["type"].as_str() {
                    Some("text") => {
                        if let Some(t) = block["text"].as_str() {
                            out.push(AgentEvent::Text(t.to_string()));
                        }
                    }
                    Some("tool_use") => {
                        if let Some(n) = block["name"].as_str() {
                            out.push(AgentEvent::Tool(n.to_string()));
                        }
                    }
                    _ => {}
                }
            }
        }
        Some("result") => {
            let error = (v["is_error"] == true)
                .then(|| v["result"].as_str().unwrap_or("agent failed").to_string());
            out.push(AgentEvent::Done { error });
        }
        _ => {}
    }
    out
}

pub struct Turn {
    pub dir: PathBuf,
    pub prompt: String,
    pub resume: Option<String>,
}

/// Start a turn. Events arrive on the returned channel; it closes when the
/// process exits. The sender thread always ends with a `Done`.
pub fn run(turn: Turn) -> async_channel::Receiver<AgentEvent> {
    let (tx, rx) = async_channel::unbounded();
    std::thread::spawn(move || {
        let mut cmd = Command::new("claude");
        cmd.current_dir(&turn.dir)
            .args(["-p", &turn.prompt, "--output-format", "stream-json", "--verbose"])
            // Privacy: no telemetry or non-essential traffic from the child.
            .env("DISABLE_TELEMETRY", "1")
            .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::null());
        if let Some(id) = &turn.resume {
            cmd.args(["--resume", id]);
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send_blocking(AgentEvent::Done {
                    error: Some(format!("could not start claude: {e}")),
                });
                return;
            }
        };
        let mut finished = false;
        if let Some(out) = child.stdout.take() {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                for ev in parse_line(&line) {
                    finished |= matches!(ev, AgentEvent::Done { .. });
                    if tx.send_blocking(ev).is_err() {
                        let _ = child.kill();
                        return;
                    }
                }
            }
        }
        let _ = child.wait();
        if !finished {
            let _ = tx.send_blocking(AgentEvent::Done {
                error: Some("claude exited without a result".into()),
            });
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_init_text_tool_and_result() {
        assert_eq!(
            parse_line(r#"{"type":"system","subtype":"init","session_id":"abc"}"#),
            vec![AgentEvent::Session("abc".into())]
        );
        assert_eq!(
            parse_line(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"},{"type":"tool_use","name":"Bash"}]}}"#),
            vec![AgentEvent::Text("hi".into()), AgentEvent::Tool("Bash".into())]
        );
        assert_eq!(
            parse_line(r#"{"type":"result","is_error":false,"result":"hi"}"#),
            vec![AgentEvent::Done { error: None }]
        );
        assert_eq!(
            parse_line(r#"{"type":"result","is_error":true,"result":"boom"}"#),
            vec![AgentEvent::Done { error: Some("boom".into()) }]
        );
    }

    #[test]
    fn ignores_noise() {
        assert!(parse_line("not json").is_empty());
        assert!(parse_line(r#"{"type":"rate_limit_event"}"#).is_empty());
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// Needs the `claude` CLI and network: `cargo test -p kiln-agent -- --ignored`.
    #[test]
    #[ignore]
    fn live_turn_streams_session_text_and_done() {
        let rx = run(Turn {
            dir: std::env::temp_dir(),
            prompt: "reply with the single word: ok".into(),
            resume: None,
        });
        let events: Vec<_> = std::iter::from_fn(|| rx.recv_blocking().ok()).collect();
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Session(_))));
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Text(_))));
        assert_eq!(events.last(), Some(&AgentEvent::Done { error: None }));
    }
}
