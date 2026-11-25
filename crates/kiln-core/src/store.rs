use crate::policy::Policy;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Running,
    NeedsInput,
    Idle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    User,
    Agent,
    /// Written by Kiln itself: errors and approval outcomes.
    Notice,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub body: String,
    /// Set when this is a tool call: the tool's name, with `body` its input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
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
        Self { role, body: body.into(), tool: None, at: 0, worked_ms: 0 }
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

/// An action the agent was denied and a human must decide on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    pub tool: String,
    /// What the user sees, e.g. the shell command.
    pub summary: String,
    /// Narrow allow rule used if the user approves.
    pub rule: String,
}

/// Which provider runs a thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Agent {
    #[default]
    Claude,
}

impl Agent {
    pub fn label(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
        }
    }

    /// Exact models this agent offers. Pinned rather than looked up, so the
    /// list never needs the network; a new release means updating it.
    pub fn models(self) -> &'static [Model] {
        match self {
            Agent::Claude => &[
                Model { id: "claude-fable-5-2", name: "Fable 5.2", alias: "fable" },
                Model { id: "claude-opus-5-5", name: "Opus 5.5", alias: "opus" },
                Model { id: "claude-sonnet-5-5", name: "Sonnet 5.5", alias: "sonnet" },
                Model { id: "claude-haiku-5-5", name: "Haiku 5.5", alias: "haiku" },
            ],
        }
    }

    /// Model used until the user picks one.
    pub fn default_model(self) -> &'static str {
        match self {
            Agent::Claude => "claude-opus-5-5",
        }
    }

    /// Reasoning levels this agent accepts, lowest first.
    pub fn efforts(self) -> &'static [Effort] {
        match self {
            Agent::Claude => {
                &[Effort::Low, Effort::Medium, Effort::High, Effort::XHigh, Effort::Max]
            }
        }
    }

    /// Reasoning level used until the user picks one.
    pub fn default_effort(self) -> Effort {
        match self {
            Agent::Claude => Effort::Medium,
        }
    }

    /// Display name for a model id, or the id itself when it is not listed.
    pub fn model_name(self, id: &str) -> &str {
        self.models().iter().find(|m| m.id == id).map_or(id, |m| m.name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Model {
    /// Exact id passed to the agent.
    pub id: &'static str,
    pub name: &'static str,
    /// Moving alias earlier versions stored; mapped to `id` on load.
    pub alias: &'static str,
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
    pub policy: Policy,
    pub messages: Vec<Message>,
    /// Provider session id, used to resume the conversation.
    pub session_id: Option<String>,
    /// Denied actions waiting for the user.
    pub pending: Vec<Approval>,
    /// Rules the policy already allows; sent with the automatic retry.
    pub auto_allow: Vec<String>,
    /// Last activity in Unix seconds; 0 when unknown.
    #[serde(default)]
    pub updated_at: u64,
    /// The user set the thread aside as done with.
    #[serde(default)]
    pub settled: bool,
}

impl Thread {
    fn new(id: u64, agent: Agent, now: u64) -> Self {
        Self {
            id,
            title: NEW_THREAD_TITLE.into(),
            agent,
            model: None,
            effort: None,
            reported_model: None,
            status: Status::Idle,
            policy: Policy::default(),
            messages: vec![],
            session_id: None,
            pending: vec![],
            auto_allow: vec![],
            updated_at: now,
            settled: false,
        }
    }

    /// Model the next turn runs with.
    pub fn model(&self) -> &str {
        self.model.as_deref().unwrap_or(self.agent.default_model())
    }

    /// Reasoning level the next turn runs with.
    pub fn effort(&self) -> Effort {
        self.effort.unwrap_or(self.agent.default_effort())
    }

    /// Record activity at `now`, which makes a settled thread active again.
    pub fn touch(&mut self, now: u64) {
        self.updated_at = now;
        self.settled = false;
    }

    /// Whether nothing is running or waiting for the user's approval.
    pub fn can_settle(&self) -> bool {
        self.status != Status::Running && self.pending.is_empty()
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

    /// Count time the agent ran toward the latest user message. A turn that
    /// resumes after an approval adds to it.
    pub fn record_work(&mut self, ran: Duration) {
        if let Some(m) = self.messages.iter_mut().rev().find(|m| m.role == Role::User) {
            let ms = u64::try_from(ran.as_millis()).unwrap_or(u64::MAX);
            m.worked_ms = m.worked_ms.saturating_add(ms);
        }
    }

    /// End a turn the user stopped. Nothing it asked for stays waiting.
    pub fn stop_turn(&mut self) {
        self.status = Status::Idle;
        self.pending.clear();
        self.auto_allow.clear();
        self.messages.push(Message::notice("Stopped"));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub dir: PathBuf,
    /// Image the user picked as the icon, in place of the one found in `dir`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<PathBuf>,
    pub threads: Vec<Thread>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Store {
    pub projects: Vec<Project>,
    #[serde(default)]
    next_id: u64,
}

impl Store {
    /// Give every thread a unique id (older state files have none).
    pub fn normalize(&mut self) {
        let mut max = self.next_id;
        let mut seen = std::collections::HashSet::new();
        for t in self.projects.iter_mut().flat_map(|p| &mut p.threads) {
            if t.id != 0 && seen.insert(t.id) {
                max = max.max(t.id);
            } else {
                t.id = 0;
            }
        }
        for t in self.projects.iter_mut().flat_map(|p| &mut p.threads) {
            if t.id == 0 {
                max += 1;
                t.id = max;
            }
            if let Some(m) = t.agent.models().iter().find(|m| t.model.as_deref() == Some(m.alias)) {
                t.model = Some(m.id.to_string());
            }
        }
        self.next_id = max;
    }

    /// Add a project for `dir`, or return the existing one.
    pub fn add_project(&mut self, dir: &Path) -> usize {
        if let Some(i) = self.projects.iter().position(|p| p.dir == dir) {
            return i;
        }
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.display().to_string());
        let dir = dir.to_path_buf();
        self.projects.push(Project { name, dir, icon: None, threads: vec![] });
        self.projects.len() - 1
    }

    /// Add a thread to project `p`, created at `now`; returns its id.
    pub fn add_thread(&mut self, p: usize, agent: Agent, now: u64) -> Option<u64> {
        self.next_id += 1;
        let id = self.next_id;
        self.projects.get_mut(p)?.threads.push(Thread::new(id, agent, now));
        Some(id)
    }

    /// Delete an idle thread. Running threads are refused.
    pub fn delete_thread(&mut self, id: u64) -> bool {
        let Some((p, t)) = self.locate(id) else { return false };
        if self.projects[p].threads[t].status == Status::Running {
            return false;
        }
        self.projects[p].threads.remove(t);
        true
    }

    pub fn remove_project(&mut self, p: usize) -> bool {
        let busy = self
            .projects
            .get(p)
            .is_none_or(|pr| pr.threads.iter().any(|t| t.status == Status::Running));
        if busy {
            return false;
        }
        self.projects.remove(p);
        true
    }

    pub fn locate(&self, id: u64) -> Option<(usize, usize)> {
        self.projects.iter().enumerate().find_map(|(p, pr)| {
            pr.threads.iter().position(|t| t.id == id).map(|t| (p, t))
        })
    }

    pub fn thread(&self, id: u64) -> Option<&Thread> {
        let (p, t) = self.locate(id)?;
        Some(&self.projects[p].threads[t])
    }

    pub fn thread_mut(&mut self, id: u64) -> Option<&mut Thread> {
        let (p, t) = self.locate(id)?;
        Some(&mut self.projects[p].threads[t])
    }

    pub fn project_dir_of(&self, id: u64) -> Option<PathBuf> {
        let (p, _) = self.locate(id)?;
        Some(self.projects[p].dir.clone())
    }

    /// The active or the settled threads of every project, latest activity
    /// first and newer threads first among equals.
    pub fn thread_ids(&self, settled: bool) -> Vec<u64> {
        let mut threads: Vec<&Thread> = self
            .projects
            .iter()
            .flat_map(|p| &p.threads)
            .filter(|t| t.settled == settled)
            .collect();
        threads.sort_unstable_by_key(|t| std::cmp::Reverse((t.updated_at, t.id)));
        threads.into_iter().map(|t| t.id).collect()
    }

    /// The thread at the top of the list: active ones come before settled ones.
    pub fn first_thread_id(&self) -> Option<u64> {
        self.thread_ids(false).first().or(self.thread_ids(true).first()).copied()
    }

    /// Test fixture: one project with two threads.
    #[cfg(test)]
    pub fn sample() -> Self {
        let mut s = Self::default();
        let p = s.add_project(Path::new("/tmp/kiln-sample"));
        s.add_thread(p, Agent::Claude, 0);
        s.add_thread(p, Agent::Claude, 0);
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_without_a_choice_uses_the_default_model() {
        let mut t = Thread::new(1, Agent::Claude, 0);
        assert_eq!(t.model(), "claude-opus-5-5");
        t.model = Some("claude-haiku-5-5".into());
        assert_eq!(t.model(), "claude-haiku-5-5");
        assert_eq!(t.agent.model_name(t.model()), "Haiku 5.5");
        assert_eq!(t.agent.model_name("claude-mystery-9"), "claude-mystery-9");
    }

    #[test]
    fn a_stopped_turn_leaves_the_thread_idle_and_keeps_its_session() {
        let mut t = Thread::new(1, Agent::Claude, 0);
        t.status = Status::Running;
        t.session_id = Some("sess-1".into());
        t.messages.push(Message::user("go"));
        t.auto_allow.push("WebSearch".into());
        t.pending.push(Approval {
            tool: "Bash".into(),
            summary: "$ touch a".into(),
            rule: "Bash(touch a)".into(),
        });
        t.stop_turn();
        assert_eq!(t.status, Status::Idle);
        assert!(t.pending.is_empty() && t.auto_allow.is_empty());
        assert_eq!(t.session_id.as_deref(), Some("sess-1"));
        assert_eq!(t.messages.len(), 2);
        assert_eq!(t.messages[1].role, Role::Notice);
    }

    #[test]
    fn work_adds_up_on_the_latest_user_message() {
        let mut t = Thread::new(1, Agent::Claude, 0);
        t.messages.push(Message::user("first"));
        t.messages.push(Message::user("second"));
        t.messages.push(Message::agent("ok"));
        t.record_work(Duration::from_millis(1500));
        t.record_work(Duration::from_millis(700));
        assert_eq!((t.messages[0].worked_ms, t.messages[1].worked_ms), (0, 2200));
    }

    #[test]
    fn messages_saved_without_times_still_load() {
        let saved = serde_json::json!({"role": "User", "body": "hi"});
        let m: Message = serde_json::from_value(saved.clone()).unwrap();
        assert_eq!((m.at, m.worked_ms), (0, 0));
        assert_eq!(serde_json::to_value(m).unwrap(), saved);
    }

    #[test]
    fn effort_is_stored_as_the_value_the_agent_takes() {
        for &effort in Agent::Claude.efforts() {
            let stored = serde_json::to_value(effort).unwrap();
            assert_eq!(stored, effort.id());
        }
    }

    #[test]
    fn threads_saved_without_an_effort_still_load() {
        let mut value = serde_json::to_value(Thread::new(1, Agent::Claude, 0)).unwrap();
        value.as_object_mut().unwrap().remove("effort");
        let thread: Thread = serde_json::from_value(value).unwrap();
        assert_eq!(thread.effort(), Effort::Medium);
    }

    #[test]
    fn normalize_pins_stored_model_aliases() {
        let mut s = Store::sample();
        s.projects[0].threads[0].model = Some("sonnet".into());
        s.projects[0].threads[1].model = Some("claude-custom-1".into());
        s.normalize();
        assert_eq!(s.projects[0].threads[0].model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(s.projects[0].threads[1].model.as_deref(), Some("claude-custom-1"));
    }

    #[test]
    fn new_threads_get_the_default_policy_and_unique_ids() {
        let s = Store::sample();
        let ids: Vec<_> = s.projects[0].threads.iter().map(|t| t.id).collect();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
        let asks = |t: &Thread| t.policy.network == crate::policy::Decision::Ask;
        assert!(s.projects[0].threads.iter().all(asks));
    }

    #[test]
    fn adding_the_same_folder_twice_reuses_the_project() {
        let mut s = Store::default();
        let a = s.add_project(Path::new("/x/app"));
        assert_eq!(s.add_project(Path::new("/x/app")), a);
        assert_eq!(s.projects[a].name, "app");
    }

    #[test]
    fn ids_survive_deleting_an_earlier_thread() {
        let mut s = Store::sample();
        let (first, second) = (s.projects[0].threads[0].id, s.projects[0].threads[1].id);
        assert!(s.delete_thread(first));
        assert_eq!(s.locate(second), Some((0, 0)));
        let third = s.add_thread(0, Agent::Claude, 0).unwrap();
        assert!(third != first && third != second);
    }

    #[test]
    fn running_threads_cannot_be_deleted() {
        let mut s = Store::sample();
        let id = s.projects[0].threads[0].id;
        s.thread_mut(id).unwrap().status = Status::Running;
        assert!(!s.delete_thread(id));
        assert!(!s.remove_project(0));
    }

    #[test]
    fn threads_of_all_projects_are_listed_by_latest_activity() {
        let mut s = Store::default();
        let a = s.add_project(Path::new("/x/a"));
        let b = s.add_project(Path::new("/x/b"));
        let first = s.add_thread(a, Agent::Claude, 10).unwrap();
        let second = s.add_thread(b, Agent::Claude, 20).unwrap();
        let third = s.add_thread(a, Agent::Claude, 20).unwrap();
        assert_eq!(s.thread_ids(false), [third, second, first]);

        s.thread_mut(first).unwrap().touch(30);
        assert_eq!(s.thread_ids(false), [first, third, second]);

        assert!(s.thread_mut(first).unwrap().settle());
        assert_eq!(s.thread_ids(false), [third, second]);
        assert_eq!(s.thread_ids(true), [first]);
        assert_eq!(s.first_thread_id(), Some(third));

        for id in [second, third] {
            assert!(s.thread_mut(id).unwrap().settle());
        }
        assert_eq!(s.first_thread_id(), Some(first));
    }

    #[test]
    fn new_activity_brings_a_settled_thread_back() {
        let mut t = Thread::new(1, Agent::Claude, 10);
        assert!(t.settle());
        t.touch(20);
        assert!(!t.settled);
        assert_eq!(t.updated_at, 20);
    }

    #[test]
    fn only_threads_that_need_nothing_can_be_settled() {
        let mut t = Thread::new(1, Agent::Claude, 0);
        t.status = Status::Running;
        assert!(!t.settle());
        t.status = Status::NeedsInput;
        t.pending.push(Approval {
            tool: "Bash".into(),
            summary: "$ touch a".into(),
            rule: "Bash(touch a)".into(),
        });
        assert!(!t.settle() && !t.settled);

        // A failed turn leaves nothing to approve, so settling dismisses it.
        t.pending.clear();
        assert!(t.settle());
        assert_eq!(t.status, Status::Idle);
    }

    #[test]
    fn threads_saved_without_activity_or_settled_state_still_load() {
        let mut value = serde_json::to_value(Thread::new(1, Agent::Claude, 10)).unwrap();
        let fields = value.as_object_mut().unwrap();
        fields.remove("updated_at");
        fields.remove("settled");
        let thread: Thread = serde_json::from_value(value).unwrap();
        assert_eq!((thread.updated_at, thread.settled), (0, false));
    }

    #[test]
    fn normalize_assigns_ids_to_legacy_threads() {
        let mut s = Store::sample();
        for t in &mut s.projects[0].threads {
            t.id = 0;
        }
        s.normalize();
        let a = s.projects[0].threads[0].id;
        let b = s.projects[0].threads[1].id;
        assert!(a != 0 && b != 0 && a != b);
        let c = s.add_thread(0, Agent::Claude, 0).unwrap();
        assert!(c != a && c != b);
    }
}
