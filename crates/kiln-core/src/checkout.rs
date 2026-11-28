//! Where a thread's turns run: the project folder itself, or a Git worktree
//! Kiln made for it. Also what the agent is told about sharing a checkout, and
//! how it asks Kiln to move the thread.

use crate::store::{Message, Role};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Checkout {
    /// The project folder, shared with every other thread that uses it.
    #[default]
    Project,
    /// A worktree Kiln created for this thread alone.
    Worktree {
        path: PathBuf,
        branch: String,
        /// What the branch started from: the project's branch or commit then.
        base: String,
    },
}

impl Checkout {
    /// The folder turns run in.
    pub fn dir<'a>(&'a self, project: &'a Path) -> &'a Path {
        match self {
            Checkout::Project => project,
            Checkout::Worktree { path, .. } => path,
        }
    }

    /// Where the thread runs, as told to the agent.
    pub fn describe(&self, project: &Path) -> String {
        match self {
            Checkout::Project => format!("the project checkout at {}", project.display()),
            Checkout::Worktree { path, branch, .. } => {
                format!("its own Git worktree at {} on the branch {branch}", path.display())
            }
        }
    }
}

/// What starts the line an agent replies with to move its thread to a new
/// worktree. A JSON [`MoveRequest`] follows it.
pub const MOVE_REQUEST: &str = "KILN_MOVE_TO_WORKTREE";
pub const PROJECT_REQUEST: &str = "KILN_CREATE_PROJECT";

#[derive(Debug, Deserialize)]
pub struct ProjectRequest {
    pub name: String,
}

/// A project request must be the last line of a reply, with a JSON payload.
pub fn take_project_request(text: &str) -> (String, Option<ProjectRequest>) {
    let text = text.trim_end();
    let (body, line) = text.rsplit_once('\n').unwrap_or(("", text));
    let request = line.strip_prefix(PROJECT_REQUEST)
        .filter(|payload| payload.starts_with(char::is_whitespace))
        .and_then(|payload| serde_json::from_str(payload).ok());
    (if request.is_some() { body } else { text }.trim().to_owned(), request)
}

/// Conventional Commits types a branch can be named after.
pub const COMMIT_TYPES: [&str; 11] =
    ["feat", "fix", "docs", "style", "refactor", "perf", "test", "build", "ci", "chore", "revert"];

/// The type used when none, or none that is listed, was given.
pub const DEFAULT_COMMIT_TYPE: &str = "feat";

/// What the branches of new worktrees start with.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchPrefix {
    /// `kiln/`.
    #[default]
    Kiln,
    /// The Conventional Commits type of the work, such as `fix/`.
    CommitType,
    /// Text the user chose; empty for no prefix.
    Custom(String),
}

impl BranchPrefix {
    /// The prefix for work of `commit_type`, without the trailing slash.
    pub fn resolve(&self, commit_type: Option<&str>) -> String {
        match self {
            BranchPrefix::Kiln => "kiln".into(),
            BranchPrefix::CommitType => commit_type
                .map(|t| t.trim().to_lowercase())
                .filter(|t| COMMIT_TYPES.contains(&t.as_str()))
                .unwrap_or_else(|| DEFAULT_COMMIT_TYPE.into()),
            BranchPrefix::Custom(text) => {
                let parts: Vec<String> = text.split('/').map(slug).filter(|s| !s.is_empty()).collect();
                parts.join("/")
            }
        }
    }

    /// The branch the agent's chosen name becomes, as told to it.
    fn pattern(&self) -> String {
        match self {
            BranchPrefix::CommitType => "<type>/<name>".into(),
            prefix => match prefix.resolve(None) {
                p if p.is_empty() => "<name>".into(),
                p => format!("{p}/<name>"),
            },
        }
    }
}

/// How an agent asks to move its thread. Every field may be left out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct MoveRequest {
    /// The short name of the new branch.
    pub name: String,
    /// The Conventional Commits type of the work.
    #[serde(rename = "type")]
    pub commit_type: Option<String>,
    /// The branch, tag, or commit to start from; `None` for the current commit.
    pub base: Option<String>,
    /// Copy the checkout's uncommitted and untracked changes into the worktree.
    pub carry_changes: bool,
}

/// What a turn in the shared project checkout should know about it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sharing {
    /// Titles of the other threads running in the same checkout.
    pub running: Vec<String>,
    /// Files with uncommitted changes that this thread did not make.
    pub changed_files: usize,
}

impl Sharing {
    pub fn is_shared(&self) -> bool {
        !self.running.is_empty() || self.changed_files > 0
    }
}

/// The note put before the prompt when the checkout is shared, telling the
/// agent how to move the thread. `None` when there is nothing to tell.
/// `fetches` says Kiln fetches `origin` before it makes the worktree.
pub fn sharing_note(sharing: &Sharing, prefix: &BranchPrefix, fetches: bool) -> Option<String> {
    if !sharing.is_shared() {
        return None;
    }
    let mut facts = Vec::new();
    if !sharing.running.is_empty() {
        let titles: Vec<_> = sharing.running.iter().map(|t| format!("\"{t}\"")).collect();
        facts.push(format!(
            "- Other agent threads are working in this same folder right now: {}.",
            titles.join(", ")
        ));
    }
    if sharing.changed_files > 0 {
        let files = if sharing.changed_files == 1 { "file" } else { "files" };
        facts.push(format!(
            "- The checkout already has uncommitted changes in {} {files} that this thread did not make.",
            sharing.changed_files
        ));
    }
    let typed = *prefix == BranchPrefix::CommitType;
    let example = if typed {
        format!(r#"{MOVE_REQUEST} {{"name": "<short-name>", "type": "<type>", "base": "<ref>", "carry_changes": false}}"#)
    } else {
        format!(r#"{MOVE_REQUEST} {{"name": "<short-name>", "base": "<ref>", "carry_changes": false}}"#)
    };
    let type_field = if typed {
        format!(
            "\n- type: the Conventional Commits type of the task, one of {}.",
            COMMIT_TYPES.join(", ")
        )
    } else {
        String::new()
    };
    Some(format!(
        "<kiln-context>\n\
         This message comes from Kiln, the app running you, not from the user.\n\
         You are working in the project's shared Git checkout.\n\
         {facts}\n\
         If your task could collide with that work, you can move this thread to its own Git \
         worktree. Decide before you change any files: you may look around, but do not edit \
         anything here if you are going to move. To move, reply with this line, filled in:\n\
         {example}\n\
         - name: a short name for the branch, which becomes {pattern}.{type_field}\n\
         - base: the branch, tag, or commit the worktree starts from. Leave it out to start \
         from the current commit (HEAD).{fetch}\n\
         - carry_changes: true copies the uncommitted and untracked changes of this checkout \
         into the worktree; they also stay here. Leave it out or false to start clean.\n\
         Kiln stops your turn as soon as it reads the line, creates the worktree, and continues \
         your task there in a new session. If moving is not needed, ignore this note and do the \
         task.\n\
         </kiln-context>",
        facts = facts.join("\n"),
        pattern = prefix.pattern(),
        fetch = if fetches {
            " Kiln fetches origin first, so origin/<branch> names are up to date."
        } else {
            " Kiln does not fetch, so remote branches are as old as the last fetch."
        },
    ))
}

/// Split an agent's reply into the text to show and its move request, if it
/// made one. Only a line of its own counts. A line that is not valid JSON
/// still asks to move, with its text as the name.
pub fn take_move_request(text: &str) -> (String, Option<MoveRequest>) {
    let mut request = None;
    let kept: Vec<&str> = text
        .lines()
        .filter(|line| {
            let Some(rest) = line.trim().strip_prefix(MOVE_REQUEST) else { return true };
            if !(rest.is_empty() || rest.starts_with([':', ' ', '{'])) {
                return true;
            }
            let rest = rest.trim_start_matches(':').trim();
            request.get_or_insert_with(|| {
                serde_json::from_str(rest).unwrap_or_else(|_| MoveRequest {
                    name: rest.to_string(),
                    ..MoveRequest::default()
                })
            });
            false
        })
        .collect();
    (kept.join("\n").trim().to_string(), request)
}

/// Lowercase letters and digits of `text`, other runs made one dash.
fn slug(text: &str) -> String {
    let mut slug = String::new();
    for c in text.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
        if slug.len() >= 40 {
            break;
        }
    }
    slug.trim_end_matches('-').to_string()
}

/// A branch name under `prefix` made of `name`, or of the thread title when
/// the name has no usable characters.
pub fn branch_name(prefix: &str, name: &str, title: &str) -> String {
    let name = name.strip_prefix(&format!("{prefix}/")).unwrap_or(name);
    let slug = Some(slug(name)).filter(|s| !s.is_empty()).unwrap_or_else(|| slug(title));
    let slug = if slug.is_empty() { "thread".to_string() } else { slug };
    if prefix.is_empty() {
        slug
    } else {
        format!("{prefix}/{slug}")
    }
}

/// Continue in a fresh provider session using the visible thread as context.
/// Include tool activity and partial replies so stopped work can be continued.
pub fn handoff_prompt(messages: &[Message], request: &str, place: &str) -> String {
    use crate::store::AttachmentContent;
    let latest = messages.iter().rposition(|m| m.role == Role::User);
    let mut prompt = format!(
        "<kiln-context>\nYou are continuing this same thread in a new provider session. \
         The working directory is {place}. Continue the user's current task using the \
         conversation below, including decisions, completed work, and unfinished steps. \
         Earlier tool activity is historical context; verify current files before editing.\n\n"
    );
    for (i, message) in messages.iter().enumerate() {
        if Some(i) == latest && message.body == request {
            continue;
        }
        let who = match message.role {
            Role::User => "User",
            Role::Agent => "Agent",
            Role::Notice => "Kiln notice",
        };
        prompt.push_str(&format!("{who}: {}\n", message.body));
        if let Some(tool) = &message.tool {
            prompt.push_str(&format!("Tool: {tool}\n"));
        }
        for attachment in &message.attachments {
            prompt.push_str(&format!("Attachment: {}\n", attachment.name));
            if let AttachmentContent::Text(text) = &attachment.content {
                prompt.push_str(text);
                prompt.push('\n');
            }
        }
        prompt.push('\n');
    }
    prompt.push_str("</kiln-context>\n\n");
    prompt.push_str(request);
    prompt
}

/// Longest recap of earlier conversation sent to a new session, in characters.
const RECAP_CHARS: usize = 12_000;

/// The prompt that continues `request` in a new session after the thread
/// moved to `place`, with the conversation before `request` as a recap.
/// `messages` ends with the request itself or what followed it.
pub fn moved_prompt(messages: &[Message], request: &str, place: &str) -> String {
    let latest = messages.iter().rposition(|m| m.role == Role::User).unwrap_or(messages.len());
    let mut recap = String::new();
    for m in messages[..latest].iter().rev().filter(|m| m.tool.is_none()) {
        let who = match m.role {
            Role::User => "User",
            Role::Agent => "Agent",
            Role::Notice => continue,
        };
        let entry = format!("{who}: {}\n\n", m.body.trim());
        if recap.chars().count() + entry.chars().count() > RECAP_CHARS {
            recap.insert_str(0, "(earlier messages left out)\n\n");
            break;
        }
        recap.insert_str(0, &entry);
    }
    let mut prompt = format!(
        "<kiln-context>\nThis thread moved to {place}, in a new session. Work only there."
    );
    if !recap.is_empty() {
        prompt.push_str("\nThe conversation so far:\n\n");
        prompt.push_str(recap.trim_end());
    }
    prompt.push_str("\n</kiln-context>\n\n");
    prompt.push_str(request);
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_handoff_keeps_task_and_partial_work() {
        let messages = vec![
            Message::user("Implement the feature"),
            Message::tool_call("Bash", "cargo test"),
            Message::agent("Implementation done; tests still need fixing"),
            Message::notice("Stopped"),
        ];
        let prompt = handoff_prompt(&messages, "Implement the feature", "/project");
        assert!(prompt.contains("Tool: Bash"));
        assert!(prompt.contains("tests still need fixing"));
        assert!(prompt.contains("Stopped"));
        assert_eq!(prompt.matches("Implement the feature").count(), 1);
        assert!(prompt.ends_with("Implement the feature"));
    }

    #[test]
    fn a_project_request_is_a_final_control_line() {
        let line = r#"KILN_CREATE_PROJECT {"name": "my-app"}"#;
        let reply = format!("I’ll create a project.\n\n{line}\n");
        let (body, request) = take_project_request(&reply);
        assert_eq!(body, "I’ll create a project.");
        assert_eq!(request.unwrap().name, "my-app");
        for reply in [format!("```\n{line}\n```"), format!("> {line}"), "KILN_CREATE_PROJECT {}".into()] {
            assert_eq!(take_project_request(&reply).0, reply);
            assert!(take_project_request(&reply).1.is_none());
        }
    }

    #[test]
    fn checkouts_say_where_they_are() {
        let project = Path::new("/p");
        assert_eq!(Checkout::Project.dir(project), project);
        assert_eq!(Checkout::Project.describe(project), "the project checkout at /p");
        let tree = Checkout::Worktree { path: "/w".into(), branch: "kiln/x".into(), base: "main".into() };
        assert_eq!(tree.dir(project), Path::new("/w"));
        assert_eq!(tree.describe(project), "its own Git worktree at /w on the branch kiln/x");
    }

    #[test]
    fn the_note_names_what_shares_the_checkout() {
        let kiln = BranchPrefix::Kiln;
        assert_eq!(sharing_note(&Sharing::default(), &kiln, false), None);
        let shared = Sharing { running: vec!["Fix the build".into()], changed_files: 0 };
        let note = sharing_note(&shared, &kiln, false).unwrap();
        assert!(note.contains("\"Fix the build\""));
        assert!(!note.contains("uncommitted changes in"));
        assert!(note.contains(MOVE_REQUEST) && note.contains("kiln/<name>"));
        assert!(!note.contains("\"type\""));
        let note = sharing_note(&shared, &BranchPrefix::CommitType, false).unwrap();
        assert!(note.contains("<type>/<name>") && note.contains("\"type\"") && note.contains("refactor"));
        let note = sharing_note(&Sharing { running: vec![], changed_files: 1 }, &kiln, false).unwrap();
        assert!(note.contains("changes in 1 file that"));
        let note = sharing_note(&shared, &BranchPrefix::Custom(String::new()), false).unwrap();
        assert!(note.contains("becomes <name>."));
        assert!(note.contains("does not fetch"));
        let note = sharing_note(&shared, &kiln, true).unwrap();
        assert!(note.contains("fetches origin first"));
    }

    #[test]
    fn a_move_request_is_a_line_of_its_own() {
        let reply = "Another thread edits this.\nKILN_MOVE_TO_WORKTREE {\"name\": \"fix-login\", \
                     \"type\": \"fix\", \"base\": \"origin/main\", \"carry_changes\": true}\n";
        let (text, request) = take_move_request(reply);
        assert_eq!(text, "Another thread edits this.");
        assert_eq!(request, Some(MoveRequest {
            name: "fix-login".into(),
            commit_type: Some("fix".into()),
            base: Some("origin/main".into()),
            carry_changes: true,
        }));
        let (text, request) = take_move_request("I could reply with KILN_MOVE_TO_WORKTREE {}");
        assert_eq!(request, None);
        assert_eq!(text, "I could reply with KILN_MOVE_TO_WORKTREE {}");
        assert_eq!(take_move_request("KILN_MOVE_TO_WORKTREES").1, None);
        let (_, request) = take_move_request("KILN_MOVE_TO_WORKTREE: login");
        assert_eq!(request, Some(MoveRequest { name: "login".into(), ..MoveRequest::default() }));
        assert_eq!(take_move_request("  KILN_MOVE_TO_WORKTREE {}"), (String::new(), Some(MoveRequest::default())));
    }

    #[test]
    fn prefixes_follow_the_setting() {
        assert_eq!(BranchPrefix::Kiln.resolve(Some("fix")), "kiln");
        assert_eq!(BranchPrefix::CommitType.resolve(Some(" Fix ")), "fix");
        assert_eq!(BranchPrefix::CommitType.resolve(Some("bugfix")), "feat");
        assert_eq!(BranchPrefix::CommitType.resolve(None), "feat");
        assert_eq!(BranchPrefix::Custom("Team A//szymon/".into()).resolve(None), "team-a/szymon");
        assert_eq!(BranchPrefix::Custom("..".into()).resolve(None), "");
        let saved = serde_json::to_value(BranchPrefix::Custom("me".into())).unwrap();
        assert_eq!(saved, serde_json::json!({ "custom": "me" }));
    }

    #[test]
    fn branch_names_are_safe_slugs() {
        assert_eq!(branch_name("kiln", "Fix Login/Flow!", ""), "kiln/fix-login-flow");
        assert_eq!(branch_name("kiln", "kiln/already", ""), "kiln/already");
        assert_eq!(branch_name("fix", "..", "Add the sidebar"), "fix/add-the-sidebar");
        assert_eq!(branch_name("kiln", "", "★"), "kiln/thread");
        assert_eq!(branch_name("", "x", ""), "x");
        assert!(branch_name("kiln", &"a".repeat(100), "").len() <= "kiln/".len() + 40);
    }

    #[test]
    fn a_moved_prompt_recaps_the_earlier_conversation() {
        let messages = vec![
            Message::user("first"),
            Message::tool_call("Bash", "$ ls"),
            Message::agent("done first"),
            Message::notice("Stopped"),
            Message::user("second"),
            Message::agent(format!("{MOVE_REQUEST} {{}}")),
        ];
        let prompt = moved_prompt(&messages, "second", "/w");
        assert!(prompt.contains("User: first\n\nAgent: done first\n</kiln-context>\n\nsecond"));
        assert!(!prompt.contains("$ ls") && !prompt.contains("Stopped"));
        let prompt = moved_prompt(&messages[4..], "second", "/w");
        assert!(!prompt.contains("so far"));
        assert!(prompt.ends_with("</kiln-context>\n\nsecond"));
    }

    #[test]
    fn long_recaps_keep_the_latest_messages() {
        let mut messages: Vec<_> = (0..20).map(|i| Message::agent(format!("{i} {}", "x".repeat(1000)))).collect();
        messages.push(Message::user("now"));
        let prompt = moved_prompt(&messages, "now", "/w");
        assert!(prompt.contains("(earlier messages left out)"));
        assert!(prompt.contains("Agent: 19 ") && !prompt.contains("Agent: 0 "));
    }
}
