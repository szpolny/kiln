use crate::policy::PermissionMode;
use serde::{Deserialize, Serialize};

/// Which provider runs a thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Agent {
    #[default]
    Claude,
    Codex,
}

const CODEX_DEFAULT_MODEL: &str = "gpt-6.2-sol";

// Visible models reported by Codex CLI 0.171.0's model/list.
const CODEX_MODELS: &[Model] = &[
    Model { id: CODEX_DEFAULT_MODEL, name: "GPT-6.2-Sol", alias: "default" },
    Model::named("gpt-6.2-astra", "GPT-6.2-Astra"),
    Model::named("gpt-6.1-terra", "GPT-6.1 Terra"),
    Model::named("gpt-6.1-sol", "GPT-6.1-Sol"),
    Model::named("gpt-6-astra", "GPT-6-Astra"),
    Model::named("gpt-6-sol", "GPT-6-Sol"),
    Model::named("gpt-6-luna", "GPT-6-Luna"),
    Model::named("gpt-5.6-sol", "GPT-5.6-Sol"),
    Model::named("gpt-5.6-terra", "GPT-5.6-Terra"),
    Model::named("gpt-5.6-luna", "GPT-5.6-Luna"),
    Model::named("gpt-5.5", "GPT-5.5"),
];

impl Agent {
    pub const ALL: [Self; 2] = [Self::Claude, Self::Codex];

    pub fn label(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Codex => "Codex",
        }
    }

    pub fn permission_modes(self) -> &'static [PermissionMode] {
        &[PermissionMode::Manual, PermissionMode::Auto, PermissionMode::FullAccess]
    }

    /// Model choices shown in the composer. The list is available offline.
    pub fn models(self) -> &'static [Model] {
        match self {
            Agent::Claude => &[
                Model { id: "claude-fable-5-2", name: "Fable 5.2", alias: "fable" },
                Model { id: "claude-opus-5-5", name: "Opus 5.5", alias: "opus" },
                Model { id: "claude-sonnet-5-5", name: "Sonnet 5.5", alias: "sonnet" },
                Model { id: "claude-haiku-5-5", name: "Haiku 5.5", alias: "haiku" },
            ],
            Agent::Codex => CODEX_MODELS,
        }
    }

    /// Model used until the user picks one.
    pub fn default_model(self) -> &'static str {
        match self {
            Agent::Claude => "claude-opus-5-5",
            Agent::Codex => CODEX_DEFAULT_MODEL,
        }
    }

    /// Reasoning levels offered for this model, lowest first.
    pub fn efforts(self, model: &str) -> &'static [Effort] {
        match (self, model) {
            (Agent::Codex, "gpt-5.5") => &[Effort::Low, Effort::Medium, Effort::High, Effort::XHigh],
            _ => &[Effort::Low, Effort::Medium, Effort::High, Effort::XHigh, Effort::Max],
        }
    }

    /// Reasoning level used until the user picks one.
    pub fn default_effort(self) -> Effort {
        Effort::Medium
    }

    /// Display name for a model id, or the id itself when it is not listed.
    pub fn model_name(self, id: &str) -> &str {
        self.models().iter().find(|m| m.id == id).map_or(id, |m| m.name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Model {
    /// Exact id passed to the provider.
    pub id: &'static str,
    pub name: &'static str,
    /// Moving alias earlier versions stored; mapped to `id` on load.
    pub alias: &'static str,
}

impl Model {
    const fn named(id: &'static str, name: &'static str) -> Self {
        Self { id, name, alias: id }
    }
}

/// How much the model reasons before it answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl Effort {
    /// Value passed to the agent.
    pub fn id(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::XHigh => "xhigh",
            Effort::Max => "max",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Effort::Low => "Low",
            Effort::Medium => "Medium",
            Effort::High => "High",
            Effort::XHigh => "Extra high",
            Effort::Max => "Max",
        }
    }
}
