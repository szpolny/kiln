//! Privacy policy. Default is deny: an agent gets nothing it was not granted.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Who decides whether the provider can run tools in a thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    #[default]
    Manual,
    Auto,
    FullAccess,
}

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

/// Project files that decide what runs: version control hooks and agent setup.
const CONTROL_FILES: [&str; 3] = [".git", ".claude", ".mcp.json"];

/// Where a file tool's target lies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    Inside,
    /// Inside the project, in a file that decides what runs.
    Control,
    /// Outside the project, or unknown.
    Outside,
}

impl Place {
    /// `resolved` is the target relative to the project once links are
    /// followed, `None` if it leaves the project. `named` is the path as asked
    /// for, so a linked control folder still counts.
    pub fn of(resolved: Option<&Path>, named: &Path) -> Self {
        let Some(resolved) = resolved else { return Place::Outside };
        let control = [resolved, named].iter().any(|path| {
            path.components().any(|part| {
                // Some file systems ignore case.
                CONTROL_FILES.iter().any(|name| part.as_os_str().eq_ignore_ascii_case(name))
            })
        });
        if control {
            Place::Control
        } else {
            Place::Inside
        }
    }
}

/// Which capability a provider tool needs. `place` only matters for file
/// tools. Unknown tools are treated as `Exec`, the strictest default that
/// still lets a human decide.
pub fn capability_for_tool(tool: &str, place: Place) -> Capability {
    match (tool, place) {
        ("Read" | "Glob" | "Grep" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit", Place::Outside) => {
            Capability::OutsideFiles
        }
        ("Read" | "Glob" | "Grep", _) => Capability::ProjectFiles,
        ("Write" | "Edit" | "MultiEdit" | "NotebookEdit", Place::Inside) => {
            Capability::ProjectFiles
        }
        (tool, _) if WEB_TOOLS.contains(&tool) => Capability::Network,
        _ => Capability::Exec,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_map_to_capabilities() {
        let inside = |tool| capability_for_tool(tool, Place::Inside);
        assert_eq!(inside("Bash"), Capability::Exec);
        assert_eq!(inside("Edit"), Capability::ProjectFiles);
        assert_eq!(inside("WebFetch"), Capability::Network);
        assert_eq!(inside("SomethingNew"), Capability::Exec);
    }

    #[test]
    fn file_tools_depend_on_where_they_reach() {
        for tool in ["Read", "Glob", "Grep", "Write", "Edit", "MultiEdit", "NotebookEdit"] {
            assert_eq!(capability_for_tool(tool, Place::Outside), Capability::OutsideFiles);
        }
        assert_eq!(capability_for_tool("Read", Place::Control), Capability::ProjectFiles);
        assert_eq!(capability_for_tool("Write", Place::Control), Capability::Exec);
        assert_eq!(capability_for_tool("Bash", Place::Outside), Capability::Exec);
    }

    #[test]
    fn control_files_are_recognised_by_either_path() {
        let place = |resolved: &str, named: &str| {
            Place::of(Some(Path::new(resolved)), Path::new(named))
        };
        assert_eq!(place("src/a.rs", "src/a.rs"), Place::Inside);
        assert_eq!(place(".gitignore", ".gitignore"), Place::Inside);
        assert_eq!(place(".git/hooks/pre-commit", "x"), Place::Control);
        assert_eq!(place("config/settings.json", ".claude/settings.json"), Place::Control);
        assert_eq!(place("sub/.MCP.json", "sub/.MCP.json"), Place::Control);
        assert_eq!(Place::of(None, Path::new("a")), Place::Outside);
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
