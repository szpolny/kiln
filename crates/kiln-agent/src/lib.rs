//! Adapters that run coding agents as local child processes.

pub mod claude;
pub mod codex;
pub mod process;
pub mod project;
pub mod worktree;

use kiln_core::{Attachment, AttachmentContent};
use process::Process;
use serde_json::Value;
use std::io;

/// Provider-neutral event stream a thread consumes.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    Session(String),
    /// The model the provider says is running this turn.
    Model(String),
    /// The permission mode the provider reports at startup.
    PermissionMode(kiln_core::policy::PermissionMode),
    /// A finished message of the agent's reply.
    Text(String),
    /// Image output supplied directly by the provider.
    Image(Attachment),
    /// Generated output saved by the provider on this machine.
    ImageFile(std::path::PathBuf),
    /// Text of the message being written, ahead of its `Text`, which repeats it whole.
    TextDelta(String),
    /// A tool call: the provider's tool name and what it was called with.
    Tool {
        name: String,
        input: String,
        /// Length of the shell source after the displayed `$ ` prompt.
        command_bytes: Option<usize>,
    },
    /// A live request that the provider is waiting for Kiln to answer.
    ApprovalRequested(ApprovalRequest),
    /// The provider cleared a live request, for example on interruption.
    ApprovalResolved(ApprovalId),
    Notice(String),
    /// The turn finished. `error` carries a message when it failed.
    Done { error: Option<String> },
    /// The turn was stopped on request and its process has exited.
    Stopped,
}

/// Names a live request within its turn.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalId(pub(crate) Value);

/// An action the provider holds until the user allows or denies it.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalRequest {
    pub id: ApprovalId,
    pub tool: String,
    pub summary: String,
    pub(crate) reply: Reply,
}

/// What an answer must carry back to the provider.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Reply {
    /// A plain accept or decline.
    Decision,
    /// Only the permissions that were displayed may be granted.
    Permissions(Value),
    /// The tool runs with exactly the input that was displayed.
    Input(Value),
}

fn reply_image(mime: &str, base64: &str) -> AgentEvent {
    if base64.len() > 14 * 1024 * 1024 || !matches!(mime, "image/png" | "image/jpeg" | "image/gif" | "image/webp") {
        return AgentEvent::Notice("This reply contains an oversized or unsupported image.".into());
    }
    AgentEvent::Image(Attachment {
        name: "Agent image".into(),
        content: AttachmentContent::Image { mime: mime.into(), base64: base64.into() },
    })
}

/// Text attachments go into the prompt as named content, even outside the checkout.
fn prompt_text(text: &str, attachments: &[Attachment]) -> String {
    use std::fmt::Write;
    let mut prompt = text.to_owned();
    for attachment in attachments {
        if let AttachmentContent::Text(content) = &attachment.content {
            let _ = write!(prompt, "\n\nAttached text file {:?} ({} bytes):\n{content}",
                attachment.name, content.len());
        }
    }
    prompt
}

/// The live connection of a turn, for answering its requests and stopping it.
#[derive(Clone)]
pub enum Client {
    Claude(claude::Client),
    Codex(codex::Client),
}

impl Client {
    pub fn respond(&self, request: &ApprovalRequest, allow: bool) -> io::Result<()> {
        match self {
            Client::Claude(client) => client.respond(request, allow),
            Client::Codex(client) => client.respond(request, allow),
        }
    }

    /// Send guidance to the running turn. Both providers fold it into the turn.
    pub fn steer(&self, text: &str, attachments: &[Attachment]) -> io::Result<()> {
        match self {
            Client::Claude(client) => client.steer_with(text, attachments),
            Client::Codex(client) => client.steer_with(text, attachments),
        }
    }

    /// Ask the agent to stop through its protocol, then end the process.
    pub fn stop(&self, process: &Process) {
        match self {
            Client::Claude(client) => client.stop(process),
            Client::Codex(client) => client.stop(process),
        }
    }
}
