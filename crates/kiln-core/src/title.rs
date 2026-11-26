//! How the agent names its thread: Kiln asks on the first turn, and the agent
//! answers with a line of its own that Kiln takes out of the reply.

/// What starts the line an agent replies with to name its thread. The title
/// follows it.
pub const TITLE_REQUEST: &str = "KILN_THREAD_TITLE";

/// Longest title kept, in characters.
pub const MAX_TITLE_CHARS: usize = 60;

/// The note put before the first prompt of a thread, asking the agent to name it.
pub fn title_note() -> String {
    format!(
        "<kiln-context>\n\
         This message comes from Kiln, the app running you, not from the user.\n\
         Name this thread for the sidebar: somewhere in your reply, put a line of its own with \
         {TITLE_REQUEST} followed by a short title of the task, for example:\n\
         {TITLE_REQUEST} Fix the login redirect\n\
         Use at most six words, plain text, no quotes and no ending period. Kiln takes the line \
         out of your reply, so do not mention it.\n\
         </kiln-context>"
    )
}

/// Split an agent's reply into the text to show and the title it gave, if
/// any. Only a line of its own counts, and the last title given wins.
pub fn take_title(text: &str) -> (String, Option<String>) {
    let mut title = None;
    let kept: Vec<&str> = text
        .lines()
        .filter(|line| {
            let Some(rest) = line.trim().strip_prefix(TITLE_REQUEST) else { return true };
            if !(rest.is_empty() || rest.starts_with([':', ' '])) {
                return true;
            }
            if let Some(clean) = clean(rest.trim_start_matches(':')) {
                title = Some(clean);
            }
            false
        })
        .collect();
    (kept.join("\n").trim().to_string(), title)
}

/// The title without surrounding quotes, markup, or a final period, cut to
/// [`MAX_TITLE_CHARS`]. `None` when nothing is left.
fn clean(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | '*')).trim();
    let trimmed = trimmed.strip_suffix('.').unwrap_or(trimmed).trim();
    let title: String = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    let title: String = title.chars().take(MAX_TITLE_CHARS).collect();
    let title = title.trim_end().to_string();
    (!title.is_empty()).then_some(title)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_note_shows_the_line_to_reply_with() {
        let note = title_note();
        assert!(note.starts_with("<kiln-context>") && note.ends_with("</kiln-context>"));
        assert!(note.contains("KILN_THREAD_TITLE Fix the login redirect"));
    }

    #[test]
    fn a_title_is_a_line_of_its_own() {
        let (text, title) = take_title("KILN_THREAD_TITLE Add thread titles\nOn it.");
        assert_eq!(text, "On it.");
        assert_eq!(title.as_deref(), Some("Add thread titles"));
        let (text, title) = take_title("I could reply with KILN_THREAD_TITLE x");
        assert_eq!((text.as_str(), title), ("I could reply with KILN_THREAD_TITLE x", None));
        assert_eq!(take_title("KILN_THREAD_TITLES x").1, None);
        let (_, title) = take_title("  KILN_THREAD_TITLE: \"Fix  the build.\"  ");
        assert_eq!(title.as_deref(), Some("Fix the build"));
        let (_, title) = take_title("KILN_THREAD_TITLE one\nKILN_THREAD_TITLE two");
        assert_eq!(title.as_deref(), Some("two"));
    }

    #[test]
    fn an_empty_title_is_dropped_with_its_line() {
        assert_eq!(take_title("KILN_THREAD_TITLE\nDone."), ("Done.".to_string(), None));
        assert_eq!(take_title("KILN_THREAD_TITLE \"\"").1, None);
    }

    #[test]
    fn long_titles_are_cut() {
        let (_, title) = take_title(&format!("KILN_THREAD_TITLE {}", "word ".repeat(40)));
        assert!(title.unwrap().chars().count() <= MAX_TITLE_CHARS);
    }
}
