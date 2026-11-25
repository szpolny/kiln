//! App-wide preferences, saved with the rest of the local state.

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub appearance: Appearance,
    /// Permission mode new threads start with when supported by their provider.
    pub permission_mode: PermissionMode,
    /// Web access new threads start with.
    pub web: Decision,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            appearance: Appearance::default(),
            permission_mode: PermissionMode::default(),
            web: Policy::default().network,
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
