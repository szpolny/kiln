//! Codex adapter using a local app server over stdio for each turn.

use crate::{
    process::{self, Process},
    project, AgentEvent, ApprovalId, ApprovalRequest, Reply,
};
use kiln_core::policy::{Capability, Decision, PermissionMode, Place, Policy};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command};
use std::sync::{Arc, Mutex};

/// Request id of `turn/steer`; a refusal must not end the turn.
const STEER_ID: u64 = 5;

pub struct Turn {
    pub dir: PathBuf,
    pub prompt: String,
    pub resume: Option<String>,
    pub model: Option<String>,
    pub effort: String,
    pub permission_mode: PermissionMode,
    pub policy: Policy,
}

fn response(request: &ApprovalRequest, allow: bool) -> Value {
    let result = match &request.reply {
        Reply::Permissions(permissions) => json!({
            "permissions": if allow { permissions.clone() } else { json!({}) },
            "scope": "turn",
        }),
        _ => json!({ "decision": if allow { "accept" } else { "decline" } }),
    };
    json!({ "id": request.id.0, "result": result })
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
    pub fn steer(&self, text: &str) -> io::Result<()> {
        let mut connection = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let (thread, turn) = connection
            .turn
            .clone()
            .ok_or_else(|| io::Error::other("Codex turn has not started"))?;
        connection.send(json!({ "id": STEER_ID, "method": "turn/steer", "params": {
            "threadId": thread,
            "input": [{ "type": "text", "text": text, "text_elements": [] }],
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
    cmd
}

fn thread_request(turn: &Turn) -> Value {
    let (approval, reviewer, sandbox) = match turn.permission_mode {
        PermissionMode::Manual => {
            let sandbox = if turn.policy.project_files == Decision::Allow {
                "workspace-write"
            } else {
                "read-only"
            };
            ("untrusted", "user", sandbox)
        }
        PermissionMode::Auto => ("on-request", "auto_review", "workspace-write"),
        PermissionMode::FullAccess => ("never", "user", "danger-full-access"),
    };
    let mut params = json!({
        "cwd": turn.dir,
        "model": turn.model,
        "approvalPolicy": approval,
        "approvalsReviewer": reviewer,
        "sandbox": sandbox,
    });
    if turn.permission_mode == PermissionMode::Manual {
        let web = if turn.policy.network == Decision::Allow {
            "live"
        } else {
            "disabled"
        };
        params["config"] = json!({ "web_search": web });
    }
    let method = if let Some(id) = &turn.resume {
        params["threadId"] = json!(id);
        params["excludeTurns"] = json!(true);
        "thread/resume"
    } else {
        "thread/start"
    };
    json!({ "id": 2, "method": method, "params": params })
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
                    "input": [{ "type": "text", "text": turn.prompt, "text_elements": [] }],
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

fn turn_result(turn: &Value) -> io::Result<()> {
    match turn["status"].as_str() {
        Some("completed") => Ok(()),
        Some("interrupted") => Err(io::Error::other("Codex interrupted the turn")),
        _ => Err(io::Error::other(
            turn["error"]["message"]
                .as_str()
                .unwrap_or("Codex turn failed"),
        )),
    }
}

fn notification(method: &str, params: &Value) -> Option<AgentEvent> {
    let item = &params["item"];
    let kind = item["type"].as_str();
    let (name, input) = match (method, kind) {
        ("item/completed", Some("agentMessage")) => {
            return Some(AgentEvent::Text(item["text"].as_str()?.to_string()));
        }
        ("item/started", Some("commandExecution")) => {
            let command = item["command"].as_str()?;
            return Some(AgentEvent::Tool {
                name: "Shell".into(),
                input: format!("$ {command}"),
                command_bytes: Some(command.len()),
            });
        }
        ("item/started", Some("fileChange")) => {
            ("File change", file_paths(&item["changes"]).join("\n"))
        }
        ("item/started", Some("mcpToolCall" | "dynamicToolCall")) => {
            (item["tool"].as_str()?, item["arguments"].to_string())
        }
        ("item/completed", Some("webSearch")) => {
            ("Web search", item["query"].as_str()?.to_string())
        }
        ("serverRequest/resolved", _) => {
            return Some(AgentEvent::ApprovalResolved(ApprovalId(
                params.get("requestId")?.clone(),
            )));
        }
        _ => return None,
    };
    Some(AgentEvent::Tool {
        name: name.to_string(),
        input,
        command_bytes: None,
    })
}

fn file_paths(changes: &Value) -> Vec<&str> {
    changes
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["path"].as_str())
        .collect()
}

fn file_capability(dir: &Path, path: &str) -> Capability {
    let path = Path::new(path);
    let resolved = project::inside(dir, path);
    match Place::of(resolved.as_deref(), path.strip_prefix(dir).unwrap_or(path)) {
        Place::Inside => Capability::ProjectFiles,
        Place::Control => Capability::Exec,
        Place::Outside => Capability::OutsideFiles,
    }
}

fn approval(
    method: &str,
    id: &Value,
    params: &Value,
    items: &HashMap<String, Value>,
    turn: &Turn,
) -> Option<(ApprovalRequest, Decision)> {
    let mut needed = Vec::new();
    let mut permissions = None;
    let item = params["itemId"].as_str().and_then(|id| items.get(id));
    let (tool, mut summary) = match method {
        "item/commandExecution/requestApproval" => {
            if let Some(host) = params["networkApprovalContext"]["host"].as_str() {
                needed.push(Capability::Network);
                let protocol = params["networkApprovalContext"]["protocol"]
                    .as_str()
                    .unwrap_or("network");
                ("Network access", format!("{host} ({protocol})"))
            } else {
                needed.push(Capability::Exec);
                // Stdin approvals must show the new input, not the original command.
                let command = params["command"].as_str().or_else(|| {
                    if params["kind"] == "writeStdin" {
                        None
                    } else {
                        item?["command"].as_str()
                    }
                })?;
                let cwd = params["cwd"].as_str().or_else(|| item?["cwd"].as_str());
                let summary = if let Some(cwd) = cwd {
                    if file_capability(&turn.dir, cwd) == Capability::OutsideFiles {
                        needed.push(Capability::OutsideFiles);
                    }
                    format!("$ {command}\nWorking directory: {cwd}")
                } else {
                    format!("$ {command}")
                };
                ("Shell", summary)
            }
        }
        "item/fileChange/requestApproval" => {
            let changes = item?["changes"].as_array()?;
            let mut files = Vec::new();
            for change in changes {
                let path = change["path"].as_str()?;
                needed.push(file_capability(&turn.dir, path));
                let diff = change["diff"].as_str().unwrap_or_default();
                files.push(format!("{path}\n{diff}"));
                if let Some(destination) = change["kind"]["move_path"].as_str() {
                    needed.push(file_capability(&turn.dir, destination));
                    files.push(format!("Move to {destination}"));
                }
            }
            if let Some(root) = params["grantRoot"].as_str() {
                needed.push(file_capability(&turn.dir, root));
                files.push(format!("Requested write root: {root}"));
            }
            ("File change", files.join("\n"))
        }
        "item/permissions/requestApproval" => {
            let requested = Value::Object(
                params["permissions"]
                    .as_object()?
                    .iter()
                    .filter(|(key, value)| {
                        matches!(key.as_str(), "network" | "fileSystem") && !value.is_null()
                    })
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            );
            permission_capabilities(&requested, &turn.dir, &mut needed)?;
            let summary = serde_json::to_string_pretty(&requested).ok()?;
            permissions = Some(requested);
            ("Additional permissions", summary)
        }
        _ => return None,
    };
    if let Some(extra) = params
        .get("additionalPermissions")
        .filter(|value| !value.is_null())
    {
        permission_capabilities(extra, &turn.dir, &mut needed)?;
        summary.push_str("\n\n");
        summary.push_str(&serde_json::to_string_pretty(extra).ok()?);
    }
    if let Some(reason) = params["reason"].as_str() {
        summary.push_str("\n\n");
        summary.push_str(reason);
    }
    let decision = if needed.is_empty()
        || needed
            .iter()
            .any(|&c| turn.policy.decide(c) == Decision::Deny)
    {
        Decision::Deny
    } else if needed
        .iter()
        .any(|&c| turn.policy.decide(c) == Decision::Ask)
    {
        Decision::Ask
    } else {
        Decision::Allow
    };
    let request = ApprovalRequest {
        id: ApprovalId(id.clone()),
        tool: tool.to_string(),
        summary,
        reply: permissions.map_or(Reply::Decision, Reply::Permissions),
    };
    Some((request, decision))
}

fn permission_capabilities(
    permissions: &Value,
    dir: &Path,
    needed: &mut Vec<Capability>,
) -> Option<()> {
    if !permissions["network"].is_null() {
        needed.push(Capability::Network);
    }
    let files = &permissions["fileSystem"];
    for name in ["read", "write"] {
        for path in files[name].as_array().into_iter().flatten() {
            needed.push(file_capability(dir, path.as_str()?));
        }
    }
    for entry in files["entries"].as_array().into_iter().flatten() {
        let capability = match entry["path"]["type"].as_str() {
            Some("path") => file_capability(dir, entry["path"]["path"].as_str()?),
            _ => Capability::OutsideFiles,
        };
        needed.push(capability);
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn() -> Turn {
        Turn {
            dir: std::env::current_dir().unwrap().canonicalize().unwrap(),
            prompt: "Continue.".into(),
            resume: None,
            model: None,
            effort: "medium".into(),
            permission_mode: PermissionMode::Manual,
            policy: Policy::default(),
        }
    }

    #[test]
    fn maps_replies_tools_errors_and_resolved_requests() {
        let reply = json!({ "item": { "type": "agentMessage", "text": "hello" } });
        assert_eq!(notification("item/started", &reply), None);
        assert_eq!(
            notification("item/completed", &reply),
            Some(AgentEvent::Text("hello".into()))
        );
        let shell = json!({ "item": { "type": "commandExecution", "command": "pwd" } });
        assert_eq!(
            notification("item/started", &shell),
            Some(AgentEvent::Tool {
                name: "Shell".into(),
                input: "$ pwd".into(),
                command_bytes: Some(3),
            })
        );
        assert_eq!(notification("item/completed", &shell), None);
        assert_eq!(
            notification("serverRequest/resolved", &json!({"requestId":"a"})),
            Some(AgentEvent::ApprovalResolved(ApprovalId(json!("a"))))
        );
        assert_eq!(
            notification("item/agentMessage/delta", &json!({"delta":"h"})),
            None
        );
        assert!(turn_result(&json!({ "status": "completed" })).is_ok());
        let error = json!({ "status": "failed", "error": { "message": "sign in" } });
        assert_eq!(turn_result(&error).unwrap_err().to_string(), "sign in");
    }

    #[test]
    fn applies_policy_and_returns_only_scoped_decisions() {
        let mut turn = turn();
        let id = json!("approval");
        let items = HashMap::new();
        let method = "item/commandExecution/requestApproval";
        let params = json!({ "command": "touch kiln-test", "cwd": turn.dir });
        let (request, decision) = approval(method, &id, &params, &items, &turn).unwrap();
        assert_eq!(decision, Decision::Ask);
        assert!(request.summary.contains("touch kiln-test"));
        assert_eq!(
            response(&request, true),
            json!({ "id": id, "result": { "decision": "accept" } })
        );
        assert_eq!(
            response(&request, false),
            json!({ "id": id, "result": { "decision": "decline" } })
        );

        let params =
            json!({ "networkApprovalContext": { "host": "example.com", "protocol": "https" } });
        turn.policy.network = Decision::Deny;
        assert_eq!(
            approval(method, &id, &params, &items, &turn).unwrap().1,
            Decision::Deny
        );
        turn.policy.network = Decision::Allow;
        let (request, decision) = approval(method, &id, &params, &items, &turn).unwrap();
        assert_eq!(decision, Decision::Allow);
        assert!(request.summary.contains("example.com"));

        let method = "item/permissions/requestApproval";
        let permissions = json!({ "network": { "enabled": true } });
        let params = json!({ "permissions": permissions });
        let (request, decision) = approval(method, &id, &params, &items, &turn).unwrap();
        assert_eq!(decision, Decision::Allow);
        assert_eq!(
            response(&request, true),
            json!({
                "id": id, "result": { "permissions": permissions, "scope": "turn" }
            })
        );
        assert_eq!(
            response(&request, false),
            json!({
                "id": id, "result": { "permissions": {}, "scope": "turn" }
            })
        );

        let params = json!({ "permissions": {
            "network": { "enabled": true },
            "fileSystem": { "write": [turn.dir.join("../outside")] },
        }});
        assert_eq!(
            approval(method, &id, &params, &items, &turn).unwrap().1,
            Decision::Deny
        );
    }

    #[test]
    fn file_approval_checks_every_target_including_moves() {
        let turn = turn();
        let mut items = HashMap::from([(
            "patch".into(),
            json!({ "changes": [{
                "path": "src/example.rs", "diff": "+fn sample() {}", "kind": { "type": "add" }
            }]}),
        )]);
        let params = json!({ "itemId": "patch" });
        let id = json!(3);
        let method = "item/fileChange/requestApproval";
        let (request, decision) = approval(method, &id, &params, &items, &turn).unwrap();
        assert_eq!(decision, Decision::Allow);
        assert!(request.summary.contains("+fn sample() {}"));
        items.get_mut("patch").unwrap()["changes"][0]["kind"] =
            json!({ "type": "update", "move_path": "../outside.rs" });
        assert_eq!(
            approval(method, &id, &params, &items, &turn).unwrap().1,
            Decision::Deny
        );
    }

    #[test]
    fn resume_reapplies_permissions_and_web_settings() {
        let mut turn = turn();
        assert_eq!(thread_request(&turn)["method"], "thread/start");
        turn.resume = Some("session".into());
        let request = thread_request(&turn);
        assert_eq!(request["method"], "thread/resume");
        assert_eq!(request["params"]["threadId"], "session");
        assert_eq!(request["params"]["approvalPolicy"], "untrusted");
        assert_eq!(request["params"]["config"]["web_search"], "disabled");
        turn.policy.network = Decision::Allow;
        assert_eq!(
            thread_request(&turn)["params"]["config"]["web_search"],
            "live"
        );
        turn.permission_mode = PermissionMode::Auto;
        assert_eq!(
            thread_request(&turn)["params"]["approvalsReviewer"],
            "auto_review"
        );
        turn.permission_mode = PermissionMode::FullAccess;
        assert_eq!(
            thread_request(&turn)["params"]["sandbox"],
            "danger-full-access"
        );
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
        client.steer("use tabs").unwrap();
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
                resume: resume.clone(),
                ..turn()
            })
            .unwrap();
            let mut approved = false;
            let mut replied = false;
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
                    AgentEvent::Text(text) => replied |= text.contains("DONE"),
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
