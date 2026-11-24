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
            for d in v["permission_denials"].as_array().into_iter().flatten() {
                if let Some(tool) = d["tool_name"].as_str() {
                    let (summary, rule) = describe_denial(tool, &d["tool_input"]);
                    out.push(AgentEvent::Denied { tool: tool.to_string(), summary, rule });
                }
            }
            let error = (v["is_error"] == true)
                .then(|| v["result"].as_str().unwrap_or("agent failed").to_string());
            out.push(AgentEvent::Done { error });
        }
        _ => {}
    }
    out
}

/// Human-readable summary plus the narrowest allow rule for a denied call.
fn describe_denial(tool: &str, input: &Value) -> (String, String) {
    if let Some(cmd) = input["command"].as_str() {
        return (cmd.to_string(), format!("{tool}({cmd})"));
    }
    if let Some(path) = input["file_path"].as_str() {
        return (format!("{tool} {path}"), format!("{tool}({path})"));
    }
    let mut detail = input.to_string();
    detail.truncate(120);
    (format!("{tool} {detail}"), tool.to_string())
}

pub struct Turn {
    pub dir: PathBuf,
    pub prompt: String,
    pub resume: Option<String>,
    /// Allow rules for this turn only (e.g. `Bash(touch a.txt)`).
    pub allowed_tools: Vec<String>,
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
        if !turn.allowed_tools.is_empty() {
            cmd.arg("--allowedTools").args(&turn.allowed_tools);
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
    fn denials_become_scoped_rules() {
        let evs = parse_line(
            r#"{"type":"result","is_error":false,"permission_denials":[
              {"tool_name":"Bash","tool_input":{"command":"touch a.txt"}},
              {"tool_name":"Edit","tool_input":{"file_path":"/p/x.rs"}}]}"#,
        );
        assert_eq!(
            evs[0],
            AgentEvent::Denied {
                tool: "Bash".into(),
                summary: "touch a.txt".into(),
                rule: "Bash(touch a.txt)".into()
            }
        );
        assert_eq!(
            evs[1],
            AgentEvent::Denied {
                tool: "Edit".into(),
                summary: "Edit /p/x.rs".into(),
                rule: "Edit(/p/x.rs)".into()
            }
        );
        assert_eq!(evs[2], AgentEvent::Done { error: None });
    }

    #[test]
    fn ignores_noise() {
        assert!(parse_line("not json").is_empty());
        assert!(parse_line(r#"{"type":"rate_limit_event"}"#).is_empty());
    }
}

#[cfg(test)]
mod live {
    /// Deny, then approve with a scoped rule and resume: the file appears.
    #[test]
    #[ignore]
    fn live_denial_then_scoped_approval() {
        use super::*;
        let dir = std::env::temp_dir().join(format!("kiln-approval-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let collect = |t: Turn| -> Vec<AgentEvent> {
            let rx = run(t);
            std::iter::from_fn(|| rx.recv_blocking().ok()).collect()
        };
        let first = collect(Turn {
            dir: dir.clone(),
            prompt: "run the shell command: touch approved.txt".into(),
            resume: None,
            allowed_tools: vec![],
        });
        let session = first.iter().find_map(|e| match e {
            AgentEvent::Session(id) => Some(id.clone()),
            _ => None,
        });
        let rule = first.iter().find_map(|e| match e {
            AgentEvent::Denied { rule, .. } => Some(rule.clone()),
            _ => None,
        });
        let Some(rule) = rule else {
            // Claude's own rules decided not to ask in this directory; nothing to approve.
            eprintln!("no denial in this directory; skipping approval half");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        assert!(!dir.join("approved.txt").exists());
        collect(Turn {
            dir: dir.clone(),
            prompt: "Permission granted. Run it now.".into(),
            resume: session,
            allowed_tools: vec![rule],
        });
        assert!(dir.join("approved.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::*;

    /// Needs the `claude` CLI and network: `cargo test -p kiln-agent -- --ignored`.
    #[test]
    #[ignore]
    fn live_turn_streams_session_text_and_done() {
        let rx = run(Turn {
            dir: std::env::temp_dir(),
            prompt: "reply with the single word: ok".into(),
            resume: None,
            allowed_tools: vec![],
        });
        let events: Vec<_> = std::iter::from_fn(|| rx.recv_blocking().ok()).collect();
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Session(_))));
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Text(_))));
        assert_eq!(events.last(), Some(&AgentEvent::Done { error: None }));
    }
}
