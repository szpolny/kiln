//! The text and selection of an editable field, without layout or rendering.
//!
//! Offsets are UTF-8 byte offsets that always fall on character boundaries.
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Default)]
pub struct Buffer {
    text: String,
    selection: Range<usize>,
    /// The caret sits at the start of the selection rather than its end.
    reversed: bool,
    /// Text the input method is still composing.
    marked: Option<Range<usize>>,
}

impl Buffer {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn len(&self) -> usize {
        self.text.len()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn selection(&self) -> Range<usize> {
        self.selection.clone()
    }

    pub fn reversed(&self) -> bool {
        self.reversed
    }

    pub fn marked(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    pub fn selected_text(&self) -> &str {
        &self.text[self.selection.clone()]
    }

    /// Where the caret is: the moving end of the selection.
    pub fn cursor(&self) -> usize {
        if self.reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Collapse the selection to `offset`.
    pub fn move_to(&mut self, offset: usize) {
        let offset = self.floor(offset);
        self.selection = offset..offset;
        self.reversed = false;
    }

    /// Move the caret to `offset`, keeping the other end of the selection.
    pub fn select_to(&mut self, offset: usize) {
        let offset = self.floor(offset);
        let anchor = if self.reversed { self.selection.end } else { self.selection.start };
        self.reversed = offset < anchor;
        self.selection = anchor.min(offset)..anchor.max(offset);
    }

    pub fn select(&mut self, range: Range<usize>) {
        self.selection = self.floor(range.start)..self.floor(range.end);
        self.reversed = false;
    }

    pub fn select_all(&mut self) {
        self.select(0..self.len());
    }

    pub fn unmark(&mut self) {
        self.marked = None;
    }

    /// Replace `range` (else the marked text, else the selection) and put the
    /// caret after the new text.
    pub fn replace(&mut self, range: Option<Range<usize>>, new: &str) {
        let range = self.target(range);
        self.text.replace_range(range.clone(), new);
        self.move_to(range.start + new.len());
        self.marked = None;
    }

    /// Replace like [`Self::replace`], marking the new text as still being
    /// composed. `selected` is relative to the new text.
    pub fn replace_and_mark(
        &mut self,
        range: Option<Range<usize>>,
        new: &str,
        selected: Option<Range<usize>>,
    ) {
        let range = self.target(range);
        self.text.replace_range(range.clone(), new);
        let start = range.start;
        self.marked = (!new.is_empty()).then(|| start..start + new.len());
        match selected {
            Some(s) => self.select(start + s.start.min(new.len())..start + s.end.min(new.len())),
            None => self.move_to(start + new.len()),
        }
    }

    fn target(&self, range: Option<Range<usize>>) -> Range<usize> {
        let r = range.or_else(|| self.marked.clone()).unwrap_or_else(|| self.selection.clone());
        let (a, b) = (self.floor(r.start), self.floor(r.end));
        a.min(b)..a.max(b)
    }

    /// The nearest character boundary at or before `offset`, within the text.
    fn floor(&self, offset: usize) -> usize {
        let mut o = offset.min(self.len());
        while !self.text.is_char_boundary(o) {
            o -= 1;
        }
        o
    }

    pub fn prev_grapheme(&self, offset: usize) -> usize {
        let o = self.floor(offset);
        self.text[..o].grapheme_indices(true).next_back().map_or(0, |(i, _)| i)
    }

    pub fn next_grapheme(&self, offset: usize) -> usize {
        let o = self.floor(offset);
        self.text[o..].graphemes(true).next().map_or(o, |g| o + g.len())
    }

    /// The start of the word before `offset`, skipping whitespace first.
    pub fn prev_word(&self, offset: usize) -> usize {
        let o = self.floor(offset);
        let mut chars = self.text[..o].char_indices().rev().peekable();
        let mut pos = o;
        while let Some(&(i, c)) = chars.peek() {
            if class(c) != Class::Space {
                break;
            }
            pos = i;
            chars.next();
        }
        let Some(&(_, first)) = chars.peek() else { return pos };
        let kind = class(first);
        for (i, c) in chars {
            if class(c) != kind {
                break;
            }
            pos = i;
        }
        pos
    }

    /// The end of the word after `offset`, skipping whitespace first.
    pub fn next_word(&self, offset: usize) -> usize {
        let o = self.floor(offset);
        let mut chars = self.text[o..].char_indices().map(|(i, c)| (o + i, c)).peekable();
        let mut pos = o;
        while let Some(&(i, c)) = chars.peek() {
            if class(c) != Class::Space {
                break;
            }
            pos = i + c.len_utf8();
            chars.next();
        }
        let Some(&(_, first)) = chars.peek() else { return pos };
        let kind = class(first);
        for (i, c) in chars {
            if class(c) != kind {
                break;
            }
            pos = i + c.len_utf8();
        }
        pos
    }

    /// The run of same-kind characters around `offset`, for double-click.
    pub fn word_at(&self, offset: usize) -> Range<usize> {
        let o = self.floor(offset);
        let Some(c) = self.text[o..].chars().next().or_else(|| self.text[..o].chars().next_back())
        else {
            return o..o;
        };
        let kind = class(c);
        let start = self.text[..o]
            .char_indices()
            .rev()
            .take_while(|&(_, c)| class(c) == kind)
            .last()
            .map_or(o, |(i, _)| i);
        let end = self.text[o..]
            .char_indices()
            .take_while(|&(_, c)| class(c) == kind)
            .last()
            .map_or(o, |(i, c)| o + i + c.len_utf8());
        start..end
    }

    /// Start of the paragraph (hard line) containing `offset`.
    pub fn line_start(&self, offset: usize) -> usize {
        let o = self.floor(offset);
        self.text[..o].rfind('\n').map_or(0, |i| i + 1)
    }

    /// End of the paragraph (hard line) containing `offset`, before its newline.
    pub fn line_end(&self, offset: usize) -> usize {
        let o = self.floor(offset);
        self.text[o..].find('\n').map_or(self.len(), |i| o + i)
    }

    pub fn offset_to_utf16(&self, offset: usize) -> usize {
        self.text[..self.floor(offset)].encode_utf16().count()
    }

    pub fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut units = 0;
        for (i, c) in self.text.char_indices() {
            if units >= offset {
                return i;
            }
            units += c.len_utf16();
        }
        self.len()
    }

    pub fn range_to_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(r.start)..self.offset_to_utf16(r.end)
    }

    pub fn range_from_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(r.start)..self.offset_from_utf16(r.end)
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Class {
    Space,
    Word,
    Punct,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(text: &str) -> Buffer {
        let mut b = Buffer::default();
        b.replace(None, text);
        b
    }

    #[test]
    fn typing_replaces_the_selection() {
        let mut b = buffer("hello world");
        b.select(0..5);
        b.replace(None, "goodbye");
        assert_eq!(b.text(), "goodbye world");
        assert_eq!(b.selection(), 7..7);
    }

    #[test]
    fn selecting_past_the_anchor_reverses() {
        let mut b = buffer("abcdef");
        b.move_to(3);
        b.select_to(5);
        assert_eq!((b.selection(), b.reversed()), (3..5, false));
        b.select_to(1);
        assert_eq!((b.selection(), b.reversed(), b.cursor()), (1..3, true, 1));
        b.select_to(4);
        assert_eq!((b.selection(), b.reversed(), b.cursor()), (3..4, false, 4));
    }

    #[test]
    fn graphemes_move_over_whole_clusters() {
        let b = buffer("a👍🏽b");
        let after_emoji = 1 + "👍🏽".len();
        assert_eq!(b.next_grapheme(1), after_emoji);
        assert_eq!(b.prev_grapheme(after_emoji), 1);
        assert_eq!(b.prev_grapheme(0), 0);
        assert_eq!(b.next_grapheme(b.len()), b.len());
    }

    #[test]
    fn words_skip_spaces_then_one_kind_of_character() {
        let b = buffer("foo  bar.baz qux");
        assert_eq!(b.prev_word(5), 0);
        assert_eq!(b.prev_word(8), 5);
        assert_eq!(b.prev_word(9), 8);
        assert_eq!(b.next_word(0), 3);
        assert_eq!(b.next_word(3), 8);
        assert_eq!(b.next_word(12), b.len());
        assert_eq!(b.word_at(6), 5..8);
        assert_eq!(b.word_at(b.len()), 13..16);
    }

    #[test]
    fn lines_end_at_newlines() {
        let b = buffer("one\ntwo\n");
        assert_eq!((b.line_start(5), b.line_end(5)), (4, 7));
        assert_eq!((b.line_start(8), b.line_end(8)), (8, 8));
        assert_eq!((b.line_start(2), b.line_end(0)), (0, 3));
    }

    #[test]
    fn utf16_offsets_round_trip() {
        let b = buffer("aé😀b");
        assert_eq!(b.offset_to_utf16(b.len()), 5);
        assert_eq!(b.offset_from_utf16(2), 3);
        assert_eq!(b.offset_from_utf16(4), 7);
        assert_eq!(b.offset_from_utf16(99), b.len());
        assert_eq!(b.offset_to_utf16(4), 2, "a byte inside a character rounds down");
    }

    #[test]
    fn composition_marks_then_commits() {
        let mut b = buffer("ab");
        b.move_to(1);
        b.replace_and_mark(None, "ni", Some(2..2));
        assert_eq!((b.text(), b.marked(), b.selection()), ("anib", Some(1..3), 3..3));
        b.replace(None, "你");
        assert_eq!((b.text(), b.marked(), b.cursor()), ("a你b", None, 4));
    }
}
