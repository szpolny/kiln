//! Checkout paths offered by the composer and the Markdown it inserts.

use crate::{assets, media};
use gpui::{font, point, px, quad, size, App, Bounds, Hsla, PaintQuad, Pixels, Point, TextRun};
use pulldown_cmark::{Event, Parser, Tag};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;

const MAX_SUGGESTIONS: usize = 6;

#[derive(Clone, PartialEq, Eq)]
pub struct Query {
    pub range: Range<usize>,
    prefix: String,
}

pub fn query(text: &str, cursor: usize, code: &[Range<usize>]) -> Option<Query> {
    let path_char = |c: char| c.is_alphanumeric() || matches!(c, '/' | '.' | '-' | '_');
    let start = text[..cursor].rfind(|c| !path_char(c))?;
    if text.as_bytes()[start] != b'@' || text[..start].ends_with(|c| path_char(c) || c == '@') {
        return None;
    }
    let suffix = &text[cursor..];
    let rest = &suffix[..suffix.find(|c| !path_char(c)).unwrap_or(suffix.len())];
    let end = cursor + rest.trim_end_matches('.').len();
    let prefix = text[start + 1..cursor].trim_start_matches("./");
    if prefix.starts_with('/') || code.iter().any(|r| r.start < end && start < r.end) {
        return None;
    }
    Some(Query {
        range: start..end,
        prefix: prefix.to_lowercase(),
    })
}

#[derive(Clone, Debug)]
pub struct Reference {
    pub source: Range<usize>,
    pub path: PathBuf,
}

pub fn references(text: &str, root: Option<&Path>) -> Vec<Reference> {
    if !text.contains(['[', '`']) {
        return vec![];
    }
    let mut references = vec![];
    for (event, source) in Parser::new(text).into_offset_iter() {
        if references
            .last()
            .is_some_and(|last: &Reference| last.source.end > source.start)
        {
            continue;
        }
        let path = match event {
            Event::Start(Tag::Link { dest_url, .. }) => media::link_path(&dest_url),
            // Older drafts stored selected paths as inline code.
            Event::Code(path) => existing_path(root, &path),
            _ => None,
        };
        if let Some(path) = path {
            references.push(Reference { source, path });
        }
    }
    references
}

/// A path written as inline code becomes a reference only when it names a checked-out file.
pub fn existing_path(root: Option<&Path>, value: &str) -> Option<PathBuf> {
    let root = root?;
    let literal = Path::new(value);
    if media::scoped_path(root, literal).is_some_and(|path| path.is_file()) {
        return Some(literal.to_owned());
    }
    let path = media::link_path(value)?;
    if path == literal {
        return None;
    }
    media::scoped_path(root, &path)
        .filter(|path| path.is_file())
        .map(|_| path)
}

pub fn markup(path: &str) -> String {
    let label = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path);
    let mut escaped = String::new();
    for c in label.chars() {
        if matches!(c, '\\' | '[' | ']' | '`') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    format!("[{escaped}](<{}>)", link_destination(path))
}

pub fn link_destination(path: &str) -> String {
    use percent_encoding::{utf8_percent_encode, CONTROLS};
    const MARKDOWN_PATH: &percent_encoding::AsciiSet = &CONTROLS
        .add(b' ')
        .add(b'%')
        .add(b'#')
        .add(b':')
        .add(b'<')
        .add(b'>')
        .add(b'\\')
        .add(b'&');
    utf8_percent_encode(path, MARKDOWN_PATH).to_string()
}

pub fn reveal(root: &Path, path: &Path, cx: &App) {
    if let Some(path) = media::scoped_path(root, path).filter(|path| path.is_file()) {
        cx.reveal_path(&path);
    }
}

#[derive(Clone, Copy, Default, PartialEq)]
pub struct ChipColors {
    pub text: Hsla,
    pub border: Hsla,
}

pub fn file_icon(path: &Path) -> (assets::Icon, Option<u32>) {
    use assets::Icon;
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("tsx" | "jsx") => (Icon::Atom, Some(0x268ba9)),
        Some("ts" | "mts" | "cts") => (Icon::FileCode, Some(0x3178c6)),
        Some("js" | "mjs" | "cjs") => (Icon::FileCode, Some(0x9c7417)),
        Some("json" | "jsonc") => (Icon::FileJson, None),
        Some("png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tiff" | "svg" | "ico") => {
            (Icon::Image, None)
        }
        Some("md" | "txt" | "rst" | "pdf") => (Icon::FileText, None),
        Some("sh" | "zsh" | "bash" | "fish") => (Icon::FileTerminal, None),
        Some(
            "rs" | "py" | "rb" | "go" | "swift" | "c" | "cpp" | "h" | "java" | "kt" | "css"
            | "html" | "nix",
        ) => (Icon::FileCode, None),
        Some("zip" | "tar" | "gz" | "xz" | "7z") => (Icon::FileArchive, None),
        _ => (Icon::File, None),
    }
}

pub fn append_chip(
    path: &Path,
    text: &mut String,
    runs: &mut Vec<TextRun>,
    run: &TextRun,
    colors: ChipColors,
) -> Range<usize> {
    let start = text.len();
    let padding = "\u{a0}\u{a0}";
    let chip = TextRun {
        len: padding.len(),
        color: colors.text,
        background_color: None,
        underline: None,
        strikethrough: None,
        ..run.clone()
    };
    text.push_str(padding);
    runs.push(chip.clone());
    let (icon, tint) = file_icon(path);
    let icon = char::from(icon);
    text.push(icon);
    runs.push(TextRun {
        len: icon.len_utf8(),
        font: font(assets::ICON_FONT),
        color: tint.map_or(chip.color, |tint| gpui::rgb(tint).into()),
        ..chip.clone()
    });
    let name = path
        .file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy();
    let length = text.len();
    text.push_str(padding);
    for (i, c) in name.chars().enumerate() {
        if i == 32 {
            text.push('…');
            break;
        }
        text.push(if c.is_whitespace() || c.is_control() {
            '\u{a0}'
        } else {
            c
        });
    }
    text.push_str(padding);
    runs.push(TextRun {
        len: text.len() - length,
        ..chip
    });
    start..text.len()
}

pub fn chip_bounds(
    range: &Range<usize>,
    line_height: Pixels,
    position: impl Fn(usize) -> Point<Pixels>,
) -> Bounds<Pixels> {
    // An index exactly at a wrap boundary is placed on the preceding row.
    // The first padding character lets us locate the row containing the icon.
    let first = position(range.start);
    let inner = position(range.start + '\u{a0}'.len_utf8());
    let end = position(range.end);
    let start = if first.y < inner.y {
        point(inner.x - px(4.), inner.y)
    } else {
        first
    };
    let width = if start.y == end.y {
        end.x - start.x
    } else {
        end.x
    };
    Bounds::new(
        point(
            if start.y == end.y { start.x } else { px(0.) },
            end.y + px(1.),
        ),
        size(width.max(px(0.)), (line_height - px(2.)).max(px(0.))),
    )
}

pub fn chip_border(bounds: Bounds<Pixels>, colors: ChipColors) -> PaintQuad {
    quad(
        bounds,
        px(5.),
        gpui::transparent_black(),
        px(1.),
        colors.border,
        Default::default(),
    )
}

pub struct File {
    path: String,
    folded: String,
}

pub fn index(root: &Path) -> Vec<File> {
    let mut paths = match Command::new("git")
        .current_dir(root)
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .output()
    {
        Ok(output) if output.status.success() => output
            .stdout
            .split(|&byte| byte == 0)
            .filter_map(|bytes| std::str::from_utf8(bytes).ok())
            .filter(|path| !path.is_empty() && !path.contains(['\n', '\r']))
            .map(str::to_owned)
            .collect(),
        _ => directory_files(root),
    };
    paths.sort_unstable();
    paths.dedup();
    paths
        .into_iter()
        .map(|path| File {
            folded: path.to_lowercase(),
            path,
        })
        .collect()
}

fn directory_files(root: &Path) -> Vec<String> {
    let mut pending = vec![root.to_path_buf()];
    let mut paths = vec![];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = dir.read_dir() else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() {
                if let Some(path) = path.strip_prefix(root).ok().and_then(Path::to_str) {
                    if !path.contains(['\n', '\r']) {
                        paths.push(path.to_owned());
                    }
                }
            }
        }
    }
    paths
}

pub fn matches(files: &[File], query: &Query) -> Vec<String> {
    let needle = query.prefix.as_str();
    let mut best = Vec::with_capacity(MAX_SUGGESTIONS + 1);
    for file in files {
        let name = file.folded.rsplit('/').next().unwrap_or(&file.folded);
        let rank = if file.folded == needle || name == needle {
            0
        } else if name.starts_with(needle) {
            1
        } else if file.folded.starts_with(needle)
            || file
                .folded
                .match_indices(needle)
                .any(|(i, _)| i > 0 && file.folded.as_bytes()[i - 1] == b'/')
        {
            2
        } else {
            continue;
        };
        let entry = (rank, file.path.len(), &file.path);
        let at = best.partition_point(|&previous| previous < entry);
        if at < MAX_SUGGESTIONS {
            best.insert(at, entry);
            best.truncate(MAX_SUGGESTIONS);
        }
    }
    best.into_iter().map(|(_, _, path)| path.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_filename_at_the_caret_without_touching_a_completed_reference() {
        let text = "Open `@guide.md`, then @src/ma.rs.";
        let caret = text.find(".rs").unwrap();
        let code = 5..16;
        let codes = std::slice::from_ref(&code);
        let candidate = query(text, caret, codes).unwrap();
        assert_eq!(candidate.prefix, "src/ma");
        assert_eq!(&text[candidate.range], "@src/ma.rs");
        assert!(query(text, text.find("guide").unwrap() + 3, codes).is_none());
    }

    #[test]
    fn only_a_standalone_at_prefix_starts_a_mention() {
        for text in [
            "Read main.rs",
            "user@example.org",
            "@@main",
            "Read @/home/no",
        ] {
            assert!(query(text, text.len(), &[]).is_none(), "{text}");
        }
        for (text, prefix) in [("@", ""), ("See @m", "m"), ("See (@./É", "é")] {
            let candidate = query(text, text.len(), &[]).unwrap();
            assert_eq!(candidate.prefix, prefix);
            assert_eq!(candidate.range, text.find('@').unwrap()..text.len());
        }
    }

    #[test]
    fn ranks_exact_names_first_and_distinguishes_duplicate_basenames() {
        let files = [
            "src/main_test.rs",
            "examples/main.rs",
            "src/main.rs",
            "docs/domain.md",
        ]
        .map(|path| File {
            path: path.into(),
            folded: path.into(),
        });
        let q = query("See @MaIn.rs", 12, &[]).unwrap();
        assert_eq!(matches(&files, &q), ["src/main.rs", "examples/main.rs"]);
        let q = query("See @src/ma", 11, &[]).unwrap();
        assert_eq!(matches(&files, &q), ["src/main.rs", "src/main_test.rs"]);
    }

    #[test]
    fn react_icons_are_used_for_both_jsx_and_tsx() {
        assert!(matches!(
            file_icon(Path::new("Panel.tsx")).0,
            assets::Icon::Atom
        ));
        assert!(matches!(
            file_icon(Path::new("Panel.JSX")).0,
            assets::Icon::Atom
        ));
        assert!(matches!(
            file_icon(Path::new("Panel.ts")).0,
            assets::Icon::FileCode
        ));
    }

    #[test]
    fn references_preserve_paths_and_source_ranges() {
        for path in [
            "my folder/main.rs",
            "notes/file`1.md",
            "docs/[draft]#1.md",
            "one:2",
        ] {
            let text = format!("Look at {} now", markup(path));
            let refs = references(&text, None);
            assert_eq!(refs.len(), 1, "{text}");
            assert_eq!(refs[0].source, 8..text.len() - 4);
            assert_eq!(refs[0].path, Path::new(path));
        }
    }
}
