//! A multi-line text field with a blinking caret, keyboard and mouse
//! selection, the clipboard, and input method composition.

mod buffer;

use self::buffer::Buffer;
use crate::file_reference::{self, ChipColors, Reference};
use crate::spelling::Dictionaries;
use crate::theme::Theme;
use gpui::{
    actions, anchored, deferred, div, fill, point, prelude::*, px, relative, rgb, size, Anchor,
    App, AvailableSpace, Bounds, Div,
    ClipboardItem, Context, CursorStyle, DispatchPhase, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, Hsla, KeyBinding,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    RenderImage, ScrollHandle, SharedString, Style, Subscription, Task, TextAlign, TextRun, UTF16Selection,
    UnderlineStyle, Window, WrappedLine,
};
use std::{ops::Range, path::{Path, PathBuf}};
use std::sync::Arc;
use std::time::Duration;

actions!(
    text_input,
    [
        Backspace,
        Delete,
        DeleteWordLeft,
        DeleteWordRight,
        DeleteToLineStart,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        Home,
        End,
        SelectHome,
        SelectEnd,
        DocStart,
        DocEnd,
        SelectDocStart,
        SelectDocEnd,
        SelectAll,
        Copy,
        Cut,
        Paste,
        Newline,
        Submit,
        SubmitAlternate,
    ]
);

const CONTEXT: &str = "TextInput";
const BLINK: Duration = Duration::from_millis(530);
/// Taller drafts scroll inside the field.
const MAX_HEIGHT: f32 = 240.;

/// Platform-standard editing shortcuts for every text field.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys(key_bindings(cfg!(target_os = "macos")));
}

/// The shortcuts on macOS (`mac`) or elsewhere.
fn key_bindings(mac: bool) -> Vec<KeyBinding> {
    let c = Some(CONTEXT);
    let mut keys = vec![
        KeyBinding::new("backspace", Backspace, c),
        KeyBinding::new("shift-backspace", Backspace, c),
        KeyBinding::new("delete", Delete, c),
        KeyBinding::new("left", Left, c),
        KeyBinding::new("right", Right, c),
        KeyBinding::new("up", Up, c),
        KeyBinding::new("down", Down, c),
        KeyBinding::new("shift-left", SelectLeft, c),
        KeyBinding::new("shift-right", SelectRight, c),
        KeyBinding::new("shift-up", SelectUp, c),
        KeyBinding::new("shift-down", SelectDown, c),
        KeyBinding::new("home", Home, c),
        KeyBinding::new("end", End, c),
        KeyBinding::new("shift-home", SelectHome, c),
        KeyBinding::new("shift-end", SelectEnd, c),
        KeyBinding::new("enter", Submit, c),
        KeyBinding::new("ctrl-enter", SubmitAlternate, c),
        KeyBinding::new("shift-enter", Newline, c),
    ];
    if mac {
        keys.extend([
            KeyBinding::new("cmd-a", SelectAll, c),
            KeyBinding::new("cmd-c", Copy, c),
            KeyBinding::new("cmd-x", Cut, c),
            KeyBinding::new("cmd-v", Paste, c),
            KeyBinding::new("alt-left", WordLeft, c),
            KeyBinding::new("alt-right", WordRight, c),
            KeyBinding::new("alt-shift-left", SelectWordLeft, c),
            KeyBinding::new("alt-shift-right", SelectWordRight, c),
            KeyBinding::new("cmd-left", Home, c),
            KeyBinding::new("cmd-right", End, c),
            KeyBinding::new("cmd-shift-left", SelectHome, c),
            KeyBinding::new("cmd-shift-right", SelectEnd, c),
            KeyBinding::new("ctrl-a", Home, c),
            KeyBinding::new("ctrl-e", End, c),
            KeyBinding::new("cmd-up", DocStart, c),
            KeyBinding::new("cmd-down", DocEnd, c),
            KeyBinding::new("cmd-shift-up", SelectDocStart, c),
            KeyBinding::new("cmd-shift-down", SelectDocEnd, c),
            KeyBinding::new("alt-backspace", DeleteWordLeft, c),
            KeyBinding::new("alt-delete", DeleteWordRight, c),
            KeyBinding::new("cmd-backspace", DeleteToLineStart, c),
        ]);
    } else {
        keys.extend([
            KeyBinding::new("ctrl-a", SelectAll, c),
            KeyBinding::new("ctrl-c", Copy, c),
            KeyBinding::new("ctrl-x", Cut, c),
            KeyBinding::new("ctrl-v", Paste, c),
            KeyBinding::new("ctrl-left", WordLeft, c),
            KeyBinding::new("ctrl-right", WordRight, c),
            KeyBinding::new("ctrl-shift-left", SelectWordLeft, c),
            KeyBinding::new("ctrl-shift-right", SelectWordRight, c),
            KeyBinding::new("ctrl-home", DocStart, c),
            KeyBinding::new("ctrl-end", DocEnd, c),
            KeyBinding::new("ctrl-shift-home", SelectDocStart, c),
            KeyBinding::new("ctrl-shift-end", SelectDocEnd, c),
            KeyBinding::new("ctrl-backspace", DeleteWordLeft, c),
            KeyBinding::new("ctrl-delete", DeleteWordRight, c),
            KeyBinding::new("shift-insert", Paste, c),
            KeyBinding::new("ctrl-insert", Copy, c),
            KeyBinding::new("shift-delete", Cut, c),
        ]);
    }
    keys
}

pub enum InputEvent {
    /// The text changed.
    Changed,
    CaretMoved,
    /// Enter was pressed.
    Submit,
    /// Ctrl+Enter was pressed.
    SubmitAlternate,
    Images(std::sync::Arc<[gpui::Image]>),
}

#[derive(Clone, Copy, Default, PartialEq)]
pub struct Colors {
    pub text: Hsla,
    pub placeholder: Hsla,
    pub caret: Hsla,
    pub selection: Hsla,
    pub reference: ChipColors,
}

pub struct TextInput {
    focus: FocusHandle,
    buffer: Buffer,
    pub paste_images: bool,
    pub file_references: bool,
    pub spelling_theme: Option<Theme>,
    dictionaries: Dictionaries,
    misspellings: Vec<Range<usize>>,
    spelling_check: Option<Task<()>>,
    spelling_menu: Option<SpellingMenu>,
    references: Vec<Reference>,
    checkout: Option<PathBuf>,
    scratch: Option<PathBuf>,
    placeholder: SharedString,
    pub colors: Colors,
    /// Geometry from the last paint, for hit testing and vertical movement.
    layout: Option<Layout>,
    selecting: bool,
    /// Horizontal position kept while moving up and down through lines.
    goal_x: Option<Pixels>,
    caret_visible: bool,
    /// Toggles the caret while focused; dropping it stops the blinking.
    blink: Option<Task<()>>,
    scroll: ScrollHandle,
    /// Scroll the caret into view on the next paint.
    reveal: bool,
    _focus_subs: [Subscription; 2],
}

struct SpellingMenu {
    range: Range<usize>,
    position: Point<Pixels>,
    suggestions: Option<Vec<String>>,
    _task: Task<()>,
}

impl EventEmitter<InputEvent> for TextInput {}

impl TextInput {
    pub fn new(placeholder: impl Into<SharedString>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let subs = [
            cx.on_focus(&focus, window, |this, _, cx| this.show_caret(cx)),
            cx.on_blur(&focus, window, |this, _, cx| {
                this.blink = None;
                this.selecting = false;
                this.spelling_menu = None;
                cx.notify();
            }),
        ];
        Self {
            focus,
            buffer: Buffer::default(),
            paste_images: false,
            file_references: false,
            spelling_theme: None,
            dictionaries: Dictionaries::default(),
            misspellings: vec![],
            spelling_check: None,
            spelling_menu: None,
            references: vec![],
            checkout: None,
            scratch: None,
            placeholder: placeholder.into(),
            colors: Colors::default(),
            layout: None,
            selecting: false,
            goal_x: None,
            caret_visible: true,
            blink: None,
            scroll: ScrollHandle::new(),
            reveal: false,
            _focus_subs: subs,
        }
    }

    pub fn text(&self) -> &str {
        self.buffer.text()
    }

    pub fn reference_query(&self) -> Option<crate::file_reference::Query> {
        if self.buffer.selection().is_empty() && self.buffer.marked().is_none() {
            let ranges = self.references.iter().map(|reference| reference.source.clone()).collect::<Vec<_>>();
            file_reference::query(self.text(), self.buffer.cursor(), &ranges)
        } else {
            None
        }
    }

    pub fn replace_reference(&mut self, query: crate::file_reference::Query, path: &str, cx: &mut Context<Self>) {
        let mut text = crate::file_reference::markup(path);
        if query.range.end == self.text().len() {
            text.push(' ');
        }
        self.buffer.replace(Some(query.range), &text);
        self.edited(cx);
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.buffer.clear();
        self.edited(cx);
    }

    /// Show `text` with the caret at its end. This is not an edit, so it emits
    /// no [`InputEvent::Changed`].
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.buffer.clear();
        self.buffer.replace(None, text);
        self.update_references();
        self.check_spelling(cx);
        self.moved(cx);
    }

    /// Show the caret solidly now, then resume blinking.
    fn show_caret(&mut self, cx: &mut Context<Self>) {
        self.caret_visible = true;
        self.reveal = true;
        self.blink = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(BLINK).await;
            let toggled = this.update(cx, |this, cx| {
                this.caret_visible = !this.caret_visible;
                cx.notify();
            });
            if toggled.is_err() {
                break;
            }
        }));
        cx.notify();
    }

    fn moved(&mut self, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.show_caret(cx);
        if self.file_references {
            cx.emit(InputEvent::CaretMoved);
        }
    }

    pub fn set_reference_paths(&mut self, checkout: Option<PathBuf>, scratch: Option<PathBuf>) {
        if self.checkout != checkout {
            self.checkout = checkout;
            self.update_references();
        }
        self.scratch = scratch;
    }

    fn reference_picture(&self, path: &Path) -> Option<crate::media::Picture> {
        file_reference::picture(self.checkout.as_deref(), self.scratch.as_deref()?, path)
    }

    fn update_references(&mut self) {
        if self.file_references {
            self.references = file_reference::references(self.text(), self.checkout.as_deref());
        }
    }

    fn edited(&mut self, cx: &mut Context<Self>) {
        self.update_references();
        self.check_spelling(cx);
        self.moved(cx);
        cx.emit(InputEvent::Changed);
    }

    pub fn set_dictionaries(&mut self, dictionaries: Dictionaries, cx: &mut Context<Self>) {
        self.dictionaries = dictionaries;
        self.check_spelling(cx);
        cx.notify();
    }

    fn check_spelling(&mut self, cx: &mut Context<Self>) {
        self.spelling_menu = None;
        self.misspellings.clear();
        self.spelling_check = None;
        if self.dictionaries.is_empty() || self.buffer.is_empty() {
            return;
        }

        let text = self.text().to_owned();
        let dictionaries = self.dictionaries.clone();
        self.spelling_check = Some(cx.spawn(async move |this, cx| {
            let executor = cx.background_executor();
            executor.timer(Duration::from_millis(120)).await;
            let misspellings = executor.spawn(async move { dictionaries.misspellings(&text) }).await;
            let _ = this.update(cx, |this, cx| {
                this.misspellings = misspellings;
                cx.notify();
            });
        }));
    }

    fn open_spelling_menu(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.buffer.marked().is_some() {
            return;
        }
        let Some(at) = self.layout.as_ref().and_then(|layout| layout.character_at(ev.position)) else {
            return;
        };
        let Some(range) = self.misspellings.iter().find(|range| range.contains(&at)).cloned() else {
            return;
        };
        let word = self.text()[range.clone()].to_owned();
        let dictionaries = self.dictionaries.clone();
        let task = cx.spawn(async move |this, cx| {
            let suggestions = cx.background_executor().spawn(async move { dictionaries.suggestions(&word) }).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(menu) = &mut this.spelling_menu {
                    menu.suggestions = Some(suggestions);
                    cx.notify();
                }
            });
        });
        window.focus(&self.focus, cx);
        self.spelling_menu = Some(SpellingMenu {
            range,
            position: ev.position,
            suggestions: None,
            _task: task,
        });
        cx.stop_propagation();
        cx.notify();
    }

    fn close_spelling_menu(&mut self, cx: &mut Context<Self>) {
        if self.spelling_menu.take().is_some() {
            cx.notify();
        }
    }

    fn spelling_menu(&self, cx: &mut Context<Self>) -> Option<Div> {
        let theme = self.spelling_theme?;
        let menu = self.spelling_menu.as_ref()?;
        let mut panel = div().id("spelling-menu").flex().flex_col().w(px(180.)).p_1()
            .rounded(px(10.)).bg(theme.card).border_1().border_color(theme.border).shadow_lg()
            .occlude().cursor(CursorStyle::Arrow)
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_spelling_menu(cx)));
        match menu.suggestions.as_deref() {
            Some(suggestions) if !suggestions.is_empty() => {
                panel = panel.children(suggestions.iter().enumerate().map(|(index, word)| {
                    div().id(("spelling-suggestion", index)).px_2().py_1().rounded_md().text_sm()
                        .text_color(theme.text).cursor_pointer().hover(|h| h.bg(theme.hover))
                        .child(word.clone())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let Some(menu) = this.spelling_menu.take() else { return };
                            if let Some(word) = menu.suggestions.as_ref().and_then(|words| words.get(index)) {
                                this.buffer.replace(Some(menu.range), word);
                                this.edited(cx);
                            }
                        }))
                }));
            }
            suggestions => {
                let label = if suggestions.is_some() { "No spelling suggestions" } else { "Checking spelling…" };
                panel = panel.child(div().px_2().py_1().text_sm().text_color(theme.muted).child(label));
            }
        }
        Some(div().absolute().child(
            deferred(
                anchored().anchor(Anchor::TopLeft).position(menu.position).child(panel),
            ).with_priority(2),
        ))
    }

    fn chip_edge(&self, offset: usize) -> usize {
        let cursor = self.buffer.cursor();
        self.references.iter().find(|r| r.source.start < offset && offset < r.source.end)
            .map_or(offset, |r| if offset < cursor { r.source.start } else { r.source.end })
    }

    fn whole_chips(&self, mut range: Range<usize>) -> Range<usize> {
        for reference in &self.references {
            if reference.source.start < range.end && range.start < reference.source.end {
                range.start = range.start.min(reference.source.start);
                range.end = range.end.max(reference.source.end);
            }
        }
        range
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.buffer.move_to(self.chip_edge(offset));
        self.moved(cx);
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.buffer.select_to(self.chip_edge(offset));
        self.moved(cx);
    }

    /// Collapse a selection toward one side, or step the caret.
    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let sel = self.buffer.selection();
        let target = match (sel.is_empty(), forward) {
            (false, false) => sel.start,
            (false, true) => sel.end,
            (true, false) => self.buffer.prev_grapheme(sel.start),
            (true, true) => self.buffer.next_grapheme(sel.end),
        };
        self.move_to(target, cx);
    }

    /// Delete the selection, or from the caret to `to` when nothing is selected.
    fn delete_to(&mut self, to: impl Fn(&Buffer, usize) -> usize, cx: &mut Context<Self>) {
        if self.buffer.selection().is_empty() {
            let cursor = self.buffer.cursor();
            self.buffer.select_to(self.chip_edge(to(&self.buffer, cursor)));
        }
        if !self.buffer.selection().is_empty() {
            self.buffer.replace(None, "");
            self.edited(cx);
        }
    }

    fn insert(&mut self, text: &str, cx: &mut Context<Self>) {
        self.buffer.replace(None, text);
        self.edited(cx);
    }

    /// Start or end of the visual row holding the caret, falling back to the paragraph.
    fn row_edge(&self, end: bool) -> usize {
        let cursor = self.buffer.cursor();
        match &self.layout {
            Some(l) if !self.buffer.is_empty() => {
                let p = l.position(cursor);
                let x = if end { px(1e6) } else { px(-1.) };
                l.index(point(l.origin.x + x, p.y + l.line_height / 2.))
            }
            _ if end => self.buffer.line_end(cursor),
            _ => self.buffer.line_start(cursor),
        }
    }

    /// The offset one row above or below the caret, keeping the goal column.
    fn vertical(&mut self, down: bool, select: bool, cx: &mut Context<Self>) {
        let sel = self.buffer.selection();
        let from = match (select, down) {
            (true, _) => self.buffer.cursor(),
            (false, false) => sel.start,
            (false, true) => sel.end,
        };
        let target = match &self.layout {
            Some(l) if !self.buffer.is_empty() => {
                let p = l.position(from);
                let x = *self.goal_x.get_or_insert(p.x);
                let y = p.y + l.line_height * if down { 1.5 } else { -0.5 };
                if y < l.origin.y {
                    0
                } else if y >= l.origin.y + l.height {
                    self.buffer.len()
                } else {
                    l.index(point(x, y))
                }
            }
            _ if down => self.buffer.len(),
            _ => 0,
        };
        let goal = self.goal_x;
        if select {
            self.select_to(target, cx);
        } else {
            self.move_to(target, cx);
        }
        self.goal_x = goal;
    }

    fn index_for(&self, position: Point<Pixels>) -> usize {
        match &self.layout {
            Some(l) if !self.buffer.is_empty() => l.index(position),
            _ => 0,
        }
    }

    fn on_mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if cfg!(target_os = "macos") && ev.modifiers.control && self.spelling_theme.is_some() {
            self.open_spelling_menu(ev, window, cx);
            return;
        }
        window.focus(&self.focus, cx);
        if ev.click_count == 1 && !ev.modifiers.shift {
            if let Some(layout) = &self.layout {
                if let Some((i, _)) = layout.chips.iter().enumerate().find(|(_, chip)| chip.bounds.contains(&ev.position)) {
                    if let Some(reference) = layout.spans.get(i).and_then(|span| {
                        self.references.iter().find(|reference| reference.source == span.source)
                    }) {
                        let picture = self.reference_picture(&reference.path);
                        if self.checkout.is_some() || picture.is_some() {
                            file_reference::open(self.checkout.as_deref(), &reference.path, picture.as_ref(), window, cx);
                            return;
                        }
                    }
                }
            }
        }
        let at = self.index_for(ev.position);
        match ev.click_count {
            1 if ev.modifiers.shift => self.select_to(at, cx),
            1 => self.move_to(at, cx),
            2 => {
                self.buffer.select(self.whole_chips(self.buffer.word_at(at)));
                self.moved(cx);
            }
            _ => {
                self.buffer.select(self.buffer.line_start(at)..self.buffer.line_end(at));
                self.moved(cx);
            }
        }
        self.selecting = ev.click_count == 1;
    }

    fn copy_selection(&self, cx: &mut Context<Self>) -> bool {
        let text = self.buffer.selected_text();
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
        }
        !text.is_empty()
    }
}

/// Action handlers, kept thin so the bindings read as a table.
impl TextInput {
    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        self.delete_to(Buffer::prev_grapheme, cx);
    }
    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        self.delete_to(Buffer::next_grapheme, cx);
    }
    fn delete_word_left(&mut self, _: &DeleteWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.delete_to(Buffer::prev_word, cx);
    }
    fn delete_word_right(&mut self, _: &DeleteWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.delete_to(Buffer::next_word, cx);
    }
    fn delete_to_line_start(&mut self, _: &DeleteToLineStart, _: &mut Window, cx: &mut Context<Self>) {
        let start = self.row_edge(false);
        self.delete_to(|_, _| start, cx);
    }
    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        self.step(false, cx);
    }
    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        self.step(true, cx);
    }
    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(false, false, cx);
    }
    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(true, false, cx);
    }
    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.buffer.prev_grapheme(self.buffer.cursor()), cx);
    }
    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.buffer.next_grapheme(self.buffer.cursor()), cx);
    }
    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(false, true, cx);
    }
    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(true, true, cx);
    }
    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.buffer.prev_word(self.buffer.cursor()), cx);
    }
    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.buffer.next_word(self.buffer.cursor()), cx);
    }
    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.buffer.prev_word(self.buffer.cursor()), cx);
    }
    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.buffer.next_word(self.buffer.cursor()), cx);
    }
    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.row_edge(false), cx);
    }
    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.row_edge(true), cx);
    }
    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.row_edge(false), cx);
    }
    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.row_edge(true), cx);
    }
    fn doc_start(&mut self, _: &DocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }
    fn doc_end(&mut self, _: &DocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.buffer.len(), cx);
    }
    fn select_doc_start(&mut self, _: &SelectDocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(0, cx);
    }
    fn select_doc_end(&mut self, _: &SelectDocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.buffer.len(), cx);
    }
    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.select_all();
        self.moved(cx);
    }
    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        self.copy_selection(cx);
    }
    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if self.copy_selection(cx) {
            self.insert("", cx);
        }
    }
    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(clipboard) = cx.read_from_clipboard() else { return };
        if self.paste_images && clipboard.entries().iter().any(|entry| {
            matches!(entry, gpui::ClipboardEntry::Image(_))
        }) {
            let images = clipboard.into_entries().filter_map(|entry| match entry {
                gpui::ClipboardEntry::Image(image) => Some(image),
                _ => None,
            }).collect();
            cx.emit(InputEvent::Images(images));
            return;
        }
        if let Some(text) = clipboard.text() {
            self.insert(&text.replace("\r\n", "\n"), cx);
        }
    }
    fn newline(&mut self, _: &Newline, _: &mut Window, cx: &mut Context<Self>) {
        self.insert("\n", cx);
    }
    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(InputEvent::Submit);
    }
    fn submit_alternate(&mut self, _: &SubmitAlternate, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(InputEvent::SubmitAlternate);
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.buffer.range_from_utf16(&range);
        actual.replace(self.buffer.range_to_utf16(&range));
        Some(self.buffer.text()[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.buffer.range_to_utf16(&self.buffer.selection()),
            reversed: self.buffer.reversed(),
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.buffer.marked().map(|r| self.buffer.range_to_utf16(&r))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.unmark();
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range.map(|r| self.whole_chips(self.buffer.range_from_utf16(&r)));
        self.buffer.replace(range, text);
        self.edited(cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range.map(|r| self.whole_chips(self.buffer.range_from_utf16(&r)));
        // `selected` counts UTF-16 units within the new text.
        let selected = selected.map(|s| {
            let at = |units: usize| {
                let mut n = 0;
                text.char_indices()
                    .find(|&(_, c)| {
                        let hit = n >= units;
                        n += c.len_utf16();
                        hit
                    })
                    .map_or(text.len(), |(i, _)| i)
            };
            at(s.start)..at(s.end)
        });
        self.buffer.replace_and_mark(range, text, selected);
        self.edited(cx);
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let l = self.layout.as_ref()?;
        let range = self.buffer.range_from_utf16(&range);
        let start = l.position(range.start);
        let end = l.position(range.end);
        let width = if end.y == start.y { end.x - start.x } else { px(0.) };
        Some(Bounds::new(start, size(width, l.line_height)))
    }

    fn character_index_for_point(
        &mut self,
        p: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        self.layout.as_ref()?;
        Some(self.buffer.offset_to_utf16(self.index_for(p)))
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TextInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let field = div()
            .id("text-input")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .cursor(CursorStyle::IBeam)
            .w_full()
            .max_h(px(MAX_HEIGHT))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_word_right))
            .on_action(cx.listener(Self::delete_to_line_start))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::doc_start))
            .on_action(cx.listener(Self::doc_end))
            .on_action(cx.listener(Self::select_doc_start))
            .on_action(cx.listener(Self::select_doc_end))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::submit_alternate))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .when(self.spelling_theme.is_some(), |d| {
                d.on_mouse_down(MouseButton::Right, cx.listener(Self::open_spelling_menu))
                    .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                        if this.spelling_menu.is_some() && event.keystroke.key == "escape" {
                            this.close_spelling_menu(cx);
                            cx.stop_propagation();
                        }
                    }))
            })
            .child(TextElement { input: cx.entity() });
        div().w_full().child(field).children(self.spelling_menu(cx))
    }
}

/// Shaped paragraphs and where they were painted.
#[derive(Clone)]
struct ChipSpan {
    source: Range<usize>,
    visible: Range<usize>,
}

fn map_offset(
    offset: usize,
    spans: &[ChipSpan],
    ranges: impl Fn(&ChipSpan) -> (&Range<usize>, &Range<usize>),
) -> usize {
    let (mut from_end, mut to_end) = (0, 0);
    for span in spans {
        let (from, to) = ranges(span);
        if offset <= from.start {
            break;
        }
        if offset < from.end {
            return if offset - from.start < from.end - offset { to.start } else { to.end };
        }
        from_end = from.end;
        to_end = to.end;
    }
    to_end + offset - from_end
}

struct Layout {
    spans: Vec<ChipSpan>,
    chips: Vec<gpui::Hitbox>,
    lines: Vec<WrappedLine>,
    /// Byte offset where each paragraph starts.
    starts: Vec<usize>,
    /// Top of each paragraph, relative to `origin`.
    tops: Vec<Pixels>,
    origin: Point<Pixels>,
    height: Pixels,
    line_height: Pixels,
}

impl Layout {
    fn new(lines: Vec<WrappedLine>, origin: Point<Pixels>, line_height: Pixels) -> Self {
        let (mut starts, mut tops) = (Vec::with_capacity(lines.len()), Vec::with_capacity(lines.len()));
        let (mut start, mut top) = (0, px(0.));
        for line in &lines {
            starts.push(start);
            tops.push(top);
            start += line.len() + 1;
            top += line.size(line_height).height;
        }
        Self { spans: vec![], chips: vec![], lines, starts, tops, origin, height: top.max(line_height), line_height }
    }

    /// Window position of the top of the caret at `offset`.
    fn position(&self, offset: usize) -> Point<Pixels> {
        self.visible_position(map_offset(offset, &self.spans, |s| (&s.source, &s.visible)))
    }

    fn visible_position(&self, offset: usize) -> Point<Pixels> {
        let i = self.starts.partition_point(|&s| s <= offset).saturating_sub(1);
        let Some(line) = self.lines.get(i) else { return self.origin };
        let local = line
            .position_for_index(offset - self.starts[i], self.line_height)
            .unwrap_or_default();
        point(self.origin.x + local.x, self.origin.y + self.tops[i] + local.y)
    }

    /// The offset closest to a window position.
    fn index(&self, p: Point<Pixels>) -> usize {
        let y = p.y - self.origin.y;
        let i = self.tops.partition_point(|&t| t <= y).saturating_sub(1);
        let Some(line) = self.lines.get(i) else { return 0 };
        let rows = line.wrap_boundaries().len() + 1;
        let row = ((y - self.tops[i]) / self.line_height).floor().clamp(0., (rows - 1) as f32);
        let local = point(p.x - self.origin.x, self.line_height * (row + 0.5));
        let (Ok(ix) | Err(ix)) = line.closest_index_for_position(local, self.line_height);
        map_offset(self.starts[i] + ix, &self.spans, |s| (&s.visible, &s.source))
    }

    /// The character under the pointer, excluding empty space and file chips.
    fn character_at(&self, p: Point<Pixels>) -> Option<usize> {
        let y = p.y - self.origin.y;
        if y < px(0.) || y >= self.height || self.chips.iter().any(|chip| chip.bounds.contains(&p)) {
            return None;
        }
        let i = self.tops.partition_point(|&top| top <= y).checked_sub(1)?;
        let local = point(p.x - self.origin.x, y - self.tops[i]);
        let ix = self.lines[i].index_for_position(local, self.line_height).ok()?;
        Some(map_offset(self.starts[i] + ix, &self.spans, |s| (&s.visible, &s.source)))
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct Shaping {
    text: SharedString,
    runs: Vec<TextRun>,
    font_size: Pixels,
    spans: Vec<ChipSpan>,
    thumbnails: Vec<(Range<usize>, Arc<RenderImage>)>,
}

struct Prepaint {
    layout: Option<Layout>,
    selection: Vec<PaintQuad>,
    caret: Option<PaintQuad>,
}

impl Element for TextElement {
    type RequestLayoutState = Shaping;
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Shaping) {
        let input = self.input.read(cx);
        let style = window.text_style();
        let (mut text, color) = if input.buffer.is_empty() {
            (input.placeholder.clone(), input.colors.placeholder)
        } else {
            (SharedString::from(input.buffer.text().to_string()), input.colors.text)
        };
        let run = TextRun {
            len: text.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let mut spans = vec![];
        let mut pictures = vec![];
        let mut runs = vec![];
        let mut visible = String::new();
        let marked = input.buffer.marked();
        let overlaps = |a: &Range<usize>, b: &Range<usize>| a.start < b.end && b.start < a.end;
        let mut underlines = Vec::new();
        if let Some(theme) = input.spelling_theme {
            let red = rgb(if theme.dark { 0xf87171 } else { 0xdc2626 }).into();
            underlines.extend(input.misspellings.iter()
                .filter(|range| !marked.as_ref().is_some_and(|marked| overlaps(range, marked)))
                .map(|range| (range.clone(), UnderlineStyle { color: Some(red), thickness: px(1.), wavy: true })));
        }
        if let Some(marked) = &marked {
            let index = underlines.partition_point(|(range, _)| range.start < marked.start);
            underlines.insert(index, (marked.clone(), UnderlineStyle { color: Some(color), thickness: px(1.), wavy: false }));
        }
        let plain = |range: Range<usize>, visible: &mut String, runs: &mut Vec<TextRun>| {
            if range.is_empty() {
                return;
            }
            visible.push_str(&text[range.clone()]);
            let mut at = range.start;
            let first = underlines.partition_point(|(mark, _)| mark.end <= at);
            for (mark, style) in &underlines[first..] {
                if mark.start >= range.end {
                    break;
                }
                let start = mark.start.max(range.start);
                let end = mark.end.min(range.end);
                if at < start {
                    runs.push(TextRun { len: start - at, ..run.clone() });
                }
                runs.push(TextRun { len: end - start, underline: Some(*style), ..run.clone() });
                at = end;
            }
            if at < range.end {
                runs.push(TextRun { len: range.end - at, ..run.clone() });
            }
        };
        let mut at = 0;
        for reference in &input.references {
            let source = reference.source.clone();
            if marked.as_ref().is_some_and(|m| m.start < source.end && source.start < m.end) {
                continue;
            }
            plain(at..source.start, &mut visible, &mut runs);
            let chip = file_reference::append_chip(
                &reference.path, &mut visible, &mut runs, &run, input.colors.reference,
            );
            at = source.end;
            spans.push(ChipSpan { source, visible: chip.range.clone() });
            if let Some(picture) = input.reference_picture(&reference.path) {
                pictures.push((chip, picture));
            }
        }
        plain(at..text.len(), &mut visible, &mut runs);
        let thumbnails = pictures.into_iter().filter_map(|(chip, picture)| {
            let image = file_reference::thumbnail(Some(&picture), window, cx)?;
            chip.hide_icon(&mut runs);
            Some((chip.range, image))
        }).collect();
        text = visible.into();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();

        let mut layout_style = Style::default();
        layout_style.size.width = relative(1.).into();
        let (measure_text, measure_runs) = (text.clone(), runs.clone());
        let id = window.request_measured_layout(layout_style, move |known, available, window, _| {
            let width = known.width.or(match available.width {
                AvailableSpace::Definite(w) => Some(w),
                _ => None,
            });
            let lines = window
                .text_system()
                .shape_text(measure_text.clone(), font_size, &measure_runs, width, None)
                .unwrap_or_default();
            let height = lines.iter().fold(px(0.), |h, l| h + l.size(line_height).height);
            size(width.unwrap_or_default(), height.max(line_height))
        });
        (id, Shaping { text, runs, font_size, spans, thumbnails })
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        shaping: &mut Shaping,
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        let input = self.input.read(cx);
        let line_height = window.line_height();
        let lines = window
            .text_system()
            .shape_text(shaping.text.clone(), shaping.font_size, &shaping.runs, Some(bounds.size.width), None)
            .unwrap_or_default();
        let mut layout = Layout::new(lines.into_vec(), bounds.origin, line_height);
        layout.spans.clone_from(&shaping.spans);
        layout.chips = layout.spans.iter().map(|span| {
            let bounds = file_reference::chip_bounds(&span.visible, line_height, |offset| {
                layout.visible_position(offset)
            });
            window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal)
        }).collect();
        let empty = input.buffer.is_empty();
        let sel = input.buffer.selection();
        let at = |o: usize| if empty { bounds.origin } else { layout.position(o) };

        let mut selection = Vec::new();
        if !sel.is_empty() {
            let (a, b) = (at(sel.start), at(sel.end));
            let color = input.colors.selection;
            let rect = |x0: Pixels, y0: Pixels, x1: Pixels, y1: Pixels| {
                fill(Bounds::from_corners(point(x0, y0), point(x1, y1)), color)
            };
            if a.y == b.y {
                selection.push(rect(a.x, a.y, b.x, a.y + line_height));
            } else {
                selection.push(rect(a.x, a.y, bounds.right(), a.y + line_height));
                if b.y > a.y + line_height {
                    selection.push(rect(bounds.left(), a.y + line_height, bounds.right(), b.y));
                }
                selection.push(rect(bounds.left(), b.y, b.x, b.y + line_height));
            }
        }
        let caret = sel.is_empty().then(|| {
            let p = at(input.buffer.cursor());
            fill(Bounds::new(p, size(px(2.), line_height)), input.colors.caret)
        });
        Prepaint { layout: Some(layout), selection, caret }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        shaping: &mut Shaping,
        prepaint: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (focus, show_caret, reveal) = {
            let input = self.input.read(cx);
            (input.focus.clone(), input.caret_visible && input.blink.is_some(), input.reveal)
        };
        let focused = focus.is_focused(window);
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.input.clone()), cx);

        let layout = prepaint.layout.take().expect("prepaint always lays out");
        let colors = self.input.read(cx).colors.reference;
        for chip in &layout.chips {
            window.set_cursor_style(CursorStyle::PointingHand, chip);
            window.paint_quad(file_reference::chip_border(chip.bounds, colors));
        }
        for quad in prepaint.selection.drain(..) {
            window.paint_quad(quad);
        }
        for (line, top) in layout.lines.iter().zip(&layout.tops) {
            let origin = point(layout.origin.x, layout.origin.y + *top);
            let _ = line.paint(origin, layout.line_height, TextAlign::Left, None, window, cx);
        }
        for (range, thumbnail) in shaping.thumbnails.drain(..) {
            file_reference::paint_thumbnail(thumbnail, &range, layout.line_height, |offset| {
                layout.visible_position(offset)
            }, window);
        }
        let caret = prepaint.caret.take();
        if let Some(caret) = caret.clone().filter(|_| focused && show_caret) {
            window.paint_quad(caret);
        }

        // Keep the caret inside the scrolled viewport after it moves.
        if let (true, Some(caret)) = (reveal, caret) {
            let scroll = self.input.read(cx).scroll.clone();
            let view = scroll.bounds();
            let mut offset = scroll.offset();
            let (top, bottom) = (caret.bounds.top(), caret.bounds.bottom());
            if top < view.top() {
                offset.y += view.top() - top;
            } else if bottom > view.bottom() {
                offset.y -= bottom - view.bottom();
            }
            if offset != scroll.offset() {
                scroll.set_offset(offset);
                window.refresh();
            }
        }

        // Drags keep selecting outside the field, until the button comes up.
        let input = self.input.clone();
        window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            input.update(cx, |input, cx| {
                if !input.selecting {
                    return;
                }
                if ev.pressed_button == Some(MouseButton::Left) {
                    let at = input.index_for(ev.position);
                    input.select_to(at, cx);
                } else {
                    input.selecting = false;
                }
            });
        });
        let input = self.input.clone();
        window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble {
                input.update(cx, |input, _| input.selecting = false);
            }
        });

        self.input.update(cx, |input, _| {
            input.layout = Some(layout);
            input.reveal = false;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chip_offsets_never_put_the_caret_in_markup() {
        let chips = [
            ChipSpan { source: 4..30, visible: 4..16 },
            ChipSpan { source: 37..52, visible: 23..47 },
        ];
        let to_source = |offset| map_offset(offset, &chips, |span| (&span.visible, &span.source));
        let to_visible = |offset| map_offset(offset, &chips, |span| (&span.source, &span.visible));
        assert_eq!((to_source(6), to_source(12), to_source(19), to_source(48)), (4, 30, 33, 53));
        assert_eq!((to_visible(4), to_visible(30), to_visible(37), to_visible(53)), (4, 16, 23, 48));
    }

    /// GPUI panics on a shortcut it cannot read, which would stop the app at launch.
    #[test]
    fn every_shortcut_parses_on_both_platforms() {
        assert!(!key_bindings(true).is_empty());
        assert!(!key_bindings(false).is_empty());
    }
}
