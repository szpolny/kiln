//! What Kiln reads from a project folder: where a path an agent asked for
//! really leads, which branch is checked out, and the project's own icon.

use std::io::{ErrorKind, Read};
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

/// Where projects commonly keep an icon, best first.
const ICON_FILES: [&str; 20] = [
    "favicon.svg",
    "favicon.ico",
    "favicon.png",
    "public/favicon.svg",
    "public/favicon.ico",
    "public/favicon.png",
    "app/favicon.ico",
    "app/favicon.png",
    "app/icon.svg",
    "app/icon.png",
    "app/icon.ico",
    "src/favicon.ico",
    "src/favicon.svg",
    "src/app/favicon.ico",
    "src/app/icon.svg",
    "src/app/icon.png",
    "assets/icon.svg",
    "assets/icon.png",
    "assets/logo.svg",
    "assets/logo.png",
];

/// Pages that may name an icon kept elsewhere.
const ICON_PAGES: [&str; 7] = [
    "index.html",
    "public/index.html",
    "src/index.html",
    "app/root.tsx",
    "src/root.tsx",
    "app/routes/__root.tsx",
    "src/routes/__root.tsx",
];

/// Largest icon file worth decoding.
const ICON_BYTES: u64 = 1 << 20;
/// How much of a page is searched for its icon.
const PAGE_BYTES: u64 = 128 << 10;

/// The project's own icon: the first file `decode` accepts, from the common
/// places and then from the pages that name one. Reads only inside the project.
pub fn icon<T>(dir: &Path, decode: impl Fn(&[u8]) -> Option<T>) -> Option<T> {
    let load = |path: &str| {
        // One byte over the limit tells a file that is too large.
        let bytes = read(dir, path, ICON_BYTES + 1)?;
        (bytes.len() as u64 <= ICON_BYTES).then(|| decode(&bytes))?
    };
    ICON_FILES.into_iter().find_map(load).or_else(|| {
        ICON_PAGES.into_iter().find_map(|page| {
            let page = read(dir, page, PAGE_BYTES)?;
            let page = String::from_utf8_lossy(&page);
            // Web projects serve `public/` as the root of the site.
            let mut found = declared_icons(&page)
                .filter_map(|href| load(&format!("public/{href}")).or_else(|| load(href)));
            found.next()
        })
    })
}

/// The start of the regular file at `path`, at most `limit` bytes of it. `None`
/// when it is missing or leads outside the project.
fn read(dir: &Path, path: &str, limit: u64) -> Option<Vec<u8>> {
    let file = dir.join(inside(dir, Path::new(path))?);
    // Opening a pipe would block, so check the kind first.
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(file).ok()?.take(limit).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// The local files a page names as its icon, relative to the site's root. Reads
/// both `<link rel="icon" href="…">` and `{ rel: "icon", href: "…" }`.
fn declared_icons(page: &str) -> impl Iterator<Item = &str> {
    page.split(['<', '{', '}']).filter_map(|part| {
        let rel = attribute(part, "rel")?;
        if !rel.split_whitespace().any(|word| word.eq_ignore_ascii_case("icon")) {
            return None;
        }
        let href = attribute(part, "href")?.split(['?', '#']).next()?;
        // A scheme or a host names no file here: nothing is fetched.
        if href.contains(':') || href.starts_with("//") {
            return None;
        }
        Some(href.trim_start_matches('/')).filter(|href| !href.is_empty())
    })
}

/// The quoted value given to `name` in a tag or an object.
fn attribute<'a>(part: &'a str, name: &str) -> Option<&'a str> {
    part.match_indices(name).find_map(|(at, _)| {
        // Not the tail of a longer name, such as `data-href`.
        let starts = part[..at].chars().next_back();
        if !starts.is_none_or(|c| c.is_whitespace() || c == ',') {
            return None;
        }
        let value = part[at + name.len()..].trim_start().strip_prefix(['=', ':'])?.trim_start();
        let quote = value.chars().next().filter(|c| matches!(c, '"' | '\''))?;
        value[1..].split_once(quote).map(|(value, _)| value)
    })
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
    fn finds_the_icon_a_project_ships() {
        let base = std::env::temp_dir().join(format!("kiln-icon-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let dir = base.join("project");
        fs::create_dir_all(dir.join("public/img")).unwrap();
        fs::create_dir_all(dir.join("assets")).unwrap();
        let find = || {
            icon(&dir, |bytes| {
                let text = String::from_utf8_lossy(bytes).into_owned();
                (text != "broken").then_some(text)
            })
        };
        assert_eq!(find(), None);

        // A page's icon counts only when no common place has one.
        fs::write(dir.join("index.html"), r#"<link rel="icon" href="/img/mark.png?v=2">"#).unwrap();
        fs::write(dir.join("public/img/mark.png"), "declared").unwrap();
        assert_eq!(find().as_deref(), Some("declared"));
        fs::write(dir.join("assets/logo.png"), "logo").unwrap();
        assert_eq!(find().as_deref(), Some("logo"));
        fs::write(dir.join("public/favicon.png"), "favicon").unwrap();
        assert_eq!(find().as_deref(), Some("favicon"));

        // Unusable files give way to the next candidate.
        fs::write(dir.join("favicon.svg"), "broken").unwrap();
        fs::write(dir.join("favicon.ico"), vec![b'a'; ICON_BYTES as usize + 1]).unwrap();
        assert_eq!(find().as_deref(), Some("favicon"));
        fs::create_dir(dir.join("favicon.png")).unwrap();
        assert_eq!(find().as_deref(), Some("favicon"));

        // Nothing outside the project is read, by link or by a page's path.
        fs::write(base.join("secret.png"), "secret").unwrap();
        fs::remove_file(dir.join("favicon.svg")).unwrap();
        symlink(base.join("secret.png"), dir.join("favicon.svg")).unwrap();
        assert_eq!(find().as_deref(), Some("favicon"));
        fs::remove_file(dir.join("public/favicon.png")).unwrap();
        fs::remove_file(dir.join("assets/logo.png")).unwrap();
        fs::write(dir.join("index.html"), r#"<link rel="icon" href="../../secret.png">"#).unwrap();
        assert_eq!(find(), None);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn reads_the_icons_a_page_declares() {
        let page = r##"
            <link rel="stylesheet" href="/style.css">
            <link rel="apple-touch-icon" href="/touch.png">
            <link rel="icon" data-href="/no.png" href='/a.svg#x'>
            <link href="b.ico" rel="shortcut icon" />
            <link rel="icon" href="https://example.com/c.png">
            <link rel="icon" href="//example.com/d.png">
            <link rel="icon" href="data:image/png;base64,AAAA">
            <link rel="icon" href={favicon}>
            links: () => [{ rel: "icon", href: "/e.png" }],
        "##;
        let found: Vec<_> = declared_icons(page).collect();
        assert_eq!(found, ["a.svg", "b.ico", "e.png"]);
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
