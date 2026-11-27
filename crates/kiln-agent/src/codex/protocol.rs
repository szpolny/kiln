use super::Turn;
use crate::{project, visualization, AgentEvent, ApprovalId, ApprovalRequest, Reply};
use kiln_core::policy::{Capability, Decision, PermissionMode, Place};
use kiln_core::{Attachment, AttachmentContent};
use serde_json::{json, Value};
use std::{collections::HashMap, io, path::Path};

pub(super) fn turn_input(text: &str, attachments: &[Attachment]) -> Vec<Value> {
    let text = crate::prompt_text(text, attachments);
    let mut input = vec![];
    if !text.is_empty() {
        input.push(json!({ "type": "text", "text": text, "text_elements": [] }));
    }
    input.extend(attachments.iter().filter_map(|attachment| match &attachment.content {
        AttachmentContent::Image { mime, base64 } => Some(json!({
            "type": "image", "url": format!("data:{mime};base64,{base64}")
        })),
        AttachmentContent::Text(_) | AttachmentContent::Visualization { .. } => None,
    }));
    input
}

pub(super) fn response(request: &ApprovalRequest, allow: bool) -> Value {
    let result = match &request.reply {
        Reply::Permissions(permissions) => json!({
            "permissions": if allow { permissions.clone() } else { json!({}) },
            "scope": "turn",
        }),
        _ => json!({ "decision": if allow { "accept" } else { "decline" } }),
    };
    json!({ "id": request.id.0, "result": result })
}

pub(super) fn thread_request(turn: &Turn) -> Value {
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

pub(super) fn turn_result(turn: &Value) -> io::Result<()> {
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

pub(super) fn notification(method: &str, params: &Value) -> Option<AgentEvent> {
    let item = &params["item"];
    let kind = item["type"].as_str();
    let (name, input) = match (method, kind) {
        ("item/agentMessage/delta", _) => {
            return Some(AgentEvent::TextDelta(params["delta"].as_str()?.to_string()));
        }
        ("item/completed", Some("agentMessage")) => {
            return Some(AgentEvent::Text(item["text"].as_str()?.to_string()));
        }
        ("item/completed", Some("imageGeneration")) => {
            if let Some(data) = item["result"].as_str().filter(|s| !s.is_empty()) {
                // Older versions send the bytes inline.
                let (mime, data) = data.strip_prefix("data:")
                    .and_then(|url| url.split_once(";base64,"))
                    .unwrap_or(("image/png", data));
                return Some(crate::reply_image(mime, data));
            }
            return Some(match item["savedPath"].as_str() {
                Some(path) => AgentEvent::ImageFile(path.into()),
                None => AgentEvent::Notice("The generated image is unavailable.".into()),
            });
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
        (_, Some("mcpToolCall"))
            if item["server"] == visualization::SERVER && item["tool"] == visualization::TOOL =>
        {
            return (method == "item/completed" && item["status"] == "completed"
                && item["result"]["content"][0]["text"] == visualization::ACCEPTED)
                .then(|| visualization::Visualization::parse(&item["arguments"]).ok())
                .flatten()
                .map(AgentEvent::Visualization);
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

pub(super) fn approval(
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
    use crate::codex::tests::turn;

    #[test]
    fn attachment_payload_keeps_text_and_image_bytes() {
        let attachments = [
            Attachment { name: "notes.txt".into(), content: AttachmentContent::Text("sample".into()) },
            Attachment {
                name: "screenshot.png".into(),
                content: AttachmentContent::Image { mime: "image/png".into(), base64: "AA==".into() },
            },
        ];
        let input = turn_input("Look at these", &attachments);
        assert!(input[0]["text"].as_str().unwrap().contains("notes.txt"));
        assert!(input[0]["text"].as_str().unwrap().ends_with("sample"));
        assert_eq!(input[1], json!({"type": "image", "url": "data:image/png;base64,AA=="}));
        let inline = json!({"item": {"type": "imageGeneration", "result": "AA=="}});
        assert!(matches!(notification("item/completed", &inline), Some(AgentEvent::Image(_))));
        let saved = json!({"item": {"type": "imageGeneration", "savedPath": "/example.png"}});
        assert_eq!(notification("item/completed", &saved), Some(AgentEvent::ImageFile("/example.png".into())));
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
            Some(AgentEvent::TextDelta("h".into()))
        );
        assert!(turn_result(&json!({ "status": "completed" })).is_ok());
        let error = json!({ "status": "failed", "error": { "message": "sign in" } });
        assert_eq!(turn_result(&error).unwrap_err().to_string(), "sign in");
    }

    #[test]
    fn publishes_visualizations_after_the_tool_accepts_them() {
        let mut call = json!({ "item": {
            "type": "mcpToolCall", "server": visualization::SERVER, "tool": visualization::TOOL,
            "status": "completed", "arguments": { "html": "<p>Chart</p>" },
            "result": { "content": [{ "type": "text", "text": visualization::ACCEPTED }] }
        }});
        assert_eq!(notification("item/started", &call), None);
        assert_eq!(notification("item/completed", &call), Some(AgentEvent::Visualization(visualization::Visualization {
            title: "Visualization".into(), html: "<p>Chart</p>".into(),
        })));
        call["item"]["status"] = json!("failed");
        assert_eq!(notification("item/completed", &call), None);
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
}
