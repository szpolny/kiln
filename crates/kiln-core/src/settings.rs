//! App-wide preferences, saved with the rest of the local state.

use crate::checkout::BranchPrefix;
use crate::policy::{Decision, PermissionMode, Policy};
use serde::{Deserialize, Serialize};

/// Which palette the window uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    /// Follow the system's light or dark mode.
    #[default]
    System,
    Light,
    Dark,
}

/// Whether `origin` is fetched before a new worktree is made, which reaches
/// the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Fetch {
    /// The user picks with each worktree they make; the agent's never fetch.
    #[default]
    Ask,
    Always,
    Never,
}

/// What a message typed while a turn runs does by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FollowUp {
    /// Send it to the running turn as guidance.
    Steer,
    /// Keep it for after the turn.
    #[default]
    Queue,
}

impl FollowUp {
    /// The option the default does not pick.
    pub fn other(self) -> Self {
        match self {
            FollowUp::Steer => FollowUp::Queue,
            FollowUp::Queue => FollowUp::Steer,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FollowUp::Steer => "Steer",
            FollowUp::Queue => "Queue",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub appearance: Appearance,
    /// Permission mode new threads start with when supported by their provider.
    pub permission_mode: PermissionMode,
    /// Web access new threads start with.
    pub web: Decision,
    /// What the branches of new worktrees start with.
    pub branch_prefix: BranchPrefix,
    /// Fetching before a new worktree is made.
    pub fetch: Fetch,
    /// What Enter does with a message typed while a turn runs.
    pub follow_up: FollowUp,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            appearance: Appearance::default(),
            permission_mode: PermissionMode::default(),
            web: Policy::default().network,
            branch_prefix: BranchPrefix::default(),
            fetch: Fetch::default(),
            follow_up: FollowUp::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_follow_the_system_and_use_manual_permissions() {
        let s = Settings::default();
        assert_eq!(s.appearance, Appearance::System);
        assert_eq!(s.permission_mode, PermissionMode::Manual);
        assert_eq!(s.web, Decision::Ask);
        assert_eq!(s.branch_prefix.resolve(None), "kiln");
        assert_eq!(s.fetch, Fetch::Ask);
        assert_eq!(s.follow_up, FollowUp::Queue);
        assert_eq!(FollowUp::Queue.other(), FollowUp::Steer);
    }

    #[test]
    fn missing_fields_load_as_defaults() {
        let s: Settings =
            serde_json::from_value(serde_json::json!({"appearance": "dark"})).unwrap();
        assert_eq!(
            s,
            Settings {
                appearance: Appearance::Dark,
                ..Settings::default()
            }
        );
    }
}
