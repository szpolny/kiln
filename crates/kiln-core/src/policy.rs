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
    /// Project files are fine; web tools and commands need a human.
    fn default() -> Self {
        Self {
            project_files: Decision::Allow,
            outside_files: Decision::Deny,
            network: Decision::Ask,
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

    /// Web tools to allow and to disable for a whole turn. Both are empty
    /// when each use needs approval.
    pub fn web_tool_rules(&self) -> (&'static [&'static str], &'static [&'static str]) {
        match self.network {
            Decision::Allow => (&WEB_TOOLS, &[]),
            Decision::Ask => (&[], &[]),
            Decision::Deny => (&[], &WEB_TOOLS),
        }
    }
}

/// Provider tools that reach the web.
pub const WEB_TOOLS: [&str; 2] = ["WebFetch", "WebSearch"];

/// Which capability a provider tool needs. Unknown tools are treated as
/// `Exec`, the strictest default that still lets a human decide.
pub fn capability_for_tool(tool: &str) -> Capability {
    match tool {
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" | "Glob" | "Grep" => {
            Capability::ProjectFiles
        }
        tool if WEB_TOOLS.contains(&tool) => Capability::Network,
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
    fn default_policy_asks_before_using_the_web() {
        let p = Policy::default();
        assert_eq!(p.decide(Capability::Network), Decision::Ask);
        assert_eq!(p.decide(Capability::OutsideFiles), Decision::Deny);
        assert_eq!(p.decide(Capability::Exec), Decision::Ask);
    }

    #[test]
    fn web_choice_sets_the_turn_rules() {
        let rules = |network| Policy { network, ..Policy::default() }.web_tool_rules();
        let (web, none): (&[&str], &[&str]) = (&WEB_TOOLS, &[]);
        assert_eq!(rules(Decision::Allow), (web, none));
        assert_eq!(rules(Decision::Ask), (none, none));
        assert_eq!(rules(Decision::Deny), (none, web));
    }
}
