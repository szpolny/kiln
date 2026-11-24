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

#[derive(Debug, Clone)]
pub struct Thread {
    pub title: String,
    pub agent: String,
    pub status: Status,
    pub policy: Policy,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone)]
pub struct Project {
    pub name: String,
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
            messages: msgs
                .iter()
                .map(|(role, b)| Message { role: *role, body: (*b).into() })
                .collect(),
        };
        Self {
            projects: vec![
                Project {
                    name: "kiln".into(),
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
