use super::agent::{Agent, Effort};
use super::message::{Attachment, Message, Prompt, Role};
use crate::checkout::Checkout;
use crate::policy::{PermissionMode, Policy};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// What resuming a stopped thread says to its agent, and shows in the thread.
pub const CONTINUE_PROMPT: &str = "Continue working on my last request.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Running,
    NeedsInput,
    Idle,
    Stopped,
}

pub const NEW_THREAD_TITLE: &str = "New thread";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thread {
    /// Stable across deletes, so a running turn never points at the wrong thread.
    #[serde(default)]
    pub id: u64,
    pub title: String,
    #[serde(default)]
    pub agent: Agent,
    /// Model requested for the next turn; `None` means the agent's default model.
    #[serde(default)]
    pub model: Option<String>,
    /// Reasoning level requested for the next turn; `None` means the agent's
    /// default level.
    #[serde(default)]
    pub effort: Option<Effort>,
    /// Model the agent reported for its most recent turn.
    #[serde(default)]
    pub reported_model: Option<String>,
    pub status: Status,
    #[serde(default)]
    pub permission_mode: PermissionMode,
    pub policy: Policy,
    pub messages: Vec<Message>,
    /// Provider session id, used to resume the conversation.
    pub session_id: Option<String>,
    /// Last activity in Unix seconds; 0 when unknown.
    #[serde(default)]
    pub updated_at: u64,
    /// The user set the thread aside as done with.
    #[serde(default)]
    pub settled: bool,
    /// What the user typed in the composer and has not sent yet.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub draft: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub draft_attachments: Vec<Attachment>,
    /// Where the thread's turns run.
    #[serde(default, skip_serializing_if = "is_project_checkout")]
    pub checkout: Checkout,
    /// The thread changed checkout, so the next turn starts a new session
    /// and recaps the conversation to it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub handoff: bool,
    /// Follow-ups saved during a turn, sent in order once a turn ends well.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queue: Vec<Prompt>,
    /// A turn ended while the thread was out of view, and its result has not
    /// been seen.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unread: bool,
}

fn is_project_checkout(checkout: &Checkout) -> bool {
    *checkout == Checkout::Project
}

impl Thread {
    pub(super) fn new(id: u64, agent: Agent, now: u64) -> Self {
        Self {
            id,
            title: NEW_THREAD_TITLE.into(),
            agent,
            model: None,
            effort: None,
            reported_model: None,
            status: Status::Idle,
            permission_mode: PermissionMode::default(),
            policy: Policy::default(),
            messages: vec![],
            session_id: None,
            updated_at: now,
            settled: false,
            draft: String::new(),
            draft_attachments: vec![],
            checkout: Checkout::Project,
            handoff: false,
            queue: vec![],
            unread: false,
        }
    }

    /// Run later turns in `checkout`. The provider session belongs to the old
    /// folder, so the next turn starts a new one.
    pub fn move_to(&mut self, checkout: Checkout) {
        self.checkout = checkout;
        self.session_id = None;
        self.handoff = !self.messages.is_empty();
    }

    /// Model the next turn runs with.
    pub fn model(&self) -> &str {
        self.model.as_deref().unwrap_or(self.agent.default_model())
    }

    /// Reasoning level the next turn runs with.
    pub fn effort(&self) -> Effort {
        self.effort.filter(|effort| self.agent.efforts(self.model()).contains(effort))
            .unwrap_or(self.agent.default_effort())
    }

    pub fn can_change_agent(&self) -> bool {
        self.status == Status::Idle && self.messages.is_empty()
            && self.session_id.is_none()
    }

    /// Nothing was sent or typed, so leaving the thread loses nothing.
    pub fn is_untouched(&self) -> bool {
        self.can_change_agent() && self.draft.trim().is_empty() && self.draft_attachments.is_empty()
    }

    /// Record activity at `now`, which makes a settled thread active again.
    pub fn touch(&mut self, now: u64) {
        self.updated_at = now;
        self.settled = false;
    }

    /// Whether nothing is running, so nothing can be waiting for the user.
    pub fn can_settle(&self) -> bool {
        self.status != Status::Running
    }

    /// Set the thread aside, which also dismisses a failed turn. Refused while
    /// the thread still needs attention.
    pub fn settle(&mut self) -> bool {
        if !self.can_settle() {
            return false;
        }
        self.status = Status::Idle;
        self.settled = true;
        true
    }

    /// Count time the agent ran toward the latest user message. A resumed
    /// turn adds to it.
    pub fn record_work(&mut self, ran: Duration) {
        if let Some(m) = self.messages.iter_mut().rev().find(|m| m.role == Role::User) {
            let ms = u64::try_from(ran.as_millis()).unwrap_or(u64::MAX);
            m.worked_ms = m.worked_ms.saturating_add(ms);
        }
    }

    /// Save a follow-up for after the running turn. Blank text is ignored.
    pub fn enqueue(&mut self, prompt: Prompt) -> bool {
        if prompt.is_empty() {
            return false;
        }
        self.queue.push(prompt);
        true
    }

    /// Take the next queued follow-up, but only when the last turn ended well.
    /// A failed, stopped, or still running turn keeps the queue for the user.
    pub fn next_queued(&mut self) -> Option<Prompt> {
        (self.status == Status::Idle && !self.queue.is_empty()).then(|| self.queue.remove(0))
    }

    /// End a turn the user stopped.
    pub fn stop_turn(&mut self) {
        self.status = Status::Stopped;
        self.messages.push(Message::notice("Stopped"));
    }

    /// Ask the provider session to carry on, or send the latest request again
    /// if the turn stopped before there was a session to resume.
    pub fn resume_prompt(&self) -> Option<Prompt> {
        if self.status != Status::Stopped {
            return None;
        }
        let request = self.messages.iter().rev()
            .find(|m| m.role == Role::User && m.body != CONTINUE_PROMPT)?;
        Some(if self.session_id.is_some() {
            Prompt { body: CONTINUE_PROMPT.into(), attachments: vec![] }
        } else {
            Prompt { body: request.body.clone(), attachments: request.attachments.clone() }
        })
    }
}
