//! Codex adapter using a local app server over stdio for each turn.

mod protocol;

use protocol::{approval, notification, response, thread_request, turn_input, turn_result};
use crate::{
    process::{self, Process},
    visualization, AgentEvent, ApprovalRequest,
};
use kiln_core::policy::{Decision, PermissionMode, Policy};
use kiln_core::Attachment;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{ChildStdin, Command};
use std::sync::{Arc, Mutex};

/// Request id of `turn/steer`; a refusal must not end the turn.
const STEER_ID: u64 = 5;

pub struct Turn {
    pub dir: PathBuf,
    pub prompt: String,
    pub attachments: Vec<Attachment>,
    pub resume: Option<String>,
    pub model: Option<String>,
    pub effort: String,
    pub permission_mode: PermissionMode,
    pub policy: Policy,
}

/// The live connection for replying to approvals and stopping this turn.
#[derive(Clone)]
pub struct Client(Arc<Mutex<Connection>>);

#[derive(Default)]
struct Connection {
    input: Option<ChildStdin>,
    turn: Option<(String, String)>,
}

impl Client {
    pub fn respond(&self, request: &ApprovalRequest, allow: bool) -> io::Result<()> {
        self.send(response(request, allow))
    }

    /// Add guidance to the turn in progress.
    pub(crate) fn steer_with(&self, text: &str, attachments: &[Attachment]) -> io::Result<()> {
        let mut connection = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let (thread, turn) = connection
            .turn
            .clone()
            .ok_or_else(|| io::Error::other("Codex turn has not started"))?;
        connection.send(json!({ "id": STEER_ID, "method": "turn/steer", "params": {
            "threadId": thread,
            "input": turn_input(text, attachments),
            "expectedTurnId": turn,
        }}))
    }

    pub fn stop(&self, process: &Process) {
        process.stop_with(|| {
            let mut connection = self.0.lock().unwrap_or_else(|e| e.into_inner());
            let (thread, turn) = connection
                .turn
                .as_ref()
                .ok_or_else(|| io::Error::other("Codex turn has not started"))?;
            let request = json!({ "id": 4, "method": "turn/interrupt", "params": {
                "threadId": thread, "turnId": turn,
            }});
            connection.send(request)
        });
    }

    fn send(&self, message: Value) -> io::Result<()> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send(message)
    }
}

impl Connection {
    fn send(&mut self, message: Value) -> io::Result<()> {
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| io::Error::other("Codex turn has ended"))?;
        writeln!(input, "{message}")
    }
}

pub fn run(mut turn: Turn) -> io::Result<(Process, crate::Client, async_channel::Receiver<AgentEvent>)> {
    turn.dir = turn.dir.canonicalize()?;
    let (process, client, events) = run_command(command(&turn), turn)?;
    Ok((process, crate::Client::Codex(client), events))
}

fn run_command(
    cmd: Command,
    turn: Turn,
) -> io::Result<(Process, Client, async_channel::Receiver<AgentEvent>)> {
    let client = Client(Arc::default());
    let wire = client.clone();
    let (process, events) = process::spawn_protocol(cmd, move |input, output, events| {
        wire.0.lock().unwrap_or_else(|e| e.into_inner()).input = Some(input);
        let result = drive(&wire, BufReader::new(output), events, turn);
        // EOF on stdin asks the server to shut down.
        wire.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .input
            .take();
        result
    })?;
    Ok((process, client, events))
}

fn command(turn: &Turn) -> Command {
    let mut cmd = Command::new("codex");
    cmd.current_dir(&turn.dir).args([
        "app-server",
        "--listen",
        "stdio://",
        "-c",
        "analytics.enabled=false",
        "-c",
        "feedback.enabled=false",
        "-c",
        "sandbox_workspace_write.network_access=false",
        "-c",
        "sandbox_workspace_write.writable_roots=[]",
        "-c",
        "sandbox_workspace_write.exclude_tmpdir_env_var=true",
        "-c",
        "sandbox_workspace_write.exclude_slash_tmp=true",
    ]);
    if let Ok(executable) = std::env::current_exe() {
        let server = format!("mcp_servers.{}", visualization::SERVER);
        cmd.args([
            "-c", &format!("{server}.command={}", json!(executable.to_string_lossy())),
            "-c", &format!("{server}.args={}", json!([visualization::SERVER_ARG])),
            "-c", &format!("{server}.enabled=true"),
            "-c", &format!("{server}.tools.{}.approval_mode=\"approve\"", visualization::TOOL),
        ]);
    }
    cmd
}

fn drive(
    wire: &Client,
    output: impl BufRead,
    events: &async_channel::Sender<AgentEvent>,
    turn: Turn,
) -> io::Result<()> {
    let emit = |event| events.send_blocking(event).map_err(io::Error::other);
    wire.send(json!({
        "id": 1, "method": "initialize",
        "params": { "clientInfo": { "name": "kiln", "version": env!("CARGO_PKG_VERSION") } },
    }))?;
    let request = thread_request(&turn);
    let mut thread_id = String::new();
    let mut items = HashMap::new();
    for line in output.lines() {
        let Ok(message) = serde_json::from_str::<Value>(&line?) else {
            continue;
        };
        if let Some(method) = message["method"].as_str() {
            let params = &message["params"];
            let current_thread =
                params["threadId"].as_str() == Some(&thread_id) && !thread_id.is_empty();
            if let Some(id) = message
                .get("id")
                .filter(|id| id.is_number() || id.is_string())
            {
                let approval = current_thread
                    .then(|| approval(method, id, params, &items, &turn))
                    .flatten();
                if let Some((request, decision)) = approval {
                    match decision {
                        Decision::Ask => emit(AgentEvent::ApprovalRequested(request))?,
                        Decision::Allow => wire.respond(&request, true)?,
                        Decision::Deny => {
                            wire.respond(&request, false)?;
                            emit(AgentEvent::Notice(format!(
                                "Blocked by policy: {}\n{}",
                                request.tool, request.summary
                            )))?;
                        }
                    }
                } else {
                    // Unsupported requests must receive a response so they cannot stall the turn.
                    wire.send(json!({ "id": id, "error": {
                        "code": -32601, "message": "Request is unsupported by Kiln"
                    }}))?;
                }
                continue;
            }
            if !current_thread {
                continue;
            }
            match method {
                "item/started"
                    if matches!(
                        params["item"]["type"].as_str(),
                        Some("fileChange" | "commandExecution")
                    ) =>
                {
                    if let Some(id) = params["item"]["id"].as_str() {
                        items.insert(id.to_string(), params["item"].clone());
                    }
                }
                "item/completed" => {
                    if let Some(id) = params["item"]["id"].as_str() {
                        items.remove(id);
                    }
                }
                "turn/completed" => return turn_result(&params["turn"]),
                _ => {}
            }
            if let Some(event) = notification(method, params) {
                emit(event)?;
            }
            continue;
        }
        if let (Some(STEER_ID), Some(error)) = (message["id"].as_u64(), message.get("error")) {
            let why = error["message"].as_str().unwrap_or("the turn did not accept it");
            emit(AgentEvent::Notice(format!("Could not steer: {why}")))?;
            continue;
        }
        if let Some(error) = message.get("error") {
            return Err(io::Error::other(
                error["message"].as_str().unwrap_or("Codex request failed"),
            ));
        }
        match message["id"].as_u64() {
            Some(1) => {
                wire.send(json!({ "method": "initialized" }))?;
                wire.send(request.clone())?;
            }
            Some(2) => {
                let result = &message["result"];
                let id = result["thread"]["id"]
                    .as_str()
                    .ok_or_else(|| io::Error::other("Codex did not return a thread id"))?;
                let params = &request["params"];
                let sandbox = match params["sandbox"].as_str() {
                    Some("workspace-write") => "workspaceWrite",
                    Some("read-only") => "readOnly",
                    _ => "dangerFullAccess",
                };
                let reviewer = &result["approvalsReviewer"];
                let auto = |value: &Value| {
                    matches!(value.as_str(), Some("auto_review" | "guardian_subagent"))
                };
                if result["approvalPolicy"] != params["approvalPolicy"]
                    || (reviewer != &params["approvalsReviewer"]
                        && !(auto(reviewer) && auto(&params["approvalsReviewer"])))
                    || result["sandbox"]["type"] != sandbox
                    || (sandbox != "dangerFullAccess"
                        && result["sandbox"]["networkAccess"] != false)
                    || (sandbox == "workspaceWrite"
                        && (result["sandbox"]["writableRoots"] != json!([])
                            || result["sandbox"]["excludeTmpdirEnvVar"] != true
                            || result["sandbox"]["excludeSlashTmp"] != true))
                {
                    return Err(io::Error::other(
                        "Codex did not apply the requested permissions",
                    ));
                }
                thread_id = id.to_string();
                emit(AgentEvent::Session(thread_id.clone()))?;
                if let Some(model) = result["model"].as_str() {
                    emit(AgentEvent::Model(model.to_string()))?;
                }
                wire.send(json!({ "id": 3, "method": "turn/start", "params": {
                    "threadId": id,
                    "input": turn_input(&turn.prompt, &turn.attachments),
                    "effort": turn.effort,
                }}))?;
            }
            Some(3) => {
                let id = message["result"]["turn"]["id"]
                    .as_str()
                    .ok_or_else(|| io::Error::other("Codex did not return a turn id"))?;
                wire.0.lock().unwrap_or_else(|e| e.into_inner()).turn =
                    Some((thread_id.clone(), id.to_string()));
            }
            _ => {}
        }
    }
    Err(io::Error::other("Codex exited without a turn result"))
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn turn() -> Turn {
        Turn {
            dir: std::env::current_dir().unwrap().canonicalize().unwrap(),
            prompt: "Continue.".into(),
            attachments: vec![],
            resume: None,
            model: None,
            effort: "medium".into(),
            permission_mode: PermissionMode::Manual,
            policy: Policy::default(),
        }
    }

    const FIXTURE_HANDSHAKE: &str = r#"
            IFS= read -r line
            case "$line" in *'"method":"initialize"'*) ;; *) exit 1 ;; esac
            echo '{"id":1,"result":{}}'
            IFS= read -r line
            IFS= read -r line
            case "$line" in *'"method":"thread/resume"'*'"threadId":"session"'*) ;; *) exit 1 ;; esac
            echo '{"id":2,"result":{"thread":{"id":"session"},"model":"test","approvalPolicy":"untrusted","approvalsReviewer":"user","sandbox":{"type":"workspaceWrite","networkAccess":false,"writableRoots":[],"excludeTmpdirEnvVar":true,"excludeSlashTmp":true}}}'
            IFS= read -r line
            case "$line" in *'"method":"turn/start"'*'"threadId":"session"'*) ;; *) exit 1 ;; esac
            echo '{"id":3,"result":{"turn":{"id":"turn"}}}'
        "#;

    fn fixture(script: &str) -> (Process, Client, async_channel::Receiver<AgentEvent>) {
        let mut turn = turn();
        turn.resume = Some("session".into());
        let mut cmd = Command::new("sh");
        cmd.args(["-c", &format!("{FIXTURE_HANDSHAKE}\n{script}")]);
        let (process, client, events) = run_command(cmd, turn).unwrap();
        assert_eq!(
            events.recv_blocking().unwrap(),
            AgentEvent::Session("session".into())
        );
        assert_eq!(
            events.recv_blocking().unwrap(),
            AgentEvent::Model("test".into())
        );
        (process, client, events)
    }

    #[test]
    fn live_approval_round_trip_resumes_the_same_turn() {
        let (_, client, events) = fixture(
            r#"
            echo '{"method":"item/commandExecution/requestApproval","id":7,"params":{"threadId":"session","turnId":"turn","command":"touch denied"}}'
            IFS= read -r line
            test "$line" = '{"id":7,"result":{"decision":"decline"}}' || exit 1
            echo '{"method":"serverRequest/resolved","params":{"threadId":"session","requestId":7}}'
            echo '{"method":"item/commandExecution/requestApproval","id":"8","params":{"threadId":"session","turnId":"turn","command":"touch approved"}}'
            IFS= read -r line
            test "$line" = '{"id":"8","result":{"decision":"accept"}}' || exit 1
            echo '{"method":"serverRequest/resolved","params":{"threadId":"session","requestId":"8"}}'
            echo '{"method":"item/completed","params":{"threadId":"session","item":{"type":"agentMessage","text":"done"}}}'
            echo '{"method":"turn/completed","params":{"threadId":"session","turn":{"status":"completed"}}}'
            if IFS= read -r line; then exit 1; fi
        "#,
        );
        for (expected, allow) in [("touch denied", false), ("touch approved", true)] {
            let AgentEvent::ApprovalRequested(request) = events.recv_blocking().unwrap() else {
                panic!("expected a live approval");
            };
            assert!(request.summary.contains(expected));
            client.respond(&request, allow).unwrap();
            assert_eq!(
                events.recv_blocking().unwrap(),
                AgentEvent::ApprovalResolved(request.id)
            );
        }
        assert_eq!(
            events.recv_blocking().unwrap(),
            AgentEvent::Text("done".into())
        );
        assert_eq!(
            events.recv_blocking().unwrap(),
            AgentEvent::Done { error: None }
        );
        assert!(events.recv_blocking().is_err());
    }

    #[test]
    fn steering_adds_guidance_to_the_running_turn_and_survives_a_refusal() {
        let (_, client, events) = fixture(
            r#"
            echo '{"method":"item/completed","params":{"threadId":"session","item":{"type":"agentMessage","text":"working"}}}'
            IFS= read -r line
            case "$line" in *'"method":"turn/steer"'*) ;; *) exit 1 ;; esac
            case "$line" in *'"expectedTurnId":"turn"'*) ;; *) exit 1 ;; esac
            case "$line" in *'"text":"use tabs"'*) ;; *) exit 1 ;; esac
            echo '{"id":5,"error":{"message":"turn is not steerable"}}'
            echo '{"method":"turn/completed","params":{"threadId":"session","turn":{"status":"completed"}}}'
        "#,
        );
        assert_eq!(events.recv_blocking().unwrap(), AgentEvent::Text("working".into()));
        client.steer_with("use tabs", &[]).unwrap();
        assert_eq!(
            events.recv_blocking().unwrap(),
            AgentEvent::Notice("Could not steer: turn is not steerable".into())
        );
        assert_eq!(events.recv_blocking().unwrap(), AgentEvent::Done { error: None });
    }

    #[test]
    fn stopping_an_approval_sends_a_turn_interrupt() {
        let (process, client, events) = fixture(
            r#"
            echo '{"method":"item/commandExecution/requestApproval","id":7,"params":{"threadId":"session","turnId":"turn","command":"touch denied"}}'
            IFS= read -r line
            case "$line" in *'"method":"turn/interrupt"'*'"threadId":"session","turnId":"turn"'*) ;; *) exit 1 ;; esac
            echo '{"method":"serverRequest/resolved","params":{"threadId":"session","requestId":7}}'
            echo '{"method":"turn/completed","params":{"threadId":"session","turn":{"status":"interrupted"}}}'
            if IFS= read -r line; then exit 1; fi
        "#,
        );
        let AgentEvent::ApprovalRequested(request) = events.recv_blocking().unwrap() else {
            panic!("expected a live approval");
        };
        client.stop(&process);
        assert_eq!(
            events.recv_blocking().unwrap(),
            AgentEvent::ApprovalResolved(request.id)
        );
        assert_eq!(events.recv_blocking().unwrap(), AgentEvent::Stopped);
        assert!(events.recv_blocking().is_err());
    }

    #[test]
    #[ignore = "requires a signed-in Codex CLI and makes live model requests"]
    fn live_codex_runs_resumes_and_approves() {
        let dir = std::env::temp_dir().join(format!("kiln-codex-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(dir);
        let mut resume = None;
        for prompt in [
            "Use the shell to run exactly: touch kiln-approved.txt. When it succeeds, reply DONE.",
            "Reply DONE without using any tools.",
        ] {
            let (_, client, events) = run(Turn {
                dir: scratch.0.clone(),
                prompt: prompt.into(),
            attachments: vec![],
                resume: resume.clone(),
                ..turn()
            })
            .unwrap();
            let mut approved = false;
            let mut replied = false;
            let mut streamed = String::new();
            while let Ok(event) = events.recv_blocking() {
                match event {
                    AgentEvent::Session(id) => {
                        if let Some(expected) = &resume {
                            assert_eq!(&id, expected);
                        }
                        if resume.is_none() {
                            resume = Some(id);
                        }
                    }
                    AgentEvent::ApprovalRequested(request) => {
                        let allow = request.tool == "Shell"
                            && request.summary.contains("touch kiln-approved.txt");
                        client.respond(&request, allow).unwrap();
                        approved |= allow;
                    }
                    AgentEvent::TextDelta(delta) => streamed.push_str(&delta),
                    AgentEvent::Text(text) => {
                        assert_eq!(std::mem::take(&mut streamed), text, "the deltas spell out the message");
                        replied |= text.contains("DONE");
                    }
                    AgentEvent::Done { error } => assert_eq!(error, None),
                    _ => {}
                }
            }
            assert!(replied);
            if prompt.starts_with("Use the shell") {
                assert!(approved);
                assert!(scratch.0.join("kiln-approved.txt").is_file());
            }
        }
    }
}
