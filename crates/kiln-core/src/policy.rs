//! Privacy policy. Default is deny: an agent gets nothing it was not granted.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Capability {
    /// Read or write files inside the project directory.
    ProjectFiles,
    /// Read or write files outside the project directory.
    OutsideFiles,
    /// Any outbound network connection.
    Network,
    /// Run a shell command.
    Exec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    Allow,
    Ask,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    pub project_files: Decision,
    pub outside_files: Decision,
    pub network: Decision,
    pub exec: Decision,
}

impl Default for Policy {
    /// Local-only: project files are fine, everything else needs a human.
    fn default() -> Self {
        Self {
            project_files: Decision::Allow,
            outside_files: Decision::Deny,
            network: Decision::Deny,
            exec: Decision::Ask,
        }
    }
}

impl Policy {
    pub fn decide(&self, cap: Capability) -> Decision {
        match cap {
            Capability::ProjectFiles => self.project_files,
            Capability::OutsideFiles => self.outside_files,
            Capability::Network => self.network,
            Capability::Exec => self.exec,
        }
    }

    pub fn is_local_only(&self) -> bool {
        self.network == Decision::Deny
    }
}

/// Which capability a provider tool needs. Unknown tools are treated as
/// `Exec`, the strictest default that still lets a human decide.
pub fn capability_for_tool(tool: &str) -> Capability {
    match tool {
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" | "Glob" | "Grep" => {
            Capability::ProjectFiles
        }
        "WebFetch" | "WebSearch" => Capability::Network,
        _ => Capability::Exec,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_map_to_capabilities() {
        assert_eq!(capability_for_tool("Bash"), Capability::Exec);
        assert_eq!(capability_for_tool("Edit"), Capability::ProjectFiles);
        assert_eq!(capability_for_tool("WebFetch"), Capability::Network);
        assert_eq!(capability_for_tool("SomethingNew"), Capability::Exec);
    }

    #[test]
    fn default_policy_is_local_only() {
        let p = Policy::default();
        assert!(p.is_local_only());
        assert_eq!(p.decide(Capability::Network), Decision::Deny);
        assert_eq!(p.decide(Capability::OutsideFiles), Decision::Deny);
        assert_eq!(p.decide(Capability::Exec), Decision::Ask);
    }
}
