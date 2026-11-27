//! The local MCP tool shared by the two agent adapters.

use serde_json::{Value, json};
use std::io::{self, BufRead, Write};
use std::sync::Arc;

pub(crate) const SERVER: &str = "kiln_visualization";
pub(crate) const TOOL: &str = "publish_visualization";
pub(crate) const CLAUDE_TOOL: &str = "mcp__kiln_visualization__publish_visualization";
pub const SERVER_ARG: &str = "--visualization-mcp";
pub(crate) const ACCEPTED: &str = "Visualization submitted for display in the conversation.";
const MAX_HTML: usize = 512 * 1024;
const MAX_TITLE: usize = 120;

#[derive(Debug, Clone, PartialEq)]
pub struct Visualization {
    pub title: String,
    pub html: Arc<str>,
}

impl Visualization {
    pub(crate) fn parse(args: &Value) -> Result<Self, &'static str> {
        let html = args["html"].as_str().ok_or("Provide HTML as a string.")?;
        if html.len() > MAX_HTML || html.trim().is_empty() || html.contains('\0') {
            return Err("Provide non-empty UTF-8 HTML of at most 512 KiB with no NUL bytes.");
        }
        let title = match args.get("title") {
            None => "Visualization",
            Some(title) => title.as_str().ok_or("Provide the title as a string.")?.trim(),
        };
        if title.chars().count() > MAX_TITLE || title.chars().any(char::is_control) {
            return Err("Provide a single-line title of at most 120 characters.");
        }
        Ok(Self {
            title: if title.is_empty() { "Visualization" } else { title }.into(),
            html: html.into(),
        })
    }
}

pub fn serve(input: impl BufRead, mut output: impl Write) -> io::Result<()> {
    for line in input.lines() {
        let Ok(request) = serde_json::from_str::<Value>(&line?) else { continue };
        let Some(id) = request.get("id") else { continue };
        let params = &request["params"];
        let result = match request["method"].as_str() {
            Some("initialize") => json!({
                "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2024-11-05"),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": SERVER, "version": env!("CARGO_PKG_VERSION") },
            }),
            Some("ping") => json!({}),
            Some("tools/list") => json!({ "tools": [{
                "name": TOOL,
                "description": "Publish a self-contained HTML chart, diagram, simulation, or other visual into the conversation. Kiln supplies light and dark themes, system typography, and CSS variables --kiln-bg, --kiln-text, --kiln-muted, --kiln-border, --kiln-fill, --kiln-accent, and --kiln-on-accent. Use these for colors (including SVG and canvas), keep the body background and typography inherited, and avoid cards, large headings, and duplicate titles. The inline viewport is 640 by 360 CSS pixels; keep the main visual within it and let the layout expand in the interactive viewer. Embed any assets and use inline CSS and JavaScript so it works offline.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "html": { "type": "string", "maxLength": MAX_HTML },
                        "title": { "type": "string", "maxLength": MAX_TITLE }
                    },
                    "required": ["html"],
                    "additionalProperties": false
                },
                "annotations": { "destructiveHint": false, "openWorldHint": false }
            }]}),
            Some("tools/call") if params["name"] == TOOL => {
                let result = Visualization::parse(&params["arguments"]);
                json!({
                    "isError": result.is_err(),
                    "content": [{ "type": "text", "text": result.err().unwrap_or(ACCEPTED) }]
                })
            }
            _ => {
                writeln!(output, "{}", json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": { "code": -32601, "message": "Unknown visualization tool request" }
                }))?;
                output.flush()?;
                continue;
            }
        };
        writeln!(output, "{}", json!({ "jsonrpc": "2.0", "id": id, "result": result }))?;
        output.flush()?;
    }
    Ok(())
}
