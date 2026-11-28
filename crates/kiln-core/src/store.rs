mod agent;
mod message;
mod thread;

pub use agent::{Agent, Effort, Model};
pub use message::{Attachment, AttachmentContent, Message, Prompt, Role};
pub use thread::{Status, Thread, CONTINUE_PROMPT, NEW_THREAD_TITLE};

use crate::checkout::Checkout;
use crate::settings::Settings;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    projectless_threads: Vec<Thread>,
    #[serde(default)]
    next_id: u64,
    #[serde(default)]
    pub settings: Settings,
    /// Whether the sidebar hides settled threads.
    #[serde(default)]
    pub settled_collapsed: bool,
    /// The last choices made in the composer, independent of thread history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_agent: Option<Agent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_effort: Option<Effort>,
}

impl Store {
    /// Give every thread a unique id (older state files have none).
    pub fn normalize(&mut self) {
        let mut max = self.next_id;
        let mut seen = std::collections::HashSet::new();
        for t in self.threads_mut() {
            if t.id != 0 && seen.insert(t.id) {
                max = max.max(t.id);
            } else {
                t.id = 0;
            }
        }
        for t in self.threads_mut() {
            if t.id == 0 {
                max += 1;
                t.id = max;
            }
            if let Some(m) = t.agent.models().iter().find(|m| t.model.as_deref() == Some(m.alias)) {
                t.model = Some(m.id.to_string());
            }
        }
        if self.last_model.as_deref() == Some("default") {
            self.last_model = Some(Agent::Codex.default_model().to_string());
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

    /// Add a thread to project `p`, created at `now`; returns its id. It starts
    /// with the last model and reasoning choices the agent supports, and the
    /// configured permission defaults.
    pub fn add_thread(&mut self, p: usize, agent: Agent, now: u64) -> Option<u64> {
        self.projects.get(p)?;
        let thread = self.new_thread(agent, now);
        let id = thread.id;
        self.projects[p].threads.push(thread);
        Some(id)
    }

    pub fn add_projectless_thread(&mut self, agent: Agent, now: u64) -> u64 {
        let thread = self.new_thread(agent, now);
        let id = thread.id;
        self.projectless_threads.push(thread);
        id
    }

    fn new_thread(&mut self, agent: Agent, now: u64) -> Thread {
        self.next_id += 1;
        let id = self.next_id;
        let mut thread = Thread::new(id, agent, now);
        thread.model = self
            .last_model
            .as_ref()
            .filter(|id| agent.models().iter().any(|model| model.id == id.as_str()))
            .cloned();
        thread.effort = self.last_effort;
        if agent.permission_modes().contains(&self.settings.permission_mode) {
            thread.permission_mode = self.settings.permission_mode;
        }
        thread.policy.network = self.settings.web;
        thread
    }

    /// Attach a projectless thread to its new checkout, keeping its history.
    pub fn attach_to_project(&mut self, id: u64, dir: &Path) -> bool {
        let Some((None, index)) = self.locate_thread(id) else { return false };
        let mut thread = self.projectless_threads.remove(index);
        thread.move_to(Checkout::Project);
        let project = self.add_project(dir);
        self.projects[project].threads.push(thread);
        true
    }

    /// Reset turns whose child processes did not survive an app restart.
    pub fn recover_interrupted_turns(&mut self) {
        for thread in self.threads_mut() {
            if thread.status == Status::Running {
                thread.status = Status::Idle;
            }
        }
    }

    fn threads_mut(&mut self) -> impl Iterator<Item = &mut Thread> {
        self.projects.iter_mut().flat_map(|p| &mut p.threads).chain(&mut self.projectless_threads)
    }

    /// Provider for a new thread: the last one chosen in the composer.
    pub fn last_agent(&self) -> Agent {
        self.last_agent.unwrap_or_default()
    }

    /// Delete an idle thread. Running threads are refused.
    pub fn delete_thread(&mut self, id: u64) -> bool {
        let Some((p, t)) = self.locate_thread(id) else { return false };
        let threads = match p {
            Some(p) => &mut self.projects[p].threads,
            None => &mut self.projectless_threads,
        };
        if threads[t].status == Status::Running {
            return false;
        }
        threads.remove(t);
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

    fn locate_thread(&self, id: u64) -> Option<(Option<usize>, usize)> {
        self.locate(id).map(|(p, t)| (Some(p), t)).or_else(|| {
            self.projectless_threads.iter().position(|t| t.id == id).map(|t| (None, t))
        })
    }

    pub fn project_of(&self, id: u64) -> Option<&Project> {
        let (p, _) = self.locate(id)?;
        self.projects.get(p)
    }

    pub fn thread(&self, id: u64) -> Option<&Thread> {
        let (p, t) = self.locate_thread(id)?;
        Some(match p {
            Some(p) => &self.projects[p].threads[t],
            None => &self.projectless_threads[t],
        })
    }

    pub fn thread_mut(&mut self, id: u64) -> Option<&mut Thread> {
        let (p, t) = self.locate_thread(id)?;
        Some(match p {
            Some(p) => &mut self.projects[p].threads[t],
            None => &mut self.projectless_threads[t],
        })
    }

    /// Apply an explicit composer choice to this thread and remember it for new ones.
    pub fn select_model(&mut self, id: u64, model: &str) -> bool {
        let Some(thread) = self.thread_mut(id) else { return false };
        thread.model = Some(model.to_owned());
        self.last_model = Some(model.to_owned());
        true
    }

    /// Select the next model in the same thread. Provider sessions cannot cross
    /// providers, so a switch starts a fresh session with a conversation handoff.
    pub fn choose_model(&mut self, id: u64, agent: Agent, model: &str, now: u64) -> Option<u64> {
        if !agent.models().iter().any(|choice| choice.id == model) {
            return None;
        }
        let source = self.thread_mut(id)?;
        let changed = source.agent != agent || source.model() != model;
        let notice = (changed && !source.messages.is_empty()).then(|| format!(
            "Switched from {} · {} to {} · {}",
            source.agent.name(), source.agent.model_name(source.model()),
            agent.name(), agent.model_name(model),
        ));
        if source.agent != agent {
            if source.status == Status::Running {
                return None;
            }
            source.agent = agent;
            source.session_id = None;
            source.reported_model = None;
            source.effort = None;
            source.handoff = !source.messages.is_empty();
        }
        if let Some(notice) = notice {
            source.messages.push(Message::notice(notice).at(now));
        }
        self.last_agent = Some(agent);
        self.select_model(id, model).then_some(id)
    }

    /// Apply an explicit composer choice to this thread and remember it for new ones.
    pub fn select_effort(&mut self, id: u64, effort: Effort) -> bool {
        let Some(thread) = self.thread_mut(id) else { return false };
        if !thread.agent.efforts(thread.model()).contains(&effort) {
            return false;
        }
        thread.effort = Some(effort);
        self.last_effort = Some(effort);
        true
    }

    pub fn project_dir_of(&self, id: u64) -> Option<PathBuf> {
        Some(self.project_of(id)?.dir.clone())
    }

    /// The folder the thread's turns run in.
    pub fn checkout_dir_of(&self, id: u64) -> Option<PathBuf> {
        let (p, t) = self.locate(id)?;
        let project = &self.projects[p];
        Some(project.threads[t].checkout.dir(&project.dir).to_path_buf())
    }

    /// Every folder a thread runs in: the projects and their worktrees.
    pub fn checkout_dirs(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = self.projects.iter().map(|p| p.dir.clone()).collect();
        for p in &self.projects {
            for t in &p.threads {
                if let Checkout::Worktree { path, .. } = &t.checkout {
                    dirs.push(path.clone());
                }
            }
        }
        dirs
    }

    /// Titles of the threads other than `id` that are running in its folder.
    pub fn running_beside(&self, id: u64) -> Vec<String> {
        let Some(dir) = self.checkout_dir_of(id) else { return vec![] };
        self.projects
            .iter()
            .flat_map(|p| p.threads.iter().map(move |t| (p, t)))
            .filter(|(p, t)| {
                t.id != id && t.status == Status::Running && t.checkout.dir(&p.dir) == dir
            })
            .map(|(_, t)| t.title.clone())
            .collect()
    }

    fn threads(&self) -> impl Iterator<Item = &Thread> {
        self.projects.iter().flat_map(|p| &p.threads).chain(&self.projectless_threads)
    }

    pub fn has_settled_threads(&self) -> bool {
        self.threads().any(|t| t.settled)
    }

    /// The active or the settled threads, latest activity
    /// first and newer threads first among equals.
    pub fn thread_ids(&self, settled: bool) -> Vec<u64> {
        let mut threads: Vec<&Thread> = self
            .threads()
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
    use std::time::Duration;

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
    fn a_stopped_turn_can_resume_with_or_without_a_session() {
        let mut t = Thread::new(1, Agent::Claude, 0);
        t.status = Status::Running;
        t.session_id = Some("sess-1".into());
        t.messages.push(Message::user("go"));
        t.stop_turn();
        assert_eq!(t.status, Status::Stopped);
        assert_eq!(t.session_id.as_deref(), Some("sess-1"));
        assert_eq!(t.messages.len(), 2);
        assert_eq!(t.messages[1].role, Role::Notice);
        assert_eq!(
            t.resume_prompt().map(|p| p.body).as_deref(),
            Some(CONTINUE_PROMPT),
        );
        t.messages.push(Message::user(CONTINUE_PROMPT));
        assert_eq!(t.resume_prompt().map(|p| p.body).as_deref(), Some(CONTINUE_PROMPT));
        t.session_id = None;
        assert_eq!(t.resume_prompt().map(|p| p.body).as_deref(), Some("go"));
        assert!(t.settle());
        assert_eq!(t.resume_prompt(), None);
    }

    #[test]
    fn queued_follow_ups_go_out_only_after_a_good_turn() {
        let mut t = Thread::new(1, Agent::Claude, 0);
        assert!(!t.enqueue(Prompt { body: "  ".trim().into(), attachments: vec![] }));
        assert!(t.enqueue(Prompt { body: " first ".trim().into(), attachments: vec![] }));
        assert!(t.enqueue(Prompt { body: "second".trim().into(), attachments: vec![] }));
        t.status = Status::Running;
        assert_eq!(t.next_queued(), None);
        for status in [Status::NeedsInput, Status::Stopped] {
            t.status = status;
            assert_eq!(t.next_queued(), None);
        }
        assert_eq!(t.queue.iter().map(|p| p.body.as_str()).collect::<Vec<_>>(), ["first", "second"]);
        t.status = Status::Idle;
        assert_eq!(t.next_queued().map(|p| p.body).as_deref(), Some("first"));
        assert_eq!(t.queue[0].body, "second");
    }

    #[test]
    fn queue_and_unread_are_saved_only_when_set() {
        let mut t = Thread::new(1, Agent::Claude, 0);
        let plain = serde_json::to_value(&t).unwrap();
        assert!(plain.get("queue").is_none() && plain.get("unread").is_none());
        t.enqueue(Prompt { body: "later".trim().into(), attachments: vec![] });
        t.unread = true;
        let back: Thread = serde_json::from_value(serde_json::to_value(&t).unwrap()).unwrap();
        assert_eq!((back.queue[0].body.as_str(), back.unread), ("later", true));
        let mut legacy = plain;
        legacy["queue"] = serde_json::json!(["from an older Kiln"]);
        let back: Thread = serde_json::from_value(legacy).unwrap();
        assert_eq!(back.queue[0].body, "from an older Kiln");
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
        for &effort in Agent::Claude.efforts(Agent::Claude.default_model()) {
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
    fn old_codex_default_resolves_to_a_named_model() {
        let mut store = Store::sample();
        let id = store.add_thread(0, Agent::Codex, 0).unwrap();
        assert_eq!(store.thread(id).unwrap().model(), "gpt-6.2-sol");
        assert!(store.select_model(id, "default"));
        store.normalize();
        assert_eq!(store.thread(id).unwrap().model(), "gpt-6.2-sol");
        assert_eq!(store.last_model.as_deref(), Some("gpt-6.2-sol"));
        assert!(Agent::Codex.models().iter().all(|model| model.id != "default"));
    }

    #[test]
    fn codex_uses_supported_effort_after_changing_models() {
        let mut store = Store::sample();
        let id = store.add_thread(0, Agent::Codex, 0).unwrap();
        assert!(store.select_effort(id, Effort::Max));
        assert_eq!(store.choose_model(id, Agent::Codex, "gpt-5.5", 1), Some(id));
        assert_eq!(store.thread(id).unwrap().effort(), Effort::Medium);
        assert!(!store.select_effort(id, Effort::Max));
        assert_eq!(store.choose_model(id, Agent::Codex, "gpt-6-luna", 2), Some(id));
        assert_eq!(store.thread(id).unwrap().effort(), Effort::Max);
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
    fn new_threads_start_with_the_last_composer_choices() {
        let mut s = Store::sample();
        let first = s.projects[0].threads[0].id;
        let second = s.projects[0].threads[1].id;
        assert!(s.select_model(first, "claude-sonnet-5-5"));
        assert!(s.select_model(second, "claude-haiku-5-5"));
        assert!(s.select_effort(first, Effort::High));
        s.settings.web = crate::policy::Decision::Deny;
        let id = s.add_thread(0, Agent::Claude, 0).unwrap();
        let t = s.thread(id).unwrap();
        assert_eq!((t.model(), t.effort()), ("claude-haiku-5-5", Effort::High));
        assert_eq!(t.policy.network, crate::policy::Decision::Deny);
        assert_eq!(s.thread(first).unwrap().model(), "claude-sonnet-5-5");
        assert_eq!(s.thread(second).unwrap().effort(), Agent::Claude.default_effort());

        assert!(s.select_model(second, Agent::Claude.default_model()));
        assert!(s.select_effort(second, Agent::Claude.default_effort()));
        let id = s.add_thread(0, Agent::Claude, 0).unwrap();
        let t = s.thread(id).unwrap();
        assert_eq!(t.model(), Agent::Claude.default_model());
        assert_eq!(t.effort(), Agent::Claude.default_effort());
    }

    #[test]
    fn new_threads_fall_back_when_the_last_model_is_unavailable() {
        let mut s = Store::sample();
        s.last_model = Some("claude-retired-1".into());
        let id = s.add_thread(0, Agent::Claude, 0).unwrap();
        assert_eq!(s.thread(id).unwrap().model(), Agent::Claude.default_model());
    }

    #[test]
    fn choosing_a_codex_model_changes_the_provider_of_an_empty_thread() {
        let mut store = Store::sample();
        let id = store.projects[0].threads[0].id;
        assert_eq!(store.choose_model(id, Agent::Codex, "gpt-6.2-sol", 1), Some(id));
        let thread = store.thread(id).unwrap();
        assert_eq!(thread.agent, Agent::Codex);
        assert_eq!(thread.model(), "gpt-6.2-sol");
        assert_eq!(store.projects[0].threads.len(), 2);
        assert!(store.choose_model(id, Agent::Claude, "gpt-6.2-sol", 2).is_none());
        assert_eq!(store.thread(id).unwrap().agent, Agent::Codex);
    }

    #[test]
    fn new_threads_use_the_last_chosen_provider() {
        let mut store = Store::sample();
        assert_eq!(store.last_agent(), Agent::Claude);
        let id = store.projects[0].threads[0].id;
        assert!(store.choose_model(id, Agent::Codex, "gpt-6.2-astra", 1).is_some());
        assert_eq!(store.last_agent(), Agent::Codex);
        let id = store.add_thread(0, store.last_agent(), 2).unwrap();
        assert_eq!(store.thread(id).unwrap().model(), "gpt-6.2-astra");
    }

    #[test]
    fn a_thread_with_a_draft_or_history_is_not_untouched() {
        let mut t = Thread::new(1, Agent::Claude, 0);
        assert!(t.is_untouched());
        t.draft = "  ".into();
        assert!(t.is_untouched());
        t.draft = "fix the build".into();
        assert!(!t.is_untouched());
        t.draft.clear();
        t.messages.push(Message::user("hi"));
        assert!(!t.is_untouched());
    }

    #[test]
    fn drafts_are_saved_only_when_present() {
        let mut t = Thread::new(1, Agent::Claude, 0);
        assert!(serde_json::to_value(&t).unwrap().get("draft").is_none());
        t.draft = "half a thought".into();
        let back: Thread = serde_json::from_value(serde_json::to_value(&t).unwrap()).unwrap();
        assert_eq!(back.draft, "half a thought");
    }

    #[test]
    fn choosing_a_different_provider_preserves_an_existing_conversation() {
        let mut store = Store::sample();
        let id = store.projects[0].threads[0].id;
        let thread = store.thread_mut(id).unwrap();
        thread.messages.push(Message::user("Existing prompt"));
        thread.session_id = Some("existing-session".into());
        let target = store.choose_model(id, Agent::Codex, "gpt-6.2-astra", 1).unwrap();
        assert_eq!(target, id);
        let thread = store.thread(id).unwrap();
        assert_eq!(thread.agent, Agent::Codex);
        assert_eq!(thread.model(), "gpt-6.2-astra");
        assert_eq!(thread.messages[0].body, "Existing prompt");
        assert!(thread.session_id.is_none() && thread.handoff);
        let saved = serde_json::to_string(&store).unwrap();
        let mut store: Store = serde_json::from_str(&saved).unwrap();
        assert!(store.thread(id).unwrap().handoff);
        store.thread_mut(id).unwrap().status = Status::Running;
        assert_eq!(store.choose_model(id, Agent::Claude, Agent::Claude.default_model(), 2), None);
        store.thread_mut(id).unwrap().status = Status::Idle;
        assert_eq!(store.choose_model(id, Agent::Claude, Agent::Claude.default_model(), 3), Some(id));
        assert!(store.thread(id).unwrap().handoff);

    }

    #[test]
    fn model_switch_notices_record_only_changes_in_existing_conversations() {
        let mut store = Store::default();
        let id = store.add_projectless_thread(Agent::Claude, 0);
        store.choose_model(id, Agent::Codex, "gpt-6.2-astra", 1).unwrap();
        assert!(store.thread(id).unwrap().messages.is_empty());
        store.thread_mut(id).unwrap().messages.push(Message::user("Hello"));
        store.choose_model(id, Agent::Codex, "gpt-6.2-astra", 2).unwrap();
        assert_eq!(store.thread(id).unwrap().messages.len(), 1);
        store.thread_mut(id).unwrap().status = Status::Running;
        assert!(store.choose_model(id, Agent::Claude, Agent::Claude.default_model(), 3).is_none());
        assert_eq!(store.thread(id).unwrap().messages.len(), 1);
        store.thread_mut(id).unwrap().status = Status::Idle;
        store.choose_model(id, Agent::Codex, "gpt-6.2-sol", 4).unwrap();
        let notice = &store.thread(id).unwrap().messages[1];
        assert_eq!(notice.role, Role::Notice);
        assert_eq!(notice.at, 4);
        assert_eq!(notice.body, format!("Switched from Codex · {} to Codex · {}",
            Agent::Codex.model_name("gpt-6.2-astra"), Agent::Codex.model_name("gpt-6.2-sol")));
        store.choose_model(id, Agent::Claude, Agent::Claude.default_model(), 5).unwrap();
        let restored: Store = serde_json::from_str(&serde_json::to_string(&store).unwrap()).unwrap();
        let notice = &restored.thread(id).unwrap().messages[2];
        assert!(notice.body.starts_with("Switched from Codex · "));
        assert!(notice.body.contains(" to Claude Code · "));
        assert_eq!(notice.at, 5);
    }

    #[test]
    fn same_provider_model_switch_keeps_native_context() {
        let mut store = Store::sample();
        let id = store.add_projectless_thread(Agent::Codex, 0);
        let thread = store.thread_mut(id).unwrap();
        thread.session_id = Some("session".into());
        thread.messages.push(Message::user("Keep working"));
        thread.draft = "next step".into();
        thread.enqueue(Prompt { body: "queued".into(), attachments: vec![] });
        assert_eq!(store.choose_model(id, Agent::Codex, "gpt-6.2-astra", 1), Some(id));
        let thread = store.thread(id).unwrap();
        assert_eq!(thread.session_id.as_deref(), Some("session"));
        assert!(!thread.handoff);
        assert_eq!(store.choose_model(id, Agent::Claude, Agent::Claude.default_model(), 2), Some(id));
        let thread = store.thread(id).unwrap();
        assert_eq!(thread.draft, "next step");
        assert_eq!(thread.queue[0].body, "queued");
        assert_eq!(thread.messages[0].body, "Keep working");
        assert!(thread.handoff && thread.session_id.is_none());
    }

    #[test]
    fn stores_saved_without_settings_still_load() {
        let mut value = serde_json::to_value(Store::sample()).unwrap();
        value.as_object_mut().unwrap().remove("settings");
        let mut store: Store = serde_json::from_value(value).unwrap();
        assert_eq!(store.settings, Settings::default());
        let id = store.add_thread(0, Agent::Claude, 0).unwrap();
        let thread = store.thread(id).unwrap();
        assert_eq!((thread.model.as_deref(), thread.effort), (None, None));
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
        assert!(!t.settled);

        // A failed turn leaves nothing to approve, so settling dismisses it.
        t.status = Status::NeedsInput;
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
    fn threads_report_who_else_runs_in_their_checkout() {
        let mut s = Store::sample();
        let (a, b) = (s.projects[0].threads[0].id, s.projects[0].threads[1].id);
        let c = s.add_thread(0, Agent::Claude, 0).unwrap();
        s.thread_mut(b).unwrap().title = "Other".into();
        s.thread_mut(b).unwrap().status = Status::Running;
        assert_eq!(s.running_beside(a), ["Other"]);
        assert!(s.running_beside(b).is_empty());

        let worktree = Checkout::Worktree {
            path: "/tmp/kiln-wt".into(),
            branch: "kiln/x".into(),
            base: "main".into(),
        };
        s.thread_mut(a).unwrap().session_id = Some("old".into());
        s.thread_mut(a).unwrap().messages.push(Message::user("hi"));
        s.thread_mut(a).unwrap().move_to(worktree.clone());
        assert_eq!(s.thread(a).unwrap().session_id, None);
        assert!(s.thread(a).unwrap().handoff);
        s.thread_mut(c).unwrap().move_to(Checkout::Project);
        assert!(!s.thread(c).unwrap().handoff, "nothing to recap");
        assert_eq!(s.checkout_dir_of(a).as_deref(), Some(Path::new("/tmp/kiln-wt")));
        assert!(s.running_beside(a).is_empty());
        assert_eq!(s.running_beside(c), ["Other"]);
        assert!(s.checkout_dirs().contains(&PathBuf::from("/tmp/kiln-wt")));

        let back: Thread = serde_json::from_value(serde_json::to_value(s.thread(a).unwrap()).unwrap()).unwrap();
        assert_eq!(back.checkout, worktree);
        let plain = serde_json::to_value(s.thread(c).unwrap()).unwrap();
        assert!(plain.get("checkout").is_none());
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
