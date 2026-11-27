use super::Turn;
use crate::{project, visualization, AgentEvent, ApprovalId, ApprovalRequest, Reply};
use kiln_core::policy::{capability_for_tool, Capability, Decision, PermissionMode, Place};
use kiln_core::{Attachment, AttachmentContent};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Parse one stream-json line into zero or more events.
pub(super) fn parse_line(line: &str) -> Vec<AgentEvent> {
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
        // Partial text of the main agent's message; a subagent's would interleave.
        Some("stream_event") if v["parent_tool_use_id"].is_null() => {
            let event = &v["event"];
            if event["type"] == "content_block_delta" && event["delta"]["type"] == "text_delta" {
                if let Some(t) = event["delta"]["text"].as_str() {
                    out.push(AgentEvent::TextDelta(t.to_string()));
                }
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
                    Some("image") => {
                        let source = &block["source"];
                        let image = match (source["media_type"].as_str(), source["data"].as_str()) {
                            (Some(mime), Some(data)) if source["type"] == "base64" => {
                                crate::reply_image(mime, data)
                            }
                            _ => AgentEvent::Notice("This reply contains an unsupported image.".into()),
                        };
                        out.push(image);
                    }
                    Some("tool_use") => {
                        if let Some(n) = block["name"].as_str().filter(|n| *n != visualization::CLAUDE_TOOL) {
                            let input = &block["input"];
                            out.push(AgentEvent::Tool {
                                name: n.to_string(),
                                input: describe_input(input),
                                command_bytes: input.get("command").and_then(Value::as_str).map(str::len),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    out
}

/// Longest argument value shown, in characters. Commands are never cut.
const MAX_ARG_CHARS: usize = 2000;

/// A tool call's arguments as text: the shell command first and in full, then
/// one `name: value` line per remaining argument.
pub(super) fn describe_input(input: &Value) -> String {
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

pub(super) fn user_message(text: &str, attachments: &[Attachment]) -> Value {
    let text = crate::prompt_text(text, attachments);
    let images = attachments.iter().filter_map(|attachment| match &attachment.content {
        AttachmentContent::Image { mime, base64 } => Some(json!({
            "type": "image", "source": { "type": "base64", "media_type": mime, "data": base64.as_ref() }
        })),
        AttachmentContent::Text(_) | AttachmentContent::Visualization { .. } => None,
    }).collect::<Vec<_>>();
    let content = if images.is_empty() {
        json!(text)
    } else {
        let mut blocks = vec![];
        if !text.is_empty() {
            blocks.push(json!({ "type": "text", "text": text }));
        }
        blocks.extend(images);
        json!(blocks)
    };
    json!({ "type": "user", "message": { "role": "user", "content": content } })
}

/// A permission request as shown to the user, and what the policy says about it.
pub(super) fn approval(id: &Value, request: &Value, turn: &Turn) -> (ApprovalRequest, Decision, &'static str) {
    let tool = request["tool_name"].as_str().unwrap_or("Tool");
    let input = &request["input"];
    let place = target(tool, input).map_or(Place::Outside, |path| {
        let resolved = project::inside(&turn.dir, &path);
        Place::of(resolved.as_deref(), path.strip_prefix(&turn.dir).unwrap_or(&path))
    });
    let capability = capability_for_tool(tool, place);
    let why = match capability {
        Capability::OutsideFiles => "Blocked outside the project",
        _ => "Blocked by policy",
    };
    let request = ApprovalRequest {
        id: ApprovalId(id.clone()),
        tool: tool.to_string(),
        summary: describe_input(input),
        reply: Reply::Input(input.clone()),
    };
    (request, turn.policy.decide(capability), why)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_payload_keeps_text_and_image_bytes() {
        let attachments = [
            Attachment { name: "notes.txt".into(), content: AttachmentContent::Text("sample".into()) },
            Attachment {
                name: "screenshot.png".into(),
                content: AttachmentContent::Image { mime: "image/png".into(), base64: "AA==".into() },
            },
        ];
        let message = user_message("Look at these", &attachments);
        let blocks = message["message"]["content"].as_array().unwrap();
        assert!(blocks[0]["text"].as_str().unwrap().contains("notes.txt"));
        assert!(blocks[0]["text"].as_str().unwrap().ends_with("sample"));
        assert_eq!(blocks[1], json!({
            "type": "image",
            "source": { "type": "base64", "media_type": "image/png", "data": "AA==" }
        }));
        let reply = parse_line(&json!({"type": "assistant", "message": {"content": [blocks[1]]}}).to_string());
        assert!(matches!(reply.as_slice(), [AgentEvent::Image(_)]));
    }

    #[test]
    fn parses_init_text_and_tool() {
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
                    input: "$ ls -la\ndescription: List files".into(),
                    command_bytes: Some(6),
                }
            ]
        );
    }

    #[test]
    fn streams_only_the_main_agents_text() {
        let delta = |parent: &str, kind: &str| {
            parse_line(&format!(
                r#"{{"type":"stream_event","parent_tool_use_id":{parent},"event":{{"type":"content_block_delta","index":0,"delta":{{"type":"{kind}","text":"he","partial_json":"{{"}}}}}}"#
            ))
        };
        assert_eq!(delta("null", "text_delta"), vec![AgentEvent::TextDelta("he".into())]);
        assert!(delta(r#""toolu_1""#, "text_delta").is_empty());
        assert!(delta("null", "input_json_delta").is_empty());
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
