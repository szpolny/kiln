//! Markdown for agent replies: parsing into a small block tree, and drawing it.
//!
//! Nothing here touches the network. Images are shown as text, and a link only
//! opens when clicked, with its address always visible next to it.

use crate::theme::{Theme, MONO};
use gpui::{
    div, font, prelude::*, px, relative, AnyElement, ClipboardItem, FontStyle, FontWeight,
    InteractiveText, SharedString, StrikethroughStyle, StyledText, TextRun, UnderlineStyle,
};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    pub link: Option<String>,
}

/// Text plus consecutive styled runs, each given by its length in bytes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Inline {
    pub text: String,
    pub runs: Vec<(usize, Style)>,
}

impl Inline {
    fn push(&mut self, text: &str, style: Style) {
        if text.is_empty() {
            return;
        }
        self.text.push_str(text);
        match self.runs.last_mut() {
            Some((len, last)) if *last == style => *len += text.len(),
            _ => self.runs.push((text.len(), style)),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Paragraph(Inline),
    Heading(u8, Inline),
    Code { language: String, text: String },
    /// `start` is the first number of an ordered list.
    List { start: Option<u64>, items: Vec<Item> },
    Quote(Vec<Block>),
    /// The first row is the header.
    Table(Vec<Vec<Inline>>),
    Rule,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// Set for task list items: whether the box is ticked.
    pub done: Option<bool>,
    pub blocks: Vec<Block>,
}

/// A container that is still open while parsing.
enum Frame {
    Root,
    Quote,
    List { start: Option<u64>, items: Vec<Item> },
    Item { done: Option<bool> },
}

#[derive(Default)]
struct Builder {
    /// Open containers, each with the blocks collected for it so far.
    stack: Vec<(Frame, Vec<Block>)>,
    inline: Inline,
    style: Style,
    code: Option<(String, String)>,
    table: Vec<Vec<Inline>>,
    /// Where the open link's text starts.
    link_start: usize,
    /// Address of the open image. Kept apart so a linked image keeps its link.
    image: Option<String>,
}

impl Builder {
    fn add(&mut self, block: Block) {
        if let Some((_, blocks)) = self.stack.last_mut() {
            blocks.push(block);
        }
    }

    /// Close any loose text as a paragraph. Tight list items have no
    /// paragraph events of their own.
    fn flush(&mut self) {
        if !self.inline.text.is_empty() {
            let inline = std::mem::take(&mut self.inline);
            self.add(Block::Paragraph(inline));
        }
    }

    fn open(&mut self, frame: Frame) {
        self.flush();
        self.stack.push((frame, vec![]));
    }

    fn close(&mut self) {
        self.flush();
        if self.stack.len() < 2 {
            return;
        }
        let Some((frame, blocks)) = self.stack.pop() else { return };
        match frame {
            Frame::Root => {}
            Frame::Quote => self.add(Block::Quote(blocks)),
            Frame::List { start, items } => self.add(Block::List { start, items }),
            Frame::Item { done } => {
                if let Some((Frame::List { items, .. }, _)) = self.stack.last_mut() {
                    items.push(Item { done, blocks });
                }
            }
        }
    }

    fn text(&mut self, text: &str) {
        match &mut self.code {
            Some((_, code)) => code.push_str(text),
            None => self.inline.push(text, self.style.clone()),
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph | Tag::Heading { .. } | Tag::HtmlBlock => self.flush(),
            Tag::BlockQuote(_) => self.open(Frame::Quote),
            Tag::CodeBlock(kind) => {
                self.flush();
                let language = match kind {
                    CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().unwrap_or_default().to_string()
                    }
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((language, String::new()));
            }
            Tag::List(start) => self.open(Frame::List { start, items: vec![] }),
            Tag::Item => self.open(Frame::Item { done: None }),
            Tag::Table(_) => {
                self.flush();
                self.table.clear();
            }
            Tag::TableHead | Tag::TableRow => self.table.push(vec![]),
            Tag::Emphasis => self.style.italic = true,
            Tag::Strong => self.style.bold = true,
            Tag::Strikethrough => self.style.strike = true,
            Tag::Link { dest_url, .. } => {
                self.link_start = self.inline.text.len();
                self.style.link = Some(dest_url.into_string());
            }
            Tag::Image { dest_url, .. } => {
                self.text("[image: ");
                self.image = Some(dest_url.into_string());
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::HtmlBlock => self.flush(),
            TagEnd::Heading(level) => {
                let inline = std::mem::take(&mut self.inline);
                self.add(Block::Heading(heading_depth(level), inline));
            }
            TagEnd::BlockQuote(_) | TagEnd::List(_) | TagEnd::Item => self.close(),
            TagEnd::CodeBlock => {
                if let Some((language, mut text)) = self.code.take() {
                    if text.ends_with('\n') {
                        text.pop();
                    }
                    self.add(Block::Code { language, text });
                }
            }
            TagEnd::TableCell => {
                let cell = std::mem::take(&mut self.inline);
                if let Some(row) = self.table.last_mut() {
                    row.push(cell);
                }
            }
            TagEnd::Table => {
                let rows = std::mem::take(&mut self.table);
                self.add(Block::Table(rows));
            }
            TagEnd::Emphasis => self.style.italic = false,
            TagEnd::Strong => self.style.bold = false,
            TagEnd::Strikethrough => self.style.strike = false,
            TagEnd::Link => {
                if let Some(url) = self.style.link.take() {
                    let shown = &self.inline.text[self.link_start..];
                    // Never hide where a link goes behind other words.
                    if shown != url && format!("mailto:{shown}") != url {
                        self.text(" (");
                        let style = Style { link: Some(url.clone()), ..self.style.clone() };
                        self.inline.push(&url, style);
                        self.text(")");
                    }
                }
            }
            TagEnd::Image => {
                if let Some(url) = self.image.take() {
                    self.text(&format!("] ({url})"));
                }
            }
            _ => {}
        }
    }
}

fn heading_depth(level: HeadingLevel) -> u8 {
    level as u8
}

pub fn parse(source: &str) -> Vec<Block> {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut b = Builder { stack: vec![(Frame::Root, vec![])], ..Builder::default() };
    for event in Parser::new_ext(source, options) {
        match event {
            Event::Start(tag) => b.start(tag),
            Event::End(tag) => b.end(tag),
            // HTML is never interpreted; it is shown as written.
            Event::Text(t) | Event::Html(t) | Event::InlineHtml(t) => b.text(&t),
            Event::Code(t) => {
                let style = Style { code: true, ..b.style.clone() };
                b.inline.push(&t, style);
            }
            Event::SoftBreak | Event::HardBreak => b.text("\n"),
            Event::Rule => {
                b.flush();
                b.add(Block::Rule);
            }
            Event::TaskListMarker(ticked) => {
                if let Some((Frame::Item { done }, _)) = b.stack.last_mut() {
                    *done = Some(ticked);
                }
            }
            _ => {}
        }
    }
    b.flush();
    b.stack.pop().map(|(_, blocks)| blocks).unwrap_or_default()
}

/// Only web and mail addresses open on click; anything else is just text.
fn opens(url: &str) -> bool {
    ["http://", "https://", "mailto:"].iter().any(|scheme| url.starts_with(scheme))
}

/// Draw parsed blocks. `key` must be unique among messages on screen.
pub fn render(blocks: &[Block], theme: Theme, key: usize) -> impl IntoElement {
    Painter { theme, key, next_id: 0 }.column(blocks)
}

struct Painter {
    theme: Theme,
    key: usize,
    next_id: usize,
}

impl Painter {
    fn id(&mut self, kind: &str) -> SharedString {
        self.next_id += 1;
        format!("md-{kind}-{}-{}", self.key, self.next_id).into()
    }

    fn column(&mut self, blocks: &[Block]) -> gpui::Div {
        let mut column = div().flex().flex_col().w_full().min_w_0().gap_2p5();
        for block in blocks {
            column = column.child(self.block(block));
        }
        column
    }

    fn block(&mut self, block: &Block) -> AnyElement {
        let theme = self.theme;
        match block {
            Block::Paragraph(inline) => {
                div().w_full().min_w_0().child(self.inline(inline, false)).into_any_element()
            }
            Block::Heading(depth, inline) => {
                let heading = div().w_full().min_w_0().pt_1();
                match depth {
                    1 => heading.text_xl(),
                    2 => heading.text_lg(),
                    _ => heading.text_base(),
                }
                .child(self.inline(inline, true))
                .into_any_element()
            }
            Block::Code { language, text } => self.code(language, text).into_any_element(),
            Block::List { start, items } => {
                let mut list = div().flex().flex_col().w_full().min_w_0().gap_1();
                // One marker width per list, wide enough for its last number.
                let last = start.map_or(0, |first| first + items.len() as u64);
                let marker_width = px(12. + 9. * last.to_string().len() as f32);
                for (i, item) in items.iter().enumerate() {
                    let marker = match (item.done, start) {
                        (Some(true), _) => "☑".to_string(),
                        (Some(false), _) => "☐".to_string(),
                        (None, Some(first)) => format!("{}.", first + i as u64),
                        (None, None) => "•".to_string(),
                    };
                    list = list.child(
                        div().flex().gap_2().w_full().min_w_0()
                            .child(div().flex().flex_shrink_0().justify_end().w(marker_width)
                                .text_color(theme.muted).child(marker))
                            .child(self.column(&item.blocks).flex_1().gap_1()),
                    );
                }
                list.into_any_element()
            }
            Block::Quote(blocks) => self
                .column(blocks)
                .pl_3()
                .border_l_2()
                .border_color(theme.border)
                .text_color(theme.muted)
                .into_any_element(),
            Block::Table(rows) => {
                let mut table = div().flex().flex_col().w_full().rounded(px(10.))
                    .border_1().border_color(theme.border);
                for (r, cells) in rows.iter().enumerate() {
                    let mut row = div().flex().w_full()
                        .when(r == 0, |d| d.bg(theme.fill))
                        .when(r > 0, |d| d.border_t_1().border_color(theme.border));
                    for cell in cells {
                        row = row.child(
                            div().flex_1().min_w_0().px_2p5().py_1()
                                .child(self.inline(cell, r == 0)),
                        );
                    }
                    table = table.child(row);
                }
                table.into_any_element()
            }
            Block::Rule => div().w_full().h(px(1.)).my_1().bg(theme.border).into_any_element(),
        }
    }

    fn code(&mut self, language: &str, text: &str) -> impl IntoElement {
        let theme = self.theme;
        let copy = SharedString::from(text.to_string());
        let language = if language.is_empty() { "code" } else { language };
        div().flex().flex_col().w_full().min_w_0().rounded(px(10.)).bg(theme.fill)
            .border_1().border_color(theme.border)
            .child(
                div().flex().items_center().px_3().py_1().text_xs().text_color(theme.muted)
                    .border_b_1().border_color(theme.border)
                    .child(div().flex_1().child(language.to_string()))
                    .child(
                        div().id(self.id("copy")).px_1p5().rounded_md().cursor_pointer()
                            .hover(|h| h.bg(theme.hover).text_color(theme.text))
                            .active(|a| a.bg(theme.selected))
                            .child("Copy")
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy.to_string()))
                            }),
                    ),
            )
            .child(
                div().px_3().py_2().font_family(MONO).text_xs().line_height(relative(1.6))
                    .text_color(theme.text).child(text.to_string()),
            )
    }

    fn inline(&mut self, inline: &Inline, heading: bool) -> AnyElement {
        let theme = self.theme;
        let mut runs = Vec::with_capacity(inline.runs.len());
        let (mut ranges, mut urls) = (vec![], vec![]);
        let mut at = 0;
        for (len, style) in &inline.runs {
            let mut face = font(if style.code { MONO } else { theme.font });
            if style.bold || heading {
                face.weight = FontWeight::SEMIBOLD;
            }
            if style.italic {
                face.style = FontStyle::Italic;
            }
            let link = style.link.as_ref().filter(|url| opens(url));
            let color = if link.is_some() { theme.accent } else { theme.text };
            if let Some(url) = link {
                ranges.push(at..at + len);
                urls.push(url.clone());
            }
            runs.push(TextRun {
                len: *len,
                font: face,
                color: color.into(),
                background_color: style.code.then(|| theme.selected.into()),
                underline: link.map(|_| UnderlineStyle { thickness: px(1.), ..Default::default() }),
                strikethrough: style
                    .strike
                    .then(|| StrikethroughStyle { thickness: px(1.), ..Default::default() }),
            });
            at += len;
        }
        let text = StyledText::new(inline.text.clone()).with_runs(runs);
        if urls.is_empty() {
            return text.into_any_element();
        }
        InteractiveText::new(self.id("link"), text)
            .on_click(ranges, move |i, _, cx| cx.open_url(&urls[i]))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> Inline {
        let mut inline = Inline::default();
        inline.push(text, Style::default());
        inline
    }

    #[test]
    fn inline_styles_become_runs_that_cover_the_text() {
        let blocks = parse("a **bold** *it* `co de` ~~no~~ é");
        let [Block::Paragraph(p)] = blocks.as_slice() else { panic!("{blocks:?}") };
        assert_eq!(p.text, "a bold it co de no é");
        assert_eq!(p.runs.iter().map(|(len, _)| len).sum::<usize>(), p.text.len());
        let styled = |wanted: fn(&Style) -> bool| {
            let mut at = 0;
            let mut found = vec![];
            for (len, style) in &p.runs {
                if wanted(style) {
                    found.push(&p.text[at..at + len]);
                }
                at += len;
            }
            found
        };
        assert_eq!(styled(|s| s.bold), ["bold"]);
        assert_eq!(styled(|s| s.italic), ["it"]);
        assert_eq!(styled(|s| s.code), ["co de"]);
        assert_eq!(styled(|s| s.strike), ["no"]);
    }

    #[test]
    fn headings_code_and_rules() {
        let blocks = parse("## Title\n\n```rust\nfn a() {}\n\n```\n\n---\n");
        assert_eq!(
            blocks,
            vec![
                Block::Heading(2, plain("Title")),
                Block::Code { language: "rust".into(), text: "fn a() {}\n".into() },
                Block::Rule,
            ]
        );
    }

    #[test]
    fn an_unclosed_fence_still_shows_its_code() {
        assert_eq!(
            parse("```\nhalf"),
            vec![Block::Code { language: String::new(), text: "half".into() }]
        );
    }

    #[test]
    fn nested_and_task_lists() {
        let blocks = parse("3. one\n   - [x] sub\n4. two");
        let sub = Block::List {
            start: None,
            items: vec![Item { done: Some(true), blocks: vec![Block::Paragraph(plain("sub"))] }],
        };
        assert_eq!(
            blocks,
            vec![Block::List {
                start: Some(3),
                items: vec![
                    Item { done: None, blocks: vec![Block::Paragraph(plain("one")), sub] },
                    Item { done: None, blocks: vec![Block::Paragraph(plain("two"))] },
                ],
            }]
        );
    }

    #[test]
    fn quotes_and_tables() {
        assert_eq!(
            parse("> said"),
            vec![Block::Quote(vec![Block::Paragraph(plain("said"))])]
        );
        assert_eq!(
            parse("| a | b |\n|---|---|\n| 1 | 2 |"),
            vec![Block::Table(vec![vec![plain("a"), plain("b")], vec![plain("1"), plain("2")]])]
        );
    }

    #[test]
    fn link_addresses_stay_visible_and_images_are_not_loaded() {
        let blocks = parse("[docs](https://example.com/x) ![cat](https://example.com/c.png)");
        let [Block::Paragraph(p)] = blocks.as_slice() else { panic!("{blocks:?}") };
        assert_eq!(
            p.text,
            "docs (https://example.com/x) [image: cat] (https://example.com/c.png)"
        );
        let bare = parse("<https://example.com>");
        let [Block::Paragraph(p)] = bare.as_slice() else { panic!("{bare:?}") };
        assert_eq!(p.text, "https://example.com");
    }

    #[test]
    fn a_linked_image_keeps_both_addresses() {
        let blocks = parse("[![badge](https://a.test/b.png)](https://a.test/page)");
        let [Block::Paragraph(p)] = blocks.as_slice() else { panic!("{blocks:?}") };
        assert_eq!(p.text, "[image: badge] (https://a.test/b.png) (https://a.test/page)");
    }

    #[test]
    fn only_web_and_mail_links_open() {
        assert!(opens("https://example.com"));
        assert!(opens("mailto:a@example.com"));
        assert!(!opens("file:///etc/passwd"));
        assert!(!opens("javascript:alert(1)"));
    }

    #[test]
    fn html_is_shown_as_written() {
        let blocks = parse("a <b>x</b>");
        let [Block::Paragraph(p)] = blocks.as_slice() else { panic!("{blocks:?}") };
        assert_eq!(p.text, "a <b>x</b>");
    }
}
