//! What Kiln reads from a project folder: where a path an agent asked for
//! really leads, and which branch is checked out.

use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

/// `path` relative to the project in `dir`, following links and `..` the way
/// the file system does. A relative `path` starts at `dir`. `None` when it
/// leads outside the project or that cannot be told.
pub fn inside(dir: &Path, path: &Path) -> Option<PathBuf> {
    // A shell would expand `~`; never mistake it for a folder in the project.
    if path.starts_with("~") {
        return None;
    }
    let root = dir.canonicalize().ok()?;
    let mut at = root.clone();
    for part in path.components() {
        match part {
            Component::Prefix(_) | Component::RootDir => at = PathBuf::from(part.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                at.pop();
            }
            Component::Normal(name) => {
                at.push(name);
                match at.canonicalize() {
                    Ok(real) => at = real,
                    // A file yet to be created is fine; a dangling link is not.
                    Err(e) if e.kind() == ErrorKind::NotFound && at.symlink_metadata().is_err() => {}
                    Err(_) => return None,
                }
            }
        }
    }
    at.strip_prefix(&root).ok().map(Path::to_path_buf)
}

/// The branch checked out where `dir` lives, or a short commit id when HEAD is
/// detached. Reads Git's files directly. `None` outside a repository.
pub fn branch(dir: &Path) -> Option<String> {
    let dot_git = dir.ancestors().map(|at| at.join(".git")).find(|at| at.exists())?;
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        // Worktrees and submodules hold a file that points to the real folder.
        let link = std::fs::read_to_string(&dot_git).ok()?;
        dot_git.parent()?.join(link.strip_prefix("gitdir:")?.trim())
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref:") {
        Some(name) => {
            let name = name.trim();
            Some(name.strip_prefix("refs/heads/").unwrap_or(name).to_string())
        }
        None => head.get(..7).map(str::to_string),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    #[test]
    fn follows_dots_and_links_out_of_the_project() {
        let base = std::env::temp_dir().join(format!("kiln-inside-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let (dir, other) = (base.join("project"), base.join("other"));
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(&other).unwrap();
        symlink(&other, dir.join("out")).unwrap();
        symlink(other.join("missing"), dir.join("dangling")).unwrap();
        symlink(dir.join("src"), base.join("back")).unwrap();

        let at = |path: &Path| inside(&dir, path);
        assert_eq!(at(&dir.join("src/new/file.rs")), Some("src/new/file.rs".into()));
        assert_eq!(at(Path::new("src/../a.txt")), Some("a.txt".into()));
        assert_eq!(at(&base.join("back/a.rs")), Some("src/a.rs".into()));
        assert_eq!(at(Path::new(".")), Some("".into()));

        assert_eq!(at(&other.join("a.txt")), None);
        assert_eq!(at(Path::new("../other/a.txt")), None);
        assert_eq!(at(&dir.join("new/../../other/a.txt")), None);
        assert_eq!(at(&dir.join("out/a.txt")), None);
        assert_eq!(at(&dir.join("dangling")), None);
        assert_eq!(at(Path::new("~/.ssh/id")), None);
        assert_eq!(inside(&base.join("gone"), Path::new("a")), None);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn reads_the_checked_out_branch() {
        let base = std::env::temp_dir().join(format!("kiln-branch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let (repo, tree) = (base.join("repo"), base.join("tree"));
        let linked = repo.join(".git/worktrees/tree");
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::create_dir_all(&linked).unwrap();
        fs::create_dir_all(&tree).unwrap();
        fs::write(repo.join(".git/HEAD"), "ref: refs/heads/feat/list\n").unwrap();
        fs::write(linked.join("HEAD"), "0123456789abcdef\n").unwrap();
        fs::write(tree.join(".git"), "gitdir: ../repo/.git/worktrees/tree\n").unwrap();

        assert_eq!(branch(&repo).as_deref(), Some("feat/list"));
        assert_eq!(branch(&repo.join("src")).as_deref(), Some("feat/list"));
        assert_eq!(branch(&tree).as_deref(), Some("0123456"));
        fs::write(repo.join(".git/HEAD"), "").unwrap();
        assert_eq!(branch(&repo), None);
        let _ = fs::remove_dir_all(&base);
    }
}
