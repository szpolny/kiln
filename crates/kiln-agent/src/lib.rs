//! Adapters that run coding agents as local child processes.

pub mod claude;

/// Provider-neutral event stream a thread consumes.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    Session(String),
    Text(String),
    Tool(String),
    /// The provider refused an action pending human approval.
    Denied { tool: String, summary: String, rule: String },
    /// The turn finished. `error` carries a message when it failed.
    Done { error: Option<String> },
}
