//! Adapters that run coding agents as local child processes.

pub mod claude;
pub mod codex;
pub mod process;
pub mod project;
pub mod worktree;

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
    Text(String),
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

    /// Ask the agent to stop through its protocol, then end the process.
    pub fn stop(&self, process: &Process) {
        match self {
            Client::Claude(client) => client.stop(process),
            Client::Codex(client) => client.stop(process),
        }
    }
}
