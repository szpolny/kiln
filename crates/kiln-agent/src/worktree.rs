//! Git worktrees Kiln makes for threads, through the `git` command.
//!
//! The repository's hooks never run: making a checkout must not execute code
//! from the project. Nothing here forces Git past a refusal, so uncommitted
//! work and branches are never lost.

use kiln_core::checkout::Checkout;
use kiln_core::Agent;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir)
        .args(["-c", "core.hooksPath=/dev/null"])
        .env("GIT_TERMINAL_PROMPT", "0")
        // Only the repository in `dir`, whatever the environment points at.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE");
    cmd
}

/// Run `cmd` and return its output, or its error message when it fails.
fn run(cmd: &mut Command) -> io::Result<String> {
    let out = cmd.output()?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string());
    }
    let error = String::from_utf8_lossy(&out.stderr);
    let error = error.trim().strip_prefix("fatal: ").unwrap_or(error.trim());
    Err(io::Error::other(if error.is_empty() { "git failed" } else { error }.to_string()))
}

/// Whether `dir` is inside a Git working tree.
pub fn is_repo(dir: &Path) -> bool {
    run(git(dir).args(["rev-parse", "--is-inside-work-tree"])).is_ok_and(|out| out == "true")
}

/// Load the provider's ignored instruction file from the project checkout when
/// the worktree has no override of its own.
pub fn local_instructions(repo: &Path, tree: &Path, agent: Agent) -> io::Result<Option<String>> {
    let name = match agent {
        Agent::Codex => "AGENTS.override.md",
        Agent::Claude => "CLAUDE.local.md",
    };
    let source = repo.join(name);
    let result = (|| {
        match fs::symlink_metadata(tree.join(name)) {
            Ok(_) => return Ok(None),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        match fs::metadata(&source) {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => return Ok(None),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        }
        let ignored = git(repo).args(["check-ignore", "-q", "--", name]).output()?;
        match ignored.status.code() {
            Some(0) => {}
            Some(1) => return Ok(None),
            _ => return Err(io::Error::other(format!(
                "git check-ignore exited with {}: {}",
                ignored.status,
                String::from_utf8_lossy(&ignored.stderr).trim(),
            ))),
        }
        let instructions = fs::read_to_string(&source)?;
        Ok((!instructions.trim().is_empty()).then(|| {
            format!(
                "<kiln-project-instructions>\n\
                 The following local project instructions come from {name}. Apply them in this worktree.\n\n\
                 {instructions}\n\
                 </kiln-project-instructions>"
            )
        }))
    })();
    result.map_err(|e| {
        io::Error::other(format!("Could not load local instructions from {}: {e}", source.display()))
    })
}

/// How many files in the checkout at `dir` have uncommitted changes, untracked
/// ones included.
pub fn changed_files(dir: &Path) -> io::Result<usize> {
    let status = run(git(dir).args(["status", "--porcelain", "--untracked-files=normal"]))?;
    Ok(status.lines().filter(|line| !line.is_empty()).count())
}

/// What a new worktree starts from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Base {
    /// The commit the project checkout is on.
    Head,
    /// The latest commit of `main`, or else `master`: the local branch, or
    /// `origin`'s when the last fetch left it ahead. Nothing is fetched.
    Main,
    /// A branch, tag, or commit.
    Ref(String),
}

/// How to make a worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The branch to create; a number is added if it is taken.
    pub branch: String,
    pub base: Base,
    /// Copy the project checkout's uncommitted and untracked changes in.
    pub carry_changes: bool,
    /// Fetch `origin` first, which reaches the network.
    pub fetch: bool,
}

#[derive(Debug)]
pub struct Created {
    pub checkout: Checkout,
    /// With [`Plan::carry_changes`]: how many changed files were copied, or
    /// why none were.
    pub carried: Option<Result<usize, String>>,
    /// With [`Plan::fetch`]: whether fetching `origin` worked. A failed fetch
    /// leaves what was fetched before, and the worktree is made anyway.
    pub fetched: Option<Result<(), String>>,
}

/// The commit `rev` names in `repo`.
fn commit(repo: &Path, rev: &str) -> Option<String> {
    let out = run(git(repo).args(["rev-parse", "--verify", "--quiet"]).arg(format!("{rev}^{{commit}}")));
    out.ok().filter(|sha| !sha.is_empty())
}

/// The name and commit a worktree starts from.
fn resolve(repo: &Path, base: &Base) -> io::Result<(String, String)> {
    match base {
        Base::Head => {
            let sha = commit(repo, "HEAD").ok_or_else(|| {
                io::Error::other("the repository has no commit to start a worktree from")
            })?;
            let name = run(git(repo).args(["symbolic-ref", "--short", "-q", "HEAD"]))
                .unwrap_or_else(|_| sha[..sha.len().min(7)].to_string());
            Ok((name, sha))
        }
        Base::Main => {
            let ahead = |old: &str, new: &str| {
                run(git(repo).args(["merge-base", "--is-ancestor", old, new])).is_ok()
            };
            for name in ["main", "master"] {
                let local = commit(repo, &format!("refs/heads/{name}"));
                let remote = commit(repo, &format!("refs/remotes/origin/{name}"));
                return Ok(match (local, remote) {
                    (Some(l), Some(r)) if l != r && ahead(&l, &r) => (format!("origin/{name}"), r),
                    (Some(l), _) => (name.to_string(), l),
                    (None, Some(r)) => (format!("origin/{name}"), r),
                    (None, None) => continue,
                });
            }
            Err(io::Error::other("the repository has no main or master branch"))
        }
        Base::Ref(name) => {
            let name = name.trim();
            // Never mistaken for an option of git.
            let sha = (!name.is_empty() && !name.starts_with('-'))
                .then(|| commit(repo, name))
                .flatten()
                .ok_or_else(|| io::Error::other(format!("there is no branch, tag, or commit named {name:?}")))?;
            Ok((name.to_string(), sha))
        }
    }
}

/// Make a worktree of the repository at `repo` in a new folder under `root`.
pub fn create(repo: &Path, root: &Path, plan: &Plan) -> io::Result<Created> {
    let fetched = plan.fetch.then(|| {
        run(git(repo).args(["fetch", "--quiet", "origin"])).map(drop).map_err(|e| e.to_string())
    });
    let (base, sha) = resolve(repo, &plan.base)?;
    let taken = |branch: &str, path: &Path| {
        path.exists()
            || run(git(repo).args(["show-ref", "--verify", "--quiet"])
                .arg(format!("refs/heads/{branch}"))).is_ok()
    };
    let folder = |branch: &str| root.join(branch.replace('/', "-"));
    let (mut branch, mut n) = (plan.branch.clone(), 1);
    while taken(&branch, &folder(&branch)) {
        n += 1;
        branch = format!("{}-{n}", plan.branch);
    }
    let path = folder(&branch);
    std::fs::create_dir_all(root)?;
    run(git(repo).args(["worktree", "add", "-b", &branch]).arg(&path).arg(&sha))?;
    // The path turns compare against, with links resolved.
    let path = path.canonicalize().unwrap_or(path);
    let carried = plan.carry_changes.then(|| carry(repo, &path).map_err(|e| e.to_string()));
    Ok(Created { checkout: Checkout::Worktree { path, branch, base }, carried, fetched })
}

/// Copy the uncommitted and untracked changes of `repo` into the new worktree
/// at `tree`, leaving `repo` as it is. On a conflict the worktree is reset, so
/// it starts clean. Returns how many files were changed.
fn carry(repo: &Path, tree: &Path) -> io::Result<usize> {
    let changed = changed_files(repo)?;
    // A commit of the tracked changes, made without touching the checkout.
    let stash = run(git(repo).args(["stash", "create"]))?;
    if !stash.is_empty() {
        if let Err(e) = run(git(tree).args(["stash", "apply", "--quiet", &stash])) {
            let _ = run(git(tree).args(["reset", "--hard", "--quiet"]));
            return Err(e);
        }
    }
    let untracked = run(git(repo).args(["ls-files", "--others", "--exclude-standard", "-z"]))?;
    for file in untracked.split('\0').filter(|f| !f.is_empty()) {
        copy(&repo.join(file), &tree.join(file))?;
    }
    Ok(changed)
}

/// Copy a file, or a link as a link, unless `to` already exists.
fn copy(from: &Path, to: &Path) -> io::Result<()> {
    if to.symlink_metadata().is_ok() {
        return Ok(());
    }
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let meta = from.symlink_metadata()?;
    if meta.is_symlink() {
        #[cfg(unix)]
        std::os::unix::fs::symlink(std::fs::read_link(from)?, to)?;
    } else if meta.is_file() {
        std::fs::copy(from, to)?;
    }
    Ok(())
}

/// Remove the worktree at `path` of the repository at `repo`. Git refuses
/// while it holds uncommitted or untracked files. Its branch stays.
pub fn remove(repo: &Path, path: &Path) -> io::Result<()> {
    if !path.exists() {
        // Already gone: only Git's record of it is left.
        return run(git(repo).args(["worktree", "prune"])).map(drop);
    }
    run(git(repo).args(["worktree", "remove"]).arg(path)).map(drop)
}

/// Where Kiln keeps the worktrees of the project named `project`.
pub fn root(data_dir: &Path, project: &str) -> PathBuf {
    let name: String = project
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    data_dir.join("worktrees").join(if name.is_empty() { "project" } else { &name })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("kiln-worktree-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let repo = base.join("repo");
        fs::create_dir_all(&repo).unwrap();
        let sh = |args: &[&str]| run(git(&repo).args(args)).unwrap();
        sh(&["init", "-q", "-b", "main"]);
        sh(&["config", "user.email", "kiln@example.invalid"]);
        sh(&["config", "user.name", "Kiln"]);
        fs::write(repo.join("a.txt"), "a").unwrap();
        sh(&["add", "."]);
        sh(&["commit", "-q", "-m", "init"]);
        repo
    }

    fn plan(branch: &str, base: Base, carry_changes: bool) -> Plan {
        Plan { branch: branch.into(), base, carry_changes, fetch: false }
    }

    fn tree(created: &Created) -> (&Path, &str, &str) {
        let Checkout::Worktree { path, branch, base } = &created.checkout else { panic!("expected a worktree") };
        (path, branch, base)
    }

    #[test]
    fn worktrees_inherit_only_ignored_instructions_for_their_provider() {
        let repo = repo("instructions");
        let root = repo.parent().unwrap().join("trees");
        let created = create(&repo, &root, &plan("instructions", Base::Head, false)).unwrap();
        let (tree, _, _) = tree(&created);
        let codex = "AGENTS.override.md";
        let claude = "CLAUDE.local.md";
        fs::write(repo.join(".git/info/exclude"), format!("/{codex}\n")).unwrap();
        fs::write(repo.join(codex), "Codex instructions").unwrap();
        fs::write(repo.join(claude), "Claude instructions").unwrap();

        let read = |agent| local_instructions(&repo, tree, agent).unwrap();
        assert!(read(Agent::Codex).unwrap().contains("Codex instructions"));
        assert!(read(Agent::Claude).is_none());
        assert!(!tree.join(codex).exists());
        assert_eq!(changed_files(tree).unwrap(), 0);

        fs::write(repo.join(".git/info/exclude"), format!("/{codex}\n/{claude}\n")).unwrap();
        fs::write(repo.join(codex), "Updated instructions").unwrap();
        assert!(read(Agent::Codex).unwrap().contains("Updated instructions"));
        assert!(read(Agent::Claude).unwrap().contains("Claude instructions"));
        fs::write(tree.join(codex), "Worktree instructions").unwrap();
        assert!(read(Agent::Codex).is_none());
        fs::remove_file(repo.join(claude)).unwrap();
        assert!(read(Agent::Claude).is_none());
        fs::remove_dir_all(repo.parent().unwrap()).unwrap();
    }

    #[test]
    fn creates_separate_worktrees_and_removes_only_clean_ones() {
        let repo = repo("create");
        let root = repo.parent().unwrap().join("trees");
        assert!(is_repo(&repo));
        assert_eq!(changed_files(&repo).unwrap(), 0);
        fs::write(repo.join("b.txt"), "b").unwrap();
        assert_eq!(changed_files(&repo).unwrap(), 1);

        let first = create(&repo, &root, &plan("kiln/fix-it", Base::Head, false)).unwrap();
        let second = create(&repo, &root, &plan("kiln/fix-it", Base::Head, false)).unwrap();
        let ((a, branch, base), (b, other, _)) = (tree(&first), tree(&second));
        assert_eq!((branch, other, base), ("kiln/fix-it", "kiln/fix-it-2", "main"));
        assert_ne!(a, b);
        assert!(first.carried.is_none());
        assert_eq!(crate::project::branch(a).as_deref(), Some("kiln/fix-it"));
        // Uncommitted work stays in the project checkout.
        assert!(a.join("a.txt").exists() && !a.join("b.txt").exists());

        fs::write(a.join("c.txt"), "c").unwrap();
        assert!(remove(&repo, a).is_err());
        assert!(a.exists());
        fs::remove_file(a.join("c.txt")).unwrap();
        remove(&repo, a).unwrap();
        assert!(!a.exists());
        let kept = run(git(&repo).args(["show-ref", "--verify", "refs/heads/kiln/fix-it"]));
        assert!(kept.is_ok(), "the branch stays");

        fs::remove_dir_all(b).unwrap();
        remove(&repo, b).unwrap();
        let _ = fs::remove_dir_all(repo.parent().unwrap());
    }

    #[test]
    fn starts_from_the_latest_main_or_any_ref() {
        let repo = repo("base");
        let root = repo.parent().unwrap().join("trees");
        let sh = |args: &[&str]| run(git(&repo).args(args)).unwrap();
        sh(&["switch", "-q", "-c", "topic"]);
        fs::write(repo.join("topic.txt"), "t").unwrap();
        sh(&["add", "."]);
        sh(&["commit", "-q", "-m", "topic"]);

        let main = create(&repo, &root, &plan("a", Base::Main, false)).unwrap();
        let (path, _, base) = tree(&main);
        assert_eq!(base, "main");
        assert!(!path.join("topic.txt").exists());

        // A fetch that moved origin's main ahead makes it the latest.
        let topic = sh(&["rev-parse", "topic"]);
        sh(&["update-ref", "refs/remotes/origin/main", &topic]);
        let ahead = create(&repo, &root, &plan("b", Base::Main, false)).unwrap();
        let (path, _, base) = tree(&ahead);
        assert_eq!(base, "origin/main");
        assert!(path.join("topic.txt").exists());

        let head = create(&repo, &root, &plan("c", Base::Head, false)).unwrap();
        assert_eq!(tree(&head).2, "topic");
        let tag = create(&repo, &root, &plan("d", Base::Ref("main~0".into()), false)).unwrap();
        assert_eq!(tree(&tag).2, "main~0");
        for bad in ["nope", "--orphan", ""] {
            assert!(create(&repo, &root, &plan("e", Base::Ref(bad.into()), false)).is_err(), "{bad}");
        }
        sh(&["branch", "-q", "-m", "main", "trunk"]);
        sh(&["update-ref", "-d", "refs/remotes/origin/main"]);
        assert!(create(&repo, &root, &plan("f", Base::Main, false)).is_err());
        let _ = fs::remove_dir_all(repo.parent().unwrap());
    }

    #[test]
    fn carries_uncommitted_changes_and_leaves_them_in_place() {
        let repo = repo("carry");
        let root = repo.parent().unwrap().join("trees");
        fs::write(repo.join("a.txt"), "edited").unwrap();
        fs::create_dir_all(repo.join("new")).unwrap();
        fs::write(repo.join("new/n.txt"), "n").unwrap();
        fs::write(repo.join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(repo.join("ignored.txt"), "i").unwrap();

        let created = create(&repo, &root, &plan("x", Base::Head, true)).unwrap();
        assert_eq!(created.carried, Some(Ok(3)));
        let path = tree(&created).0;
        assert_eq!(fs::read_to_string(path.join("a.txt")).unwrap(), "edited");
        assert_eq!(fs::read_to_string(path.join("new/n.txt")).unwrap(), "n");
        assert!(!path.join("ignored.txt").exists());
        assert_eq!(fs::read_to_string(repo.join("a.txt")).unwrap(), "edited");
        assert_eq!(changed_files(&repo).unwrap(), 3);

        // Changes that do not apply to the base leave the worktree clean.
        let sh = |args: &[&str]| run(git(&repo).args(args)).unwrap();
        sh(&["stash", "-q", "-u"]);
        sh(&["switch", "-q", "-c", "other"]);
        fs::write(repo.join("a.txt"), "other").unwrap();
        sh(&["commit", "-q", "-am", "other"]);
        sh(&["switch", "-q", "main"]);
        sh(&["stash", "pop", "-q"]);
        let created = create(&repo, &root, &plan("y", Base::Ref("other".into()), true)).unwrap();
        assert!(matches!(created.carried, Some(Err(_))));
        let path = tree(&created).0;
        assert_eq!(fs::read_to_string(path.join("a.txt")).unwrap(), "other");
        assert_eq!(changed_files(path).unwrap(), 0);
        let _ = fs::remove_dir_all(repo.parent().unwrap());
    }

    #[test]
    fn fetches_origin_first_when_asked() {
        let upstream = repo("upstream");
        let repo = repo("fetching");
        let root = repo.parent().unwrap().join("trees");
        let sh = |dir: &Path, args: &[&str]| run(git(dir).args(args)).unwrap();
        sh(&repo, &["remote", "add", "origin", &upstream.to_string_lossy()]);
        sh(&repo, &["fetch", "-q", "origin"]);
        sh(&repo, &["reset", "-q", "--hard", "origin/main"]);
        fs::write(upstream.join("new.txt"), "n").unwrap();
        sh(&upstream, &["add", "."]);
        sh(&upstream, &["commit", "-q", "-m", "new"]);

        let local = create(&repo, &root, &plan("a", Base::Main, false)).unwrap();
        assert_eq!((tree(&local).2, &local.fetched), ("main", &None));
        let fetching = Plan { fetch: true, ..plan("b", Base::Main, false) };
        let fresh = create(&repo, &root, &fetching).unwrap();
        assert_eq!(fresh.fetched, Some(Ok(())));
        assert_eq!(tree(&fresh).2, "origin/main");
        assert!(tree(&fresh).0.join("new.txt").exists());

        // Without a reachable origin the worktree is still made.
        sh(&repo, &["remote", "remove", "origin"]);
        let offline = create(&repo, &root, &Plan { fetch: true, ..plan("c", Base::Main, false) }).unwrap();
        assert!(matches!(offline.fetched, Some(Err(_))));
        let _ = fs::remove_dir_all(repo.parent().unwrap());
        let _ = fs::remove_dir_all(upstream.parent().unwrap());
    }

    #[test]
    fn folders_outside_git_are_not_repositories() {
        let dir = std::env::temp_dir().join(format!("kiln-worktree-plain-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        assert!(!is_repo(&dir));
        assert!(changed_files(&dir).is_err());
        assert!(create(&dir, &dir.join("trees"), &plan("x", Base::Head, false)).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn roots_are_one_folder_per_project_name() {
        assert_eq!(root(Path::new("/d"), "my app/x"), Path::new("/d/worktrees/my-app-x"));
        assert_eq!(root(Path::new("/d"), ""), Path::new("/d/worktrees/project"));
    }
}
