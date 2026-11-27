//! Which rows the conversation shows. Once the agent has replied, what it sent
//! on the way there is folded under one "Worked" row.

pub(crate) mod code;
pub(crate) mod markdown;
pub(crate) mod selection;
pub(crate) mod shell;

use chrono::{DateTime, Datelike, TimeZone};
use kiln_core::checkout::{MOVE_REQUEST, PROJECT_REQUEST};
use kiln_core::title::TITLE_REQUEST;
use kiln_core::{Message, Role};
use std::fmt::Display;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    /// The message at this index.
    Message(usize),
    /// The row that opens and closes the work starting at message `start`.
    /// `reply` is the last message of the reply it led to.
    Work { start: usize, reply: usize },
    /// A contiguous run of tool calls. Its children remain virtualized.
    ToolGroup { start: usize, count: usize },
    GroupedTool(usize),
    /// Live turn status at the beginning of the current response.
    Activity,
    /// Streamed text after the current turn's recorded messages.
    LiveReply,
}

/// The title of a work row: how long the agent ran, in its two largest units.
pub fn worked(ms: u64) -> String {
    if ms == 0 {
        return "Worked".into();
    }
    let secs = (ms / 1000).max(1);
    let (large, small) = match secs {
        0..60 => return format!("Worked for {secs}s"),
        60..3600 => ((secs / 60, "m"), (secs % 60, "s")),
        _ => ((secs / 3600, "h"), (secs % 3600 / 60, "m")),
    };
    match small.0 {
        0 => format!("Worked for {}{}", large.0, large.1),
        _ => format!("Worked for {}{} {}{}", large.0, large.1, small.0, small.1),
    }
}

/// The time of `at` as seen at `now`: the day is named only when it differs,
/// and the year likewise.
pub fn clock<Tz: TimeZone>(at: DateTime<Tz>, now: DateTime<Tz>) -> String
where
    Tz::Offset: Display,
{
    let format = if at.date_naive() == now.date_naive() {
        "%H:%M"
    } else if at.year() == now.year() {
        "%b %-d, %H:%M"
    } else {
        "%b %-d %Y, %H:%M"
    };
    at.format(format).to_string()
}

/// The rows for `messages`. `running` says a turn is in progress, which keeps
/// the latest turn's work in view and adds an activity row at the end. The
/// disclosure callbacks take the first message index of the work or tool group.
pub fn entries(
    messages: &[Message],
    running: bool,
    work_open: impl Fn(usize) -> bool,
    group_open: impl Fn(usize) -> bool,
) -> Vec<Entry> {
    let mut out = Vec::with_capacity(messages.len() + usize::from(running));
    let mut start = 0;
    // What the agent did between one user message and the next.
    for turn in messages.split_inclusive(|m| m.role == Role::User) {
        let end = start + turn.len();
        // The final slice can end at the user message that starts the
        // running turn, while that turn's output is still empty.
        let unfinished = running
            && end == messages.len()
            && turn.last().is_some_and(|m| m.role != Role::User);
        let reply = if unfinished { None } else { reply(turn) };
        let mut shown = start;
        if let Some((first, last)) = reply.filter(|&(first, _)| first > 0) {
            out.push(Entry::Work { start, reply: start + last });
            if !work_open(start) {
                shown = start + first;
                // Generated images remain visible when later tool work is folded.
                out.extend((start..shown).filter(|&i| {
                    messages[i].role == Role::Agent && !messages[i].attachments.is_empty()
                }).map(Entry::Message));
            }
        }
        let mut index = shown;
        for run in messages[shown..end].chunk_by(|a, b| a.tool.is_some() && b.tool.is_some()) {
            let count = run.len();
            if count > 1 {
                out.push(Entry::ToolGroup { start: index, count });
                if group_open(index) {
                    out.extend((index..index + count).map(Entry::GroupedTool));
                }
            } else {
                out.push(Entry::Message(index));
            }
            index += count;
        }
        start = end;
    }
    if running {
        let first = messages.iter().rposition(|m| m.role == Role::User).map_or(0, |i| i + 1);
        let at = out.iter().position(|entry| match *entry {
            Entry::Message(i) | Entry::GroupedTool(i) => i >= first,
            Entry::ToolGroup { start, .. } | Entry::Work { start, .. } => start >= first,
            _ => false,
        }).unwrap_or(out.len());
        out.insert(at, Entry::Activity);
        out.push(Entry::LiveReply);
    }
    out
}

/// The first and last message of the reply that closes `turn`: the last run of
/// agent text, with only notices or the next user message after it. `None` when
/// the turn ended without one.
fn reply(turn: &[Message]) -> Option<(usize, usize)> {
    let is_text = |m: &Message| m.role == Role::Agent && m.tool.is_none();
    let last = turn.iter().rposition(|m| m.role == Role::Agent)?;
    if !is_text(&turn[last]) {
        return None;
    }
    let first = turn[..last].iter().rposition(|m| !is_text(m)).map_or(0, |before| before + 1);
    Some((first, last))
}

/// How many bytes of a message still being written to show: up to its last
/// whole word, and only whole lines once a line may be one Kiln takes out:
/// a request to move the thread, or its title.
pub fn stream_shown(text: &str) -> usize {
    let words = text
        .char_indices()
        .rfind(|(_, c)| c.is_whitespace())
        .map_or(0, |(at, c)| at + c.len_utf8());
    let line = text[..words].rfind('\n').map_or(0, |at| at + 1);
    let current = text[line..].trim_start();
    if [MOVE_REQUEST, PROJECT_REQUEST, TITLE_REQUEST].iter().any(|kiln| current.starts_with(kiln)) {
        line
    } else {
        words
    }
}

#[cfg(test)]
mod tests {
    use super::Entry::{Activity, Message as M};
    use super::*;
    use chrono::Utc;

    fn work(start: usize, reply: usize) -> Entry {
        Entry::Work { start, reply }
    }

    #[test]
    fn a_stream_shows_whole_words_and_holds_back_a_move_request() {
        fn shown(text: &str) -> &str {
            &text[..stream_shown(text)]
        }
        assert_eq!(shown("Hel"), "");
        assert_eq!(shown("Hello wor"), "Hello ");
        assert_eq!(shown("- one\n- tw"), "- one\n- ");
        assert_eq!(shown("a\u{a0}b"), "a\u{a0}");
        assert_eq!(shown("Done.\nKILN_MOVE_TO_WORKTREE {\"name\": "), "Done.\n");
        assert_eq!(shown("Done.\nKILN_MOVE_TO_WORKTREE {}\nok "), "Done.\nKILN_MOVE_TO_WORKTREE {}\nok ");
        assert_eq!(shown("KILN_THREAD_TITLE Fix the lo"), "");
    }

    #[test]
    fn work_time_uses_its_two_largest_units() {
        for (ms, shown) in [
            (0, "Worked"),
            (400, "Worked for 1s"),
            (59_999, "Worked for 59s"),
            (60_000, "Worked for 1m"),
            (125_000, "Worked for 2m 5s"),
            (3_600_000, "Worked for 1h"),
            (3_905_000, "Worked for 1h 5m"),
        ] {
            assert_eq!(worked(ms), shown, "{ms}");
        }
    }

    #[test]
    fn a_reply_time_names_the_day_only_when_it_differs() {
        let at = |y, m, d, h| Utc.with_ymd_and_hms(y, m, d, h, 5, 0).unwrap();
        let now = at(2026, 3, 9, 18);
        assert_eq!(clock(at(2026, 3, 9, 7), now), "07:05");
        assert_eq!(clock(at(2026, 3, 8, 23), now), "Mar 8, 23:05");
        assert_eq!(clock(at(2025, 3, 9, 7), now), "Mar 9 2025, 07:05");
    }

    fn tool() -> Message {
        Message::tool_call("Bash", "$ ls")
    }

    fn turn() -> Vec<Message> {
        vec![
            Message::user("go"),
            Message::agent("Looking."),
            tool(),
            Message::agent("Done."),
        ]
    }

    #[test]
    fn work_before_the_reply_is_folded() {
        assert_eq!(entries(&turn(), false, |_| false, |_| false), [M(0), work(1, 3), M(3)]);
    }

    #[test]
    fn generated_images_stay_visible_when_later_work_is_folded() {
        let mut image = Message::agent("");
        image.attachments.push(kiln_core::Attachment {
            name: "Diagram".into(),
            content: kiln_core::AttachmentContent::Image {
                mime: "image/png".into(),
                base64: "AA==".into(),
            },
        });
        let messages = [Message::user("Draw a diagram"), image, tool(), Message::agent("Done")];
        assert_eq!(
            entries(&messages, false, |_| false, |_| false),
            [Entry::Message(0), work(1, 3), Entry::Message(1), Entry::Message(3)]
        );
    }

    #[test]
    fn opened_work_is_shown_under_its_row() {
        assert_eq!(entries(&turn(), false, |at| at == 1, |_| false), [M(0), work(1, 3), M(1), M(2), M(3)]);
    }

    #[test]
    fn a_running_turn_is_shown_in_full_and_earlier_ones_stay_folded() {
        let mut messages = turn();
        messages.extend(turn());
        assert_eq!(
            entries(&messages[..5], true, |_| false, |_| false),
            [M(0), work(1, 3), M(3), M(4), Activity, Entry::LiveReply]
        );
        assert_eq!(
            entries(&messages[..5], true, |at| at == 1, |_| false),
            [M(0), work(1, 3), M(1), M(2), M(3), M(4), Activity, Entry::LiveReply]
        );
        assert_eq!(
            entries(&messages, true, |_| false, |_| false),
            [M(0), work(1, 3), M(3), M(4), Activity, M(5), M(6), M(7), Entry::LiveReply]
        );
        assert_eq!(
            entries(&messages, false, |_| false, |_| false),
            [M(0), work(1, 3), M(3), M(4), work(5, 7), M(7)]
        );
    }

    #[test]
    fn activity_stays_above_the_live_turn_until_it_ends() {
        let mut messages = vec![Message::user("go")];
        assert_eq!(entries(&messages, true, |_| false, |_| false), [M(0), Activity, Entry::LiveReply]);
        messages.push(Message::agent("Looking."));
        assert_eq!(entries(&messages, true, |_| false, |_| false), [M(0), Activity, M(1), Entry::LiveReply]);
        messages.push(Message::agent("Done."));
        assert_eq!(entries(&messages, true, |_| false, |_| false), [M(0), Activity, M(1), M(2), Entry::LiveReply]);
        assert_eq!(entries(&messages, false, |_| false, |_| false), [M(0), M(1), M(2)]);
    }

    #[test]
    fn consecutive_calls_form_a_group_inside_the_work_disclosure() {
        let messages = [
            Message::user("go"),
            tool(),
            tool(),
            Message::agent("Looking."),
            tool(),
            Message::agent("Done."),
        ];
        let group = Entry::ToolGroup { start: 1, count: 2 };
        assert_eq!(entries(&messages, false, |_| false, |_| true), [M(0), work(1, 5), M(5)]);
        assert_eq!(
            entries(&messages, false, |_| true, |_| false),
            [M(0), work(1, 5), group, M(3), M(4), M(5)],
        );
        assert_eq!(
            entries(&messages, true, |_| false, |start| start == 1),
            [M(0), Activity, group, Entry::GroupedTool(1), Entry::GroupedTool(2), M(3), M(4), M(5), Entry::LiveReply],
        );
        assert_eq!(
            entries(&messages[..3], true, |_| false, |_| false),
            [M(0), Activity, group, Entry::LiveReply],
        );
        assert_eq!(entries(&messages[..2], true, |_| false, |_| false), [M(0), Activity, M(1), Entry::LiveReply]);
    }

    #[test]
    fn a_reply_without_work_has_no_row() {
        let messages = [Message::user("hi"), Message::agent("Hello.")];
        assert_eq!(entries(&messages, false, |_| false, |_| false), [M(0), M(1)]);
    }

    #[test]
    fn a_turn_that_ended_without_a_reply_is_not_folded() {
        let messages = [
            Message::user("go"),
            Message::agent("Looking."),
            tool(),
            Message::notice("Stopped"),
        ];
        assert_eq!(entries(&messages, false, |_| false, |_| false), [M(0), M(1), M(2), M(3)]);
    }

    #[test]
    fn the_whole_reply_and_the_notices_after_it_stay_in_view() {
        let mut messages = turn();
        messages.push(Message::agent("One more thing."));
        messages.push(Message::notice("Blocked by policy"));
        messages.push(Message::user("thanks"));
        assert_eq!(
            entries(&messages, false, |_| false, |_| false),
            [M(0), work(1, 4), M(3), M(4), M(5), M(6)]
        );
    }
}
