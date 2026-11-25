//! Where a path an agent asked for really leads.

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
}
