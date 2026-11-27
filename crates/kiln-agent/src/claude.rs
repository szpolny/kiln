//! Claude Code adapter: one `claude -p` process per turn, resumed by session id.
//!
//! The prompt and the answers to permission requests go over stdin as stream
//! JSON, so Claude waits on a request while the user decides.

mod protocol;

use protocol::{approval, describe_input, parse_line, user_message};
use crate::process::{self, Process};
use crate::{visualization, AgentEvent, ApprovalId, ApprovalRequest, Reply};
use kiln_core::policy::{Decision, PermissionMode, Policy};
use kiln_core::Attachment;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{ChildStdin, Command};
use std::sync::{Arc, Mutex};

/// What Claude is told when the user turns a request down.
const DENIED: &str = "The user denied this action.";
/// What Claude is told when Kiln's policy turns a request down.
const BLOCKED: &str = "Kiln's permission policy blocks this action.";

pub struct Turn {
    pub dir: PathBuf,
    pub prompt: String,
    pub attachments: Vec<Attachment>,
    pub resume: Option<String>,
    /// Model alias or full name; `None` uses the agent's configured default.
    pub model: Option<String>,
    /// Reasoning level; `None` uses the agent's configured default.
    pub effort: Option<String>,
    pub permission_mode: PermissionMode,
    /// Decides the requests Claude makes in manual mode.
    pub policy: Policy,
}

/// The live connection for answering requests and stopping this turn.
#[derive(Clone)]
pub struct Client(Arc<Mutex<Option<ChildStdin>>>);

impl Client {
    pub fn respond(&self, request: &ApprovalRequest, allow: bool) -> io::Result<()> {
        self.answer(request, allow, DENIED)
    }

    /// Add guidance to the turn in progress; Claude folds a message sent
    /// mid-turn into the same turn and answers with one result.
    pub(crate) fn steer_with(&self, text: &str, attachments: &[Attachment]) -> io::Result<()> {
        self.send(user_message(text, attachments))
    }

    pub fn stop(&self, process: &Process) {
        process.stop_with(|| {
            self.send(json!({ "type": "control_request", "request_id": "kiln-interrupt",
                "request": { "subtype": "interrupt" } }))
        });
    }

    fn answer(&self, request: &ApprovalRequest, allow: bool, why: &str) -> io::Result<()> {
        let response = match (&request.reply, allow) {
            (Reply::Input(input), true) => json!({ "behavior": "allow", "updatedInput": input }),
            _ => json!({ "behavior": "deny", "message": why }),
        };
        self.send(json!({ "type": "control_response", "response": {
            "subtype": "success", "request_id": request.id.0, "response": response,
        }}))
    }

    fn send(&self, message: Value) -> io::Result<()> {
        let mut input = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let input = input.as_mut().ok_or_else(|| io::Error::other("Claude turn has ended"))?;
        writeln!(input, "{message}")?;
        input.flush()
    }

    fn close(&self) {
        // EOF on stdin lets Claude exit once its turn is over.
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}

/// Start a turn: its process, its connection, and the events it produces. The
/// last event is `Done` or `Stopped`.
pub fn run(turn: Turn) -> io::Result<(Process, crate::Client, async_channel::Receiver<AgentEvent>)> {
    run_command(command(&turn), turn)
}

fn run_command(
    cmd: Command,
    turn: Turn,
) -> io::Result<(Process, crate::Client, async_channel::Receiver<AgentEvent>)> {
    let client = Client(Arc::default());
    let wire = client.clone();
    let (process, events) = process::spawn_protocol(cmd, move |input, output, events| {
        *wire.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(input);
        let result = drive(&wire, BufReader::new(output), events, &turn);
        wire.close();
        result
    })?;
    Ok((process, crate::Client::Claude(client), events))
}

fn command(turn: &Turn) -> Command {
    let mode = match turn.permission_mode {
        PermissionMode::Manual => "default",
        PermissionMode::Auto => "auto",
        PermissionMode::FullAccess => "bypassPermissions",
    };
    let mut cmd = Command::new("claude");
    cmd.current_dir(&turn.dir)
        .args(["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose"])
        .arg("--include-partial-messages")
        .args(["--permission-mode", mode])
        // Privacy: no telemetry or non-essential traffic from the child.
        .env("DISABLE_TELEMETRY", "1")
        .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1");
    let mut allowed = Vec::new();
    if let Ok(executable) = std::env::current_exe() {
        let config = json!({ "mcpServers": { (visualization::SERVER): {
            "type": "stdio", "command": executable, "args": [visualization::SERVER_ARG]
        }}});
        cmd.args(["--mcp-config", &config.to_string()]);
        allowed.push(visualization::CLAUDE_TOOL);
    }
    if turn.permission_mode == PermissionMode::Manual {
        // Ask Kiln over stdin instead of refusing what would need a prompt.
        cmd.args(["--permission-prompt-tool", "stdio"]);
        let (web_allowed, disallowed) = turn.policy.web_tool_rules();
        allowed.extend(web_allowed.iter().copied());
        if !disallowed.is_empty() {
            cmd.arg("--disallowedTools").args(disallowed);
        }
    }
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
    if !allowed.is_empty() {
        cmd.arg("--allowedTools").args(allowed);
    }
    cmd
}

fn drive(
    wire: &Client,
    output: impl BufRead,
    events: &async_channel::Sender<AgentEvent>,
    turn: &Turn,
) -> io::Result<()> {
    let emit = |event| events.send_blocking(event).map_err(io::Error::other);
    wire.send(user_message(&turn.prompt, &turn.attachments))?;
    // Tool calls Claude asked about; it lists them again among its denials.
    let mut asked = HashSet::new();
    let mut visualizations = HashMap::new();
    for line in output.lines() {
        let line = line?;
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match message["type"].as_str() {
            Some("control_request") => {
                let request = &message["request"];
                if request["subtype"] != "can_use_tool" {
                    // Unsupported requests must receive a response so they cannot stall the turn.
                    wire.send(json!({ "type": "control_response", "response": {
                        "subtype": "error", "request_id": message["request_id"],
                        "error": "Request is unsupported by Kiln",
                    }}))?;
                    continue;
                }
                if let Some(id) = request["tool_use_id"].as_str() {
                    asked.insert(id.to_string());
                }
                let (request, decision, why) = approval(&message["request_id"], request, turn);
                match decision {
                    Decision::Ask => emit(AgentEvent::ApprovalRequested(request))?,
                    Decision::Allow => wire.answer(&request, true, "")?,
                    Decision::Deny => {
                        wire.answer(&request, false, BLOCKED)?;
                        emit(AgentEvent::Notice(format!("{why}: {}\n{}", request.tool, request.summary)))?;
                    }
                }
            }
            Some("control_cancel_request") => {
                emit(AgentEvent::ApprovalResolved(ApprovalId(message["request_id"].clone())))?;
            }
            Some("result") => {
                let by = match turn.permission_mode {
                    PermissionMode::Manual => "Blocked by the agent's own settings",
                    _ => "Blocked by Claude",
                };
                for denial in message["permission_denials"].as_array().into_iter().flatten() {
                    let unasked = denial["tool_use_id"].as_str().is_none_or(|id| !asked.contains(id));
                    if let (true, Some(tool)) = (unasked, denial["tool_name"].as_str()) {
                        let summary = describe_input(&denial["tool_input"]);
                        emit(AgentEvent::Notice(format!("{by}: {tool}\n{summary}")))?;
                    }
                }
                return match message["is_error"] == true {
                    true => Err(io::Error::other(message["result"].as_str().unwrap_or("agent failed"))),
                    false => Ok(()),
                };
            }
            _ => {
                if message["parent_tool_use_id"].is_null() {
                    for block in message["message"]["content"].as_array().into_iter().flatten() {
                        match (message["type"].as_str(), block["type"].as_str()) {
                            (Some("assistant"), Some("tool_use")) if block["name"] == visualization::CLAUDE_TOOL => {
                                if let (Some(id), Ok(visual)) = (
                                    block["id"].as_str(),
                                    visualization::Visualization::parse(&block["input"]),
                                ) {
                                    visualizations.insert(id.to_owned(), visual);
                                }
                            }
                            (Some("user"), Some("tool_result")) => {
                                if let Some(visual) = block["tool_use_id"].as_str().and_then(|id| visualizations.remove(id)) {
                                    let accepted = match &block["content"] {
                                        Value::String(text) => text == visualization::ACCEPTED,
                                        Value::Array(content) => content.iter().any(|item| item["text"] == visualization::ACCEPTED),
                                        _ => false,
                                    };
                                    if block["is_error"] != true && accepted {
                                        emit(AgentEvent::Visualization(visual))?;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                for event in parse_line(&line) {
                    emit(event)?;
                }
            }
        }
    }
    Err(io::Error::other("claude exited without a result"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(permission_mode: PermissionMode) -> Turn {
        Turn {
            dir: ".".into(),
            prompt: "Continue.".into(),
            attachments: vec![],
            resume: Some("session".into()),
            model: None,
            effort: None,
            permission_mode,
            policy: Policy::default(),
        }
    }

    #[test]
    fn resumed_turns_set_the_requested_permission_mode() {
        for (permission_mode, expected) in [
            (PermissionMode::Manual, "default"),
            (PermissionMode::Auto, "auto"),
            (PermissionMode::FullAccess, "bypassPermissions"),
        ] {
            let cmd = command(&turn(permission_mode));
            let args: Vec<_> = cmd.get_args().collect();
            assert!(args.windows(2).any(|pair| pair == ["--permission-mode", expected]));
            assert!(args.windows(2).any(|pair| pair == ["--resume", "session"]));
            let asks = args.windows(2).any(|pair| pair == ["--permission-prompt-tool", "stdio"]);
            assert_eq!(asks, permission_mode == PermissionMode::Manual);
        }
    }

    #[test]
    fn web_tools_follow_the_network_policy_in_manual_mode() {
        let mut turn = turn(PermissionMode::Manual);
        turn.policy.network = Decision::Deny;
        let args: Vec<_> = command(&turn).get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        let at = args.iter().position(|a| a == "--disallowedTools").unwrap();
        assert_eq!(args[at + 1..at + 3], ["WebFetch", "WebSearch"]);
        let at = args.iter().position(|a| a == "--allowedTools").unwrap();
        assert_eq!(args[at + 1..], [visualization::CLAUDE_TOOL]);
    }

    /// Run `script` as Claude after it has read the prompt.
    fn fixture(script: &str, turn: Turn) -> (Process, crate::Client, async_channel::Receiver<AgentEvent>) {
        let mut cmd = Command::new("sh");
        // `has` fails unless the last line read holds every fragment, in any order.
        let prompt = r#"
            has() { for part; do case "$line" in *"$part"*) ;; *) exit 1 ;; esac; done; }
            IFS= read -r line
            has '"type":"user"' '"role":"user"' '"content":"Continue."'
            echo '{"type":"system","subtype":"init","session_id":"session"}'
        "#;
        cmd.args(["-c", &format!("{prompt}\n{script}")]);
        let (process, client, events) = run_command(cmd, turn).unwrap();
        assert_eq!(events.recv_blocking().unwrap(), AgentEvent::Session("session".into()));
        (process, client, events)
    }

    #[test]
    fn publishes_only_successful_visualization_calls() {
        let (_, _, events) = fixture(
            r#"
            echo '{"type":"assistant","message":{"content":[
                {"type":"tool_use","id":"rejected","name":"mcp__kiln_visualization__publish_visualization","input":{"html":"<p>Rejected</p>"}},
                {"type":"tool_use","id":"shown","name":"mcp__kiln_visualization__publish_visualization","input":{"html":"<p>Chart</p>","title":"Builds"}}
            ]}}' | tr -d '\n'
            echo
            echo '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"rejected","is_error":true,"content":"Denied"}]}}'
            echo '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"shown","content":[{"type":"text","text":"Visualization submitted for display in the conversation."}]}]}}'
            echo '{"type":"result","is_error":false}'
        "#,
            turn(PermissionMode::Manual),
        );
        assert_eq!(events.recv_blocking().unwrap(), AgentEvent::Visualization(visualization::Visualization {
            title: "Builds".into(), html: "<p>Chart</p>".into(),
        }));
        assert_eq!(events.recv_blocking().unwrap(), AgentEvent::Done { error: None });
    }

    #[test]
    fn a_live_request_waits_for_the_user_within_the_turn() {
        let (_, client, events) = fixture(
            r#"
            echo '{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"touch a"},"tool_use_id":"t1"}}'
            IFS= read -r line
            has '"type":"control_response"' '"subtype":"success"' '"request_id":"r1"' '"behavior":"deny"' '"message":"The user denied this action."'
            echo '{"type":"control_request","request_id":"r2","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"touch b"},"tool_use_id":"t2"}}'
            IFS= read -r line
            has '"request_id":"r2"' '"behavior":"allow"' '"updatedInput":{"command":"touch b"}'
            echo '{"type":"assistant","message":{"content":[{"type":"text","text":"done"}]}}'
            echo '{"type":"result","is_error":false,"permission_denials":[{"tool_name":"Bash","tool_use_id":"t1","tool_input":{"command":"touch a"}}]}'
            if IFS= read -r line; then exit 1; fi
        "#,
            turn(PermissionMode::Manual),
        );
        for (command, allow) in [("$ touch a", false), ("$ touch b", true)] {
            let AgentEvent::ApprovalRequested(request) = events.recv_blocking().unwrap() else {
                panic!("expected a live approval");
            };
            assert_eq!((request.tool.as_str(), request.summary.as_str()), ("Bash", command));
            client.respond(&request, allow).unwrap();
        }
        // The denial the user made is not reported again.
        assert_eq!(events.recv_blocking().unwrap(), AgentEvent::Text("done".into()));
        assert_eq!(events.recv_blocking().unwrap(), AgentEvent::Done { error: None });
        assert!(events.recv_blocking().is_err());
    }

    #[test]
    fn steering_sends_another_user_message_into_the_same_turn() {
        let (_, client, events) = fixture(
            r#"
            echo '{"type":"assistant","message":{"content":[{"type":"text","text":"working"}]}}'
            IFS= read -r line
            has '"type":"user"' '"content":"use tabs"'
            echo '{"type":"result","is_error":false}'
            if IFS= read -r line; then exit 1; fi
        "#,
            turn(PermissionMode::Manual),
        );
        assert_eq!(events.recv_blocking().unwrap(), AgentEvent::Text("working".into()));
        client.steer("use tabs", &[]).unwrap();
        assert_eq!(events.recv_blocking().unwrap(), AgentEvent::Done { error: None });
        assert!(events.recv_blocking().is_err());
    }

    #[test]
    fn the_policy_answers_what_it_already_decides() {
        let dir = std::env::temp_dir().join(format!("kiln-claude-policy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut turn = turn(PermissionMode::Manual);
        turn.dir = dir.clone();
        let inside = dir.join("a.txt");
        let script = format!(
            r#"
            echo '{{"type":"control_request","request_id":"r1","request":{{"subtype":"can_use_tool","tool_name":"Write","input":{{"file_path":"{inside}"}},"tool_use_id":"t1"}}}}'
            IFS= read -r line
            case "$line" in *'"behavior":"allow"'*) ;; *) exit 1 ;; esac
            echo '{{"type":"control_request","request_id":"r2","request":{{"subtype":"can_use_tool","tool_name":"Read","input":{{"file_path":"/etc/hosts"}},"tool_use_id":"t2"}}}}'
            IFS= read -r line
            case "$line" in *'"behavior":"deny"'*) ;; *) exit 1 ;; esac
            echo '{{"type":"result","is_error":false,"permission_denials":[{{"tool_name":"WebSearch","tool_use_id":"t3","tool_input":{{"query":"q"}}}}]}}'
        "#,
            inside = inside.display()
        );
        let (_, _, events) = fixture(&script, turn);
        let rest: Vec<_> = std::iter::from_fn(|| events.recv_blocking().ok()).collect();
        assert_eq!(rest, vec![
            AgentEvent::Notice("Blocked outside the project: Read\nfile_path: /etc/hosts".into()),
            AgentEvent::Notice("Blocked by the agent's own settings: WebSearch\nquery: q".into()),
            AgentEvent::Done { error: None },
        ]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stopping_a_waiting_request_interrupts_the_turn() {
        let (process, client, events) = fixture(
            r#"
            echo '{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"touch a"},"tool_use_id":"t1"}}'
            IFS= read -r line
            case "$line" in *'"subtype":"interrupt"'*) ;; *) exit 1 ;; esac
            echo '{"type":"control_cancel_request","request_id":"r1"}'
            echo '{"type":"result","is_error":true,"permission_denials":[{"tool_name":"Bash","tool_use_id":"t1","tool_input":{"command":"touch a"}}]}'
            if IFS= read -r line; then exit 1; fi
        "#,
            turn(PermissionMode::Manual),
        );
        let AgentEvent::ApprovalRequested(request) = events.recv_blocking().unwrap() else {
            panic!("expected a live approval");
        };
        client.stop(&process);
        assert_eq!(events.recv_blocking().unwrap(), AgentEvent::ApprovalResolved(request.id));
        assert_eq!(events.recv_blocking().unwrap(), AgentEvent::Stopped);
        assert!(events.recv_blocking().is_err());
    }

    #[test]
    fn unsupported_requests_are_answered() {
        let (_, _, events) = fixture(
            r#"
            echo '{"type":"control_request","request_id":"r1","request":{"subtype":"mcp_message"}}'
            IFS= read -r line
            has '"request_id":"r1"' '"subtype":"error"'
            echo '{"type":"result","is_error":true,"result":"boom"}'
        "#,
            turn(PermissionMode::Manual),
        );
        assert_eq!(events.recv_blocking().unwrap(), AgentEvent::Done { error: Some("boom".into()) });
    }
}

#[cfg(test)]
mod live {
    use std::path::Path;
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kiln-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn turn(dir: &Path, prompt: &str, resume: Option<String>) -> Turn {
        Turn {
            dir: dir.to_path_buf(),
            prompt: prompt.into(),
            attachments: vec![],
            resume,
            model: None,
            effort: None,
            permission_mode: PermissionMode::Manual,
            policy: Policy::default(),
        }
    }

    /// Answer every live request with `allow`, and collect the turn's events.
    fn answer_all(turn: Turn, allow: bool) -> Vec<AgentEvent> {
        let (_process, client, rx) = run(turn).unwrap();
        let mut events = vec![];
        while let Ok(event) = rx.recv_blocking() {
            if let AgentEvent::ApprovalRequested(request) = &event {
                client.respond(request, allow).unwrap();
            }
            events.push(event);
        }
        events
    }

    /// A denied command does not run; an approved one runs in the same turn.
    #[test]
    #[ignore]
    fn live_approval_within_the_turn() {
        let dir = temp("approval");
        let prompt = "run the shell command: touch approved.txt";
        let denied = answer_all(turn(&dir, prompt, None), false);
        assert!(!dir.join("approved.txt").exists());
        if !denied.iter().any(|e| matches!(e, AgentEvent::ApprovalRequested(_))) {
            // Claude's own rules decided not to ask in this directory; nothing to approve.
            eprintln!("no request in this directory; skipping approval half");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let approved = answer_all(turn(&dir, prompt, None), true);
        assert_eq!(approved.last(), Some(&AgentEvent::Done { error: None }));
        assert!(dir.join("approved.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Stop a turn while its shell command runs: the process and the command end,
    /// and the session still resumes.
    #[test]
    #[ignore]
    fn live_stop_then_resume() {
        let dir = temp("stop");
        std::fs::write(dir.join("slow.sh"), "sleep 20\ntouch finished.txt\n").unwrap();
        let prompt = "run this shell command in the foreground: bash slow.sh";
        let (process, client, rx) = run(turn(&dir, prompt, None)).unwrap();
        let mut session = None;
        loop {
            match rx.recv_blocking().expect("ended before running a tool") {
                AgentEvent::Session(id) => session = Some(id),
                AgentEvent::ApprovalRequested(request) => client.respond(&request, true).unwrap(),
                AgentEvent::Tool { .. } => break,
                _ => {}
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
        client.stop(&process);
        let after: Vec<_> = std::iter::from_fn(|| rx.recv_blocking().ok()).collect();
        assert_eq!(after.last(), Some(&AgentEvent::Stopped));
        // Long enough for the command to finish, had it been left running.
        std::thread::sleep(std::time::Duration::from_secs(22));
        assert!(!dir.join("finished.txt").exists());

        let events = answer_all(turn(&dir, "reply with the single word: ok", session), false);
        assert_eq!(events.last(), Some(&AgentEvent::Done { error: None }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Needs the `claude` CLI and network: `cargo test -p kiln-agent -- --ignored`.
    #[test]
    #[ignore]
    fn live_turn_streams_session_text_and_done() {
        let prompt = "reply with the words one to ten, one per line, and no tools";
        let events = answer_all(turn(&std::env::temp_dir(), prompt, None), false);
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Session(_))));
        let streamed: String = events.iter()
            .filter_map(|e| match e { AgentEvent::TextDelta(t) => Some(t.as_str()), _ => None })
            .collect();
        let text = events.iter().find_map(|e| match e { AgentEvent::Text(t) => Some(t), _ => None });
        assert_eq!(Some(&streamed), text, "the deltas spell out the whole message");
        assert_eq!(events.last(), Some(&AgentEvent::Done { error: None }));
    }
}
