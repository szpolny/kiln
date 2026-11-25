//! Claude Code adapter: one `claude -p` process per turn, resumed by session id.

use crate::process::{self, Process};
use crate::AgentEvent;
use kiln_core::policy::PermissionMode;
use serde_json::Value;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

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
            if let Some(model) = v["model"].as_str() {
                out.push(AgentEvent::Model(model.to_string()));
            }
            let mode = match v["permissionMode"].as_str() {
                Some("default" | "manual") => Some(PermissionMode::Manual),
                Some("auto") => Some(PermissionMode::Auto),
                Some("bypassPermissions") => Some(PermissionMode::FullAccess),
                _ => None,
            };
            if let Some(mode) = mode {
                out.push(AgentEvent::PermissionMode(mode));
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
                            out.push(AgentEvent::Tool {
                                name: n.to_string(),
                                input: describe_input(&block["input"]),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        Some("result") => {
            for d in v["permission_denials"].as_array().into_iter().flatten() {
                if let Some(tool) = d["tool_name"].as_str() {
                    let input = &d["tool_input"];
                    out.push(AgentEvent::Denied {
                        tool: tool.to_string(),
                        summary: describe_input(input),
                        rule: allow_rule(tool, input),
                        path: target(tool, input),
                    });
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

/// Longest argument value shown, in characters. Commands are never cut.
const MAX_ARG_CHARS: usize = 2000;

/// A tool call's arguments as text: the shell command first and in full, then
/// one `name: value` line per remaining argument.
fn describe_input(input: &Value) -> String {
    let Some(args) = input.as_object() else {
        return String::new();
    };
    let command = args.get("command").and_then(Value::as_str);
    let mut lines: Vec<String> = command.map(|c| format!("$ {c}")).into_iter().collect();
    // The argument that says what is being touched goes first.
    let main = ["file_path", "path", "pattern", "url", "query"];
    let rank = |key: &str| main.iter().position(|m| *m == key).unwrap_or(main.len());
    let mut rest: Vec<_> = args
        .iter()
        .filter(|(key, _)| command.is_none() || *key != "command")
        .collect();
    rest.sort_by_key(|(key, _)| rank(key));
    for (key, value) in rest {
        let value = match value {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let cut = value.char_indices().nth(MAX_ARG_CHARS);
        lines.push(match cut {
            Some((at, _)) => {
                let more = value[at..].chars().count();
                format!("{key}: {}… ({more} more characters)", &value[..at])
            }
            None => format!("{key}: {value}"),
        });
    }
    lines.join("\n")
}

/// The file or folder a file tool call reaches; `None` for other tools and
/// when it cannot be told.
fn target(tool: &str, input: &Value) -> Option<PathBuf> {
    let arg = |name: &str| input[name].as_str().map(PathBuf::from);
    match tool {
        "Read" | "Write" | "Edit" | "MultiEdit" => arg("file_path"),
        "NotebookEdit" => arg("notebook_path"),
        "Grep" => Some(arg("path").unwrap_or_else(|| ".".into())),
        // A glob pattern can leave the search folder by itself.
        "Glob" => {
            let pattern = input["pattern"].as_str()?;
            if ["..", "{/", ",/"].iter().any(|p| pattern.contains(p)) {
                return None;
            }
            let fixed: PathBuf = Path::new(pattern)
                .components()
                .take_while(|c| !c.as_os_str().to_string_lossy().contains(['*', '?', '[', '{']))
                .collect();
            Some(arg("path").unwrap_or_else(|| ".".into()).join(fixed))
        }
        _ => None,
    }
}

/// The narrowest allow rule for a denied call.
///
/// Claude Code matches file rules as patterns, only under the names `Read` and
/// `Edit`, and reads a single leading slash as relative to its settings.
fn allow_rule(tool: &str, input: &Value) -> String {
    if let Some(command) = input["command"].as_str() {
        return format!("{tool}({command})");
    }
    let (name, below) = match tool {
        "Read" => ("Read", ""),
        "Glob" | "Grep" => ("Read", "/**"),
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => ("Edit", ""),
        _ => return tool.to_string(),
    };
    // Never fall back to a bare tool name: that would allow every file.
    let Some(path) = target(tool, input) else {
        return format!("{name}(./)");
    };
    let mut pattern = String::new();
    for c in path.to_string_lossy().chars() {
        if matches!(c, '\\' | '*' | '?' | '[' | ']') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    let root = if path.is_absolute() {
        "/"
    } else if path.starts_with(".") {
        ""
    } else {
        "./"
    };
    format!("{name}({root}{pattern}{below})")
}

pub struct Turn {
    pub dir: PathBuf,
    pub prompt: String,
    pub resume: Option<String>,
    /// Model alias or full name; `None` uses the agent's configured default.
    pub model: Option<String>,
    /// Reasoning level; `None` uses the agent's configured default.
    pub effort: Option<String>,
    pub permission_mode: PermissionMode,
    /// Allow rules for this turn only (e.g. `Bash(touch a.txt)`).
    pub allowed_tools: Vec<String>,
    /// Tools the agent may not use this turn, whatever its own settings say.
    pub disallowed_tools: Vec<String>,
}

/// Start a turn: its process, and the events it produces. The last event is
/// `Done` or `Stopped`.
pub fn run(turn: Turn) -> io::Result<(Process, async_channel::Receiver<AgentEvent>)> {
    process::spawn(command(turn), parse_line)
}

fn command(turn: Turn) -> Command {
    let mode = match turn.permission_mode {
        PermissionMode::Manual => "default",
        PermissionMode::Auto => "auto",
        PermissionMode::FullAccess => "bypassPermissions",
    };
    let mut cmd = Command::new("claude");
    cmd.current_dir(&turn.dir)
        .args(["-p", &turn.prompt, "--output-format", "stream-json", "--verbose"])
        .args(["--permission-mode", mode])
        // Privacy: no telemetry or non-essential traffic from the child.
        .env("DISABLE_TELEMETRY", "1")
        .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1");
    if let Some(id) = &turn.resume {
        cmd.args(["--resume", id]);
    }
    if let Some(model) = &turn.model {
        cmd.args(["--model", model]);
    }
    if let Some(effort) = &turn.effort {
        // The variable outranks the flag and would silently undo the choice.
        cmd.args(["--effort", effort]).env_remove("CLAUDE_CODE_EFFORT_LEVEL");
    }
    if !turn.allowed_tools.is_empty() {
        cmd.arg("--allowedTools").args(&turn.allowed_tools);
    }
    if !turn.disallowed_tools.is_empty() {
        cmd.arg("--disallowedTools").args(&turn.disallowed_tools);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resumed_turns_set_the_requested_permission_mode() {
        for (permission_mode, expected) in [
            (PermissionMode::Manual, "default"),
            (PermissionMode::Auto, "auto"),
            (PermissionMode::FullAccess, "bypassPermissions"),
        ] {
            let cmd = command(Turn {
                dir: ".".into(),
                prompt: "Continue.".into(),
                resume: Some("session".into()),
                model: None,
                effort: None,
                permission_mode,
                allowed_tools: vec![],
                disallowed_tools: vec![],
            });
            let args: Vec<_> = cmd.get_args().collect();
            assert!(args.windows(2).any(|pair| pair == ["--permission-mode", expected]));
            assert!(args.windows(2).any(|pair| pair == ["--resume", "session"]));
        }
    }

    #[test]
    fn parses_init_text_tool_and_result() {
        assert_eq!(
            parse_line(r#"{"type":"system","subtype":"init","session_id":"abc","model":"m-1","permissionMode":"default"}"#),
            vec![
                AgentEvent::Session("abc".into()),
                AgentEvent::Model("m-1".into()),
                AgentEvent::PermissionMode(PermissionMode::Manual),
            ]
        );
        assert_eq!(
            parse_line(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"},{"type":"tool_use","name":"Bash","input":{"command":"ls -la","description":"List files"}}]}}"#),
            vec![
                AgentEvent::Text("hi".into()),
                AgentEvent::Tool {
                    name: "Bash".into(),
                    input: "$ ls -la\ndescription: List files".into()
                }
            ]
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
                summary: "$ touch a.txt".into(),
                rule: "Bash(touch a.txt)".into(),
                path: None,
            }
        );
        assert_eq!(
            evs[1],
            AgentEvent::Denied {
                tool: "Edit".into(),
                summary: "file_path: /p/x.rs".into(),
                rule: "Edit(//p/x.rs)".into(),
                path: Some("/p/x.rs".into()),
            }
        );
        assert_eq!(evs[2], AgentEvent::Done { error: None });
    }

    #[test]
    fn file_rules_name_one_target_the_way_claude_matches_them() {
        let rule = |tool, input| allow_rule(tool, &input);
        let json = |v: &str| serde_json::from_str::<Value>(v).unwrap();
        let file = json(r#"{"file_path":"/p/a b/[id]*.ts"}"#);
        assert_eq!(rule("Write", file.clone()), r"Edit(//p/a b/\[id\]\*.ts)");
        assert_eq!(rule("Read", file), r"Read(//p/a b/\[id\]\*.ts)");
        let notebook = json(r#"{"notebook_path":"/p/n.ipynb"}"#);
        assert_eq!(rule("NotebookEdit", notebook), "Edit(//p/n.ipynb)");
        assert_eq!(rule("Grep", json(r#"{"path":"/p/src"}"#)), "Read(//p/src/**)");
        assert_eq!(rule("Grep", json("{}")), "Read(./**)");
        assert_eq!(rule("Edit", json("{}")), "Edit(./)");
    }

    #[test]
    fn glob_targets_include_where_the_pattern_leads() {
        let glob = |v: serde_json::Value| target("Glob", &v);
        let at = |pattern: &str| glob(serde_json::json!({ "pattern": pattern, "path": "/p" }));
        assert_eq!(at("src/**/*.rs"), Some("/p/src".into()));
        assert_eq!(at("/etc/*.conf"), Some("/etc".into()));
        assert_eq!(at("../*"), None);
        assert_eq!(at("{/etc,src}/*"), None);
    }

    #[test]
    fn tool_input_keeps_commands_whole_and_caps_other_values() {
        let long = "é".repeat(MAX_ARG_CHARS + 5);
        let command = serde_json::json!({ "command": long });
        assert_eq!(describe_input(&command), format!("$ {long}"));

        let write = serde_json::json!({ "content": long, "file_path": "/p/a" });
        let shown = describe_input(&write);
        assert!(shown.starts_with("file_path: /p/a\ncontent: é"));
        assert!(shown.ends_with("… (5 more characters)"));
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
            let (_process, rx) = run(t).unwrap();
            std::iter::from_fn(|| rx.recv_blocking().ok()).collect()
        };
        let first = collect(Turn {
            dir: dir.clone(),
            prompt: "run the shell command: touch approved.txt".into(),
            resume: None,
            model: None,
            effort: None,
            permission_mode: PermissionMode::Manual,
            allowed_tools: vec![],
            disallowed_tools: vec![],
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
            model: None,
            effort: None,
            permission_mode: PermissionMode::Manual,
            allowed_tools: vec![rule],
            disallowed_tools: vec![],
        });
        assert!(dir.join("approved.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::*;

    /// Approving the rules Kiln writes lets the same file edits through.
    #[test]
    #[ignore]
    fn live_file_rules_are_accepted() {
        let dir = std::env::temp_dir().join(format!("kiln-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let file = dir.join("[id] (draft).txt");
        let prompt = format!(
            "Use the Write tool to create {} containing the word hi. Do not use Bash.",
            file.display()
        );
        let turn = |allowed_tools| -> Vec<AgentEvent> {
            let turn = Turn {
                dir: dir.clone(),
                prompt: prompt.clone(),
                resume: None,
                model: None,
                effort: None,
                permission_mode: PermissionMode::Manual,
                allowed_tools,
                disallowed_tools: vec![],
            };
            let (_process, rx) = run(turn).unwrap();
            std::iter::from_fn(|| rx.recv_blocking().ok()).collect()
        };
        let rules: Vec<_> = turn(vec![])
            .into_iter()
            .filter_map(|e| match e {
                AgentEvent::Denied { rule, .. } => Some(rule),
                _ => None,
            })
            .collect();
        if !rules.is_empty() {
            assert!(!file.exists());
            let events = turn(rules);
            assert!(!events.iter().any(|e| matches!(e, AgentEvent::Denied { .. })));
        }
        assert!(file.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Stop a turn while its shell command runs: the process and the command end,
    /// and the session still resumes.
    #[test]
    #[ignore]
    fn live_stop_then_resume() {
        let dir = std::env::temp_dir().join(format!("kiln-stop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("slow.sh"), "sleep 20\ntouch finished.txt\n").unwrap();
        let turn = |prompt: &str, resume| Turn {
            dir: dir.clone(),
            prompt: prompt.into(),
            resume,
            model: None,
            effort: None,
            permission_mode: PermissionMode::Manual,
            allowed_tools: vec!["Bash(bash slow.sh)".into()],
            disallowed_tools: vec![],
        };
        let (process, rx) =
            run(turn("run this shell command in the foreground: bash slow.sh", None)).unwrap();
        let mut session = None;
        loop {
            match rx.recv_blocking().expect("ended before running a tool") {
                AgentEvent::Session(id) => session = Some(id),
                AgentEvent::Tool { .. } => break,
                _ => {}
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
        process.stop();
        let after: Vec<_> = std::iter::from_fn(|| rx.recv_blocking().ok()).collect();
        assert_eq!(after.last(), Some(&AgentEvent::Stopped));
        // Long enough for the command to finish, had it been left running.
        std::thread::sleep(std::time::Duration::from_secs(22));
        assert!(!dir.join("finished.txt").exists());

        let (_process, rx) = run(turn("reply with the single word: ok", session)).unwrap();
        let events: Vec<_> = std::iter::from_fn(|| rx.recv_blocking().ok()).collect();
        assert_eq!(events.last(), Some(&AgentEvent::Done { error: None }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Needs the `claude` CLI and network: `cargo test -p kiln-agent -- --ignored`.
    #[test]
    #[ignore]
    fn live_turn_streams_session_text_and_done() {
        let turn = Turn {
            dir: std::env::temp_dir(),
            prompt: "reply with the single word: ok".into(),
            resume: None,
            model: None,
            effort: None,
            permission_mode: PermissionMode::Manual,
            allowed_tools: vec![],
            disallowed_tools: vec![],
        };
        let (_process, rx) = run(turn).unwrap();
        let events: Vec<_> = std::iter::from_fn(|| rx.recv_blocking().ok()).collect();
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Session(_))));
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Text(_))));
        assert_eq!(events.last(), Some(&AgentEvent::Done { error: None }));
    }
}
