//! A multi-line text field with a blinking caret, keyboard and mouse
//! selection, the clipboard, and input method composition.
use crate::text_edit::Buffer;
use gpui::{
    actions, div, fill, point, prelude::*, px, relative, size, App, AvailableSpace, Bounds,
    ClipboardItem, Context, CursorStyle, DispatchPhase, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, Hsla, KeyBinding,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    ScrollHandle, SharedString, Style, Subscription, Task, TextAlign, TextRun, UTF16Selection,
    UnderlineStyle, Window, WrappedLine,
};
use std::ops::Range;
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
    ]
);

const CONTEXT: &str = "TextInput";
const BLINK: Duration = Duration::from_millis(530);
/// Taller drafts scroll inside the field.
const MAX_HEIGHT: f32 = 240.;

/// Platform-standard editing shortcuts for every text field.
pub fn bind_keys(cx: &mut App) {
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
        KeyBinding::new("secondary-a", SelectAll, c),
        KeyBinding::new("secondary-c", Copy, c),
        KeyBinding::new("secondary-x", Cut, c),
        KeyBinding::new("secondary-v", Paste, c),
        KeyBinding::new("enter", Submit, c),
        KeyBinding::new("shift-enter", Newline, c),
    ];
    if cfg!(target_os = "macos") {
        keys.extend([
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
    cx.bind_keys(keys);
}

pub enum InputEvent {
    /// The text changed.
    Changed,
    /// Enter was pressed.
    Submit,
}

#[derive(Clone, Copy, Default, PartialEq)]
pub struct Colors {
    pub text: Hsla,
    pub placeholder: Hsla,
    pub caret: Hsla,
    pub selection: Hsla,
}

pub struct TextInput {
    focus: FocusHandle,
    buffer: Buffer,
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

impl EventEmitter<InputEvent> for TextInput {}

impl TextInput {
    pub fn new(placeholder: impl Into<SharedString>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let subs = [
            cx.on_focus(&focus, window, |this, _, cx| this.show_caret(cx)),
            cx.on_blur(&focus, window, |this, _, cx| {
                this.blink = None;
                this.selecting = false;
                cx.notify();
            }),
        ];
        Self {
            focus,
            buffer: Buffer::default(),
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

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.buffer.clear();
        self.edited(cx);
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
    }

    fn edited(&mut self, cx: &mut Context<Self>) {
        self.moved(cx);
        cx.emit(InputEvent::Changed);
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.buffer.move_to(offset);
        self.moved(cx);
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.buffer.select_to(offset);
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
            self.buffer.select_to(to(&self.buffer, cursor));
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
        window.focus(&self.focus);
        let at = self.index_for(ev.position);
        match ev.click_count {
            1 if ev.modifiers.shift => self.select_to(at, cx),
            1 => self.move_to(at, cx),
            2 => {
                self.buffer.select(self.buffer.word_at(at));
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
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.insert(&text.replace("\r\n", "\n"), cx);
        }
    }
    fn newline(&mut self, _: &Newline, _: &mut Window, cx: &mut Context<Self>) {
        self.insert("\n", cx);
    }
    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(InputEvent::Submit);
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

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.buffer.unmark();
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range.map(|r| self.buffer.range_from_utf16(&r));
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
        let range = range.map(|r| self.buffer.range_from_utf16(&r));
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
        div()
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
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .child(TextElement { input: cx.entity() })
    }
}

/// Shaped paragraphs and where they were painted.
struct Layout {
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
        Self { lines, starts, tops, origin, height: top.max(line_height), line_height }
    }

    /// Window position of the top of the caret at `offset`.
    fn position(&self, offset: usize) -> Point<Pixels> {
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
        self.starts[i] + ix
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct Shaping {
    text: SharedString,
    runs: Vec<TextRun>,
    font_size: Pixels,
}

struct Prepaint {
    layout: Option<Layout>,
    selection: Vec<PaintQuad>,
    caret: Option<PaintQuad>,
}

impl IntoElement for TextElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
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
        let (text, color) = if input.buffer.is_empty() {
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
        let runs = match input.buffer.marked() {
            Some(m) => [
                TextRun { len: m.start, ..run.clone() },
                TextRun {
                    len: m.len(),
                    underline: Some(UnderlineStyle { color: Some(color), thickness: px(1.), wavy: false }),
                    ..run.clone()
                },
                TextRun { len: text.len() - m.end, ..run },
            ]
            .into_iter()
            .filter(|r| r.len > 0)
            .collect(),
            None => vec![run],
        };
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
        (id, Shaping { text, runs, font_size })
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
        let layout = Layout::new(lines.into_vec(), bounds.origin, line_height);
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
        _: &mut Shaping,
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

        for quad in prepaint.selection.drain(..) {
            window.paint_quad(quad);
        }
        let layout = prepaint.layout.take().expect("prepaint always lays out");
        for (line, top) in layout.lines.iter().zip(&layout.tops) {
            let origin = point(layout.origin.x, layout.origin.y + *top);
            let _ = line.paint(origin, layout.line_height, TextAlign::Left, None, window, cx);
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
