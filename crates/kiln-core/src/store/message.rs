use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    User,
    Agent,
    /// Written by Kiln itself: errors and approval outcomes.
    Notice,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    pub content: AttachmentContent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttachmentContent {
    Text(Arc<str>),
    Image { mime: String, base64: Arc<str> },
    /// A PDF, which agents read from its bytes rather than as pictures.
    Document { mime: String, base64: Arc<str> },
    Visualization {
        html: Arc<str>,
        preview: Result<Arc<str>, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dark_preview: Option<Arc<str>>,
    },
}

impl Attachment {
    pub fn stored_bytes(&self) -> usize {
        match &self.content {
            AttachmentContent::Text(text) => text.len(),
            AttachmentContent::Image { base64, .. } | AttachmentContent::Document { base64, .. } => base64.len(),
            AttachmentContent::Visualization { html, preview, dark_preview } => {
                html.len() + preview.as_ref().map_or_else(|error| error.len(), |png| png.len())
                    + dark_preview.as_ref().map_or(0, |png| png.len())
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "StoredPrompt")]
pub struct Prompt {
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StoredPrompt {
    Text(String),
    WithAttachments { body: String, #[serde(default)] attachments: Vec<Attachment> },
}

impl From<StoredPrompt> for Prompt {
    fn from(value: StoredPrompt) -> Self {
        match value {
            StoredPrompt::Text(body) => Self { body, attachments: vec![] },
            StoredPrompt::WithAttachments { body, attachments } => Self { body, attachments },
        }
    }
}

impl Prompt {
    pub fn is_empty(&self) -> bool {
        self.body.trim().is_empty() && self.attachments.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout_dir: Option<PathBuf>,
    /// Set when this is a tool call: the tool's name, with `body` its input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// Length of the shell source after the displayed `$ ` prompt, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_bytes: Option<usize>,
    /// When it arrived, in Unix seconds; 0 when unknown.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub at: u64,
    /// On a user message, how long the agent has run to answer it, in
    /// milliseconds; 0 when unknown.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub worked_ms: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl Message {
    fn new(role: Role, body: impl Into<String>) -> Self {
        Self { role, body: body.into(), attachments: vec![], checkout_dir: None, tool: None, command_bytes: None, at: 0, worked_ms: 0 }
    }

    pub fn user(body: impl Into<String>) -> Self {
        Self::new(Role::User, body)
    }

    pub fn agent(body: impl Into<String>) -> Self {
        Self::new(Role::Agent, body)
    }

    pub fn notice(body: impl Into<String>) -> Self {
        Self::new(Role::Notice, body)
    }

    pub fn tool_call(name: impl Into<String>, input: impl Into<String>) -> Self {
        Self { tool: Some(name.into()), ..Self::agent(input) }
    }

    /// The same message, arrived at `now`.
    pub fn at(self, now: u64) -> Self {
        Self { at: now, ..self }
    }
}
