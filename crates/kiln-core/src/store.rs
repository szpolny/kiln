use crate::policy::Policy;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Running,
    NeedsInput,
    Idle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Agent,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub role: Role,
    pub body: String,
}

/// An action the agent was denied and a human must decide on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approval {
    pub tool: String,
    /// What the user sees, e.g. the shell command.
    pub summary: String,
    /// Narrow allow rule used if the user approves.
    pub rule: String,
}

#[derive(Debug, Clone)]
pub struct Thread {
    pub title: String,
    pub agent: String,
    pub status: Status,
    pub policy: Policy,
    pub messages: Vec<Message>,
    /// Provider session id, used to resume the conversation.
    pub session_id: Option<String>,
    /// Denied actions waiting for the user.
    pub pending: Vec<Approval>,
    /// Rules the policy already allows; sent with the automatic retry.
    pub auto_allow: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Project {
    pub name: String,
    pub dir: std::path::PathBuf,
    pub threads: Vec<Thread>,
}

#[derive(Debug, Clone, Default)]
pub struct Store {
    pub projects: Vec<Project>,
}

impl Store {
    /// Placeholder content until persistence exists.
    pub fn demo() -> Self {
        let t = |title: &str, agent: &str, status, msgs: &[(Role, &str)]| Thread {
            title: title.into(),
            agent: agent.into(),
            status,
            policy: Policy::default(),
            session_id: None,
            pending: vec![],
            auto_allow: vec![],
            messages: msgs
                .iter()
                .map(|(role, b)| Message { role: *role, body: (*b).into() })
                .collect(),
        };
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
        Self {
            projects: vec![
                Project {
                    name: "kiln".into(),
                    dir: home.join("source/kiln"),
                    threads: vec![
                        t("Scaffold GPUI shell", "claude", Status::Running, &[
                            (Role::User, "Set up the app shell."),
                            (Role::Agent, "Done. Sidebar, thread view and composer are in place."),
                        ]),
                        t("Sandbox policy draft", "codex", Status::NeedsInput, &[
                            (Role::Agent, "May I run `cargo test`?"),
                        ]),
                    ],
                },
                Project {
                    name: "nix".into(),
                    dir: home.join("nix"),
                    threads: vec![t("Tidy home-manager", "claude", Status::Idle, &[])],
                },
            ],
        }
    }

    pub fn thread_mut(&mut self, p: usize, t: usize) -> Option<&mut Thread> {
        self.projects.get_mut(p)?.threads.get_mut(t)
    }

    pub fn thread(&self, p: usize, t: usize) -> Option<&Thread> {
        self.projects.get(p)?.threads.get(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_threads_default_to_local_only() {
        let s = Store::demo();
        assert!(s.projects.iter().flat_map(|p| &p.threads).all(|t| t.policy.is_local_only()));
    }
}
