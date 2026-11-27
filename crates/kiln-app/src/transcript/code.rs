//! Bounded syntax coloring for fenced code, cached independently of frame rendering.
use crate::theme::{Theme, MONO};
use gpui::{font, rgb, SharedString, StyledText, TextRun};
use std::{cell::RefCell, collections::VecDeque, sync::OnceLock};
use syntect::{
    easy::HighlightLines, highlighting::ThemeSet, parsing::SyntaxSet, util::LinesWithEndings,
};

const MAX_BYTES: usize = 32 * 1024;
const CACHE_SIZE: usize = 16;
struct Cached {
    language: String,
    text: SharedString,
    dark: bool,
    runs: Vec<TextRun>,
}
thread_local! {
    static CACHE: RefCell<VecDeque<Cached>> = const { RefCell::new(VecDeque::new()) };
}

pub fn render(language: &str, text: SharedString, theme: Theme) -> StyledText {
    let runs = CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(at) = cache.iter().position(|item| {
            item.language == language && item.text == text && item.dark == theme.dark
        }) {
            let item = cache.remove(at).unwrap();
            let runs = item.runs.clone();
            cache.push_back(item);
            return Some(runs);
        }
        let runs = highlights(language, &text, theme.dark)?;
        if cache.len() == CACHE_SIZE {
            cache.pop_front();
        }
        cache.push_back(Cached {
            language: language.to_owned(),
            text: text.clone(),
            dark: theme.dark,
            runs: runs.clone(),
        });
        Some(runs)
    });
    let styled = StyledText::new(text);
    match runs {
        Some(runs) => styled.with_runs(runs),
        None => styled,
    }
}

fn highlights(language: &str, text: &str, dark: bool) -> Option<Vec<TextRun>> {
    if language.is_empty() || text.len() > MAX_BYTES || text.lines().any(|line| line.len() > 4096) {
        return None;
    }
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    static THEMES: OnceLock<ThemeSet> = OnceLock::new();
    let syntaxes = SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines);
    let token = match language.split_whitespace().next()? {
        "shell" | "console" | "bash" | "zsh" => "sh",
        "rust" => "rs",
        "python" => "py",
        "javascript" => "js",
        "typescript" => "ts",
        "yml" => "yaml",
        other => other,
    };
    let syntax = syntaxes.find_syntax_by_token(token)?;
    let themes = THEMES.get_or_init(ThemeSet::load_defaults);
    let palette = themes.themes.get(if dark {
        "base16-ocean.dark"
    } else {
        "InspiredGitHub"
    })?;
    let mut highlighter = HighlightLines::new(syntax, palette);
    let face = font(MONO);
    let mut runs: Vec<TextRun> = Vec::new();
    for line in LinesWithEndings::from(text) {
        for (style, source) in highlighter.highlight_line(line, syntaxes).ok()? {
            let c = style.foreground;
            let color = rgb((u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b)).into();
            if let Some(last) = runs.last_mut().filter(|last| last.color == color) {
                last.len += source.len();
            } else {
                runs.push(TextRun {
                    len: source.len(),
                    font: face.clone(),
                    color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                });
            }
        }
    }
    Some(runs)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coloring_preserves_unicode_and_multiline_source() {
        let text = "fn main() {\n    println!(\"héllo 🦀\"); // comment\n}\n";
        for dark in [false, true] {
            let runs = highlights("rust", text, dark).unwrap();
            let mut offset = 0;
            for run in &runs {
                offset += run.len;
                assert!(text.is_char_boundary(offset));
            }
            assert_eq!(offset, text.len());
            assert!(runs.windows(2).any(|pair| pair[0].color != pair[1].color));
        }
    }
    #[test]
    fn unsupported_and_large_blocks_fall_back_to_plain_text() {
        assert!(highlights("unknown-language", "text", false).is_none());
        assert!(highlights("rs", &"x".repeat(MAX_BYTES + 1), true).is_none());
        assert!(highlights("rs", &"x".repeat(4097), true).is_none());
    }
}
