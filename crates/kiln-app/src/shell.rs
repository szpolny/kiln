//! Syntax coloring for recorded shell calls.

use crate::theme::{Theme, MONO};
use gpui::{font, StyledText, TextRun};
use std::sync::OnceLock;
use tree_sitter_highlight::{Highlight, HighlightConfiguration, HighlightEvent, Highlighter};

const PROMPT: &str = "$ ";
const CAPTURES: [&str; 8] = [
    "function", "keyword", "string", "comment", "property", "operator", "constant", "number",
];
const MAX_HIGHLIGHT_BYTES: usize = 64 * 1024;

pub fn is_shell(name: &str) -> bool {
    matches!(name, "Shell" | "Bash")
}

fn command<'a>(name: &str, input: &'a str, command_bytes: Option<usize>) -> Option<&'a str> {
    let source = input.strip_prefix(PROMPT)?;
    if let Some(len) = command_bytes {
        return source.get(..len);
    }
    // Older Claude calls appended named arguments without storing the boundary.
    if name == "Bash" {
        let end = [
            "\ndescription: ",
            "\ntimeout: ",
            "\nrun_in_background: ",
            "\ndangerouslyDisableSandbox: ",
        ]
        .iter()
        .filter_map(|arg| source.find(arg))
        .min()
        .unwrap_or(source.len());
        return Some(&source[..end]);
    }
    Some(source)
}

pub struct Span {
    len: usize,
    highlight: Option<Highlight>,
}

pub fn highlights(name: &str, input: &str, command_bytes: Option<usize>) -> Option<Vec<Span>> {
    let source = command(name, input, command_bytes)?;
    if source.is_empty() || source.len() > MAX_HIGHLIGHT_BYTES {
        return None;
    }
    static CONFIG: OnceLock<Option<HighlightConfiguration>> = OnceLock::new();
    let config = CONFIG
        .get_or_init(|| {
            let mut config = HighlightConfiguration::new(
                tree_sitter_bash::LANGUAGE.into(),
                "bash",
                tree_sitter_bash::HIGHLIGHT_QUERY,
                "",
                "",
            )
            .ok()?;
            config.configure(&CAPTURES);
            Some(config)
        })
        .as_ref()?;
    let mut highlighter = Highlighter::new();
    let events = highlighter
        .highlight(config, source.as_bytes(), None, |_| None)
        .ok()?;
    let mut spans = vec![Span {
        len: PROMPT.len(),
        highlight: None,
    }];
    let mut stack = Vec::new();
    for event in events {
        match event.ok()? {
            HighlightEvent::HighlightStart(kind) => stack.push(kind),
            HighlightEvent::HighlightEnd => {
                stack.pop();
            }
            HighlightEvent::Source { start, end } => spans.push(Span {
                len: end - start,
                highlight: stack.last().copied(),
            }),
        }
    }
    let tail = input.len() - PROMPT.len() - source.len();
    if tail > 0 {
        spans.push(Span {
            len: tail,
            highlight: None,
        });
    }
    Some(spans)
}

pub fn render(input: &str, spans: &[Span], theme: Theme) -> StyledText {
    let face = font(MONO);
    let runs = spans
        .iter()
        .map(|span| {
            let color = match span.highlight.map(|kind| CAPTURES[kind.0]) {
                Some("function" | "keyword") => theme.accent,
                Some("string") => theme.ok,
                Some("comment" | "operator") => theme.muted,
                Some("property" | "constant" | "number") => theme.warn,
                _ => theme.text,
            };
            TextRun {
                len: span.len,
                font: face.clone(),
                color: color.into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }
        })
        .collect();
    StyledText::new(input.to_string()).with_runs(runs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_highlights_preserve_multiline_unicode_and_leave_tool_arguments_plain() {
        let source = "printf '%s\\n' \"é\" |\n  cat # comment";
        let input = format!("$ {source}\ndescription: Print a value");
        let spans = highlights("Bash", &input, Some(source.len())).unwrap();
        let mut offset = 0;
        let styled: Vec<_> = spans
            .iter()
            .map(|span| {
                let start = offset;
                offset += span.len;
                (
                    &input[start..offset],
                    span.highlight.map(|kind| CAPTURES[kind.0]),
                )
            })
            .collect();
        assert_eq!(offset, input.len());
        assert!(styled.contains(&("printf", Some("function"))));
        assert!(styled.contains(&("\"é\"", Some("string"))));
        assert!(styled.contains(&("# comment", Some("comment"))));
        assert_eq!(styled.last(), Some(&("\ndescription: Print a value", None)));
        assert_eq!(command("Bash", &input, None), Some(source));
    }
}
