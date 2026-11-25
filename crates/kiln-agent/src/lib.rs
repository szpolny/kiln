//! Adapters that run coding agents as local child processes.

pub mod claude;
pub mod codex;
pub mod process;
pub mod project;

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
    Tool { name: String, input: String },
    /// A live request that the provider is waiting for Kiln to answer.
    ApprovalRequested(codex::ApprovalRequest),
    /// The provider cleared a live request, for example on interruption.
    ApprovalResolved(codex::ApprovalId),
    Notice(String),
    /// The provider refused an action pending human approval.
    Denied {
        tool: String,
        summary: String,
        rule: String,
        /// What a file tool reaches; `None` for other tools or when unknown.
        path: Option<std::path::PathBuf>,
    },
    /// The turn finished. `error` carries a message when it failed.
    Done { error: Option<String> },
    /// The turn was stopped on request and its process has exited.
    Stopped,
}
