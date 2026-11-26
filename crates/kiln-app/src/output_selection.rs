//! Mouse selection over the styled text fragments of one transcript message.

use gpui::{
    canvas, fill, point, prelude::*, px, size, App, Bounds, ClipboardItem, DispatchPhase, Div,
    FocusHandle, Hsla, KeyBinding, MouseButton, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    SharedString, StyledText, TextLayout, Window,
};
use std::{
    cell::RefCell,
    ops::Range,
    rc::Rc,
    time::{Duration, Instant},
};

pub fn bind_keys(cx: &mut App) {
    let shortcut = if cfg!(target_os = "macos") {
        "cmd-c"
    } else {
        "ctrl-c"
    };
    cx.bind_keys([KeyBinding::new(
        shortcut,
        crate::text_input::Copy,
        Some("Transcript"),
    )]);
}

pub struct Selection {
    pub focus: FocusHandle,
    active: RefCell<Option<Drag>>,
    copied_code: RefCell<Option<(SharedString, Instant)>>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Position {
    fragment: usize,
    byte: usize,
}

struct Drag {
    message: usize,
    anchor: Position,
    head: Position,
    pressed: bool,
    moved: bool,
    fragments: Rc<Vec<Fragment>>,
}

impl Drag {
    fn range(&self, i: usize, text: &str) -> Range<usize> {
        let start = self.anchor.min(self.head);
        let end = self.anchor.max(self.head);
        if i < start.fragment || i > end.fragment || start == end {
            return 0..0;
        }
        let from = if i == start.fragment { start.byte } else { 0 };
        let to = if i == end.fragment {
            end.byte
        } else {
            text.len()
        };
        text.floor_char_boundary(from.min(text.len()))..text.floor_char_boundary(to.min(text.len()))
    }
}

impl Selection {
    pub fn new(cx: &mut App) -> Self {
        Self {
            focus: cx.focus_handle(),
            active: RefCell::new(None),
            copied_code: RefCell::new(None),
        }
    }

    pub fn reset(&self) {
        self.clear();
        self.copied_code.borrow_mut().take();
    }

    fn clear(&self) {
        self.active.borrow_mut().take();
    }

    pub fn copy(&self, cx: &mut App) {
        let active = self.active.borrow();
        let Some(drag) = active.as_ref().filter(|drag| drag.anchor != drag.head) else {
            return;
        };
        let mut copied = String::new();
        for (i, fragment) in drag.fragments.iter().enumerate() {
            let range = drag.range(i, &fragment.text);
            if range.is_empty() {
                continue;
            }
            if !copied.is_empty() {
                copied.push('\n');
            }
            let mut start = range.start;
            for (chip, path) in &fragment.chips {
                if chip.start < range.end && chip.end > start {
                    copied.push_str(&fragment.text[start..start.max(chip.start)]);
                    copied.push_str(path);
                    start = chip.end.min(range.end);
                }
            }
            copied.push_str(&fragment.text[start..range.end]);
        }
        if !copied.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(copied));
        }
    }

    pub fn dragged(&self, message: usize) -> bool {
        self.active
            .borrow()
            .as_ref()
            .is_some_and(|drag| drag.message == message && drag.moved)
    }
}

pub struct Document {
    selection: Rc<Selection>,
    message: usize,
    fragments: Vec<Fragment>,
}

struct Fragment {
    text: SharedString,
    layout: TextLayout,
    chips: Vec<(Range<usize>, String)>,
}

impl Document {
    pub fn new(selection: Rc<Selection>, message: usize) -> Self {
        Self {
            selection,
            message,
            fragments: Vec::new(),
        }
    }

    pub fn code_copied(&self, id: &SharedString) -> bool {
        self.selection
            .copied_code
            .borrow()
            .as_ref()
            .is_some_and(|(copied, until)| copied == id && Instant::now() < *until)
    }

    pub fn copy_code(
        &self,
        id: SharedString,
        code: SharedString,
    ) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + use<> {
        let selection = self.selection.clone();
        move |_, window, cx| {
            const FEEDBACK: Duration = Duration::from_millis(1500);
            cx.write_to_clipboard(ClipboardItem::new_string(code.to_string()));
            *selection.copied_code.borrow_mut() = Some((id.clone(), Instant::now() + FEEDBACK));
            window.refresh();
            window
                .spawn(cx, async move |cx| {
                    cx.background_executor().timer(FEEDBACK).await;
                    let _ = cx.update(|window, _| window.refresh());
                })
                .detach();
        }
    }

    pub fn drag_guard(&self) -> impl Fn() -> bool + use<> {
        let selection = self.selection.clone();
        let message = self.message;
        move || selection.dragged(message)
    }

    pub fn plain(&mut self, text: impl Into<SharedString>) -> StyledText {
        let text = text.into();
        let styled = StyledText::new(text.clone());
        self.track(text, &styled, Vec::new());
        styled
    }

    pub fn track(
        &mut self,
        text: SharedString,
        styled: &StyledText,
        chips: Vec<(Range<usize>, String)>,
    ) {
        if !text.is_empty() {
            self.fragments.push(Fragment {
                text,
                layout: styled.layout().clone(),
                chips,
            });
        }
    }

    pub fn render(self, content: Div, color: Hsla) -> Div {
        let Self {
            selection,
            message,
            fragments,
        } = self;
        if fragments.is_empty() {
            return content;
        }
        let fragments = Rc::new(fragments);
        content
            .relative()
            .on_mouse_down(MouseButton::Left, {
                let fragments = fragments.clone();
                let selection = selection.clone();
                move |event, window, cx| {
                    let hit = fragments
                        .iter()
                        .any(|f| f.layout.bounds().contains(&event.position));
                    if hit {
                        let at = locate(&fragments, event.position);
                        *selection.active.borrow_mut() = Some(Drag {
                            message,
                            anchor: at,
                            head: at,
                            pressed: true,
                            moved: false,
                            fragments: fragments.clone(),
                        });
                        selection.focus.focus(window, cx);
                    } else {
                        selection.clear();
                    }
                    window.refresh();
                }
            })
            .on_mouse_down_out({
                let selection = selection.clone();
                move |event, window, _| {
                    let owns_selection = selection
                        .active
                        .borrow()
                        .as_ref()
                        .is_some_and(|drag| drag.message == message);
                    if event.button == MouseButton::Left && owns_selection {
                        selection.clear();
                        window.refresh();
                    }
                }
            })
            .child(
                canvas(
                    |_, _, _| (),
                    move |_, (), window, _| {
                        let active = selection.active.borrow();
                        let Some(drag) = active.as_ref().filter(|drag| {
                            drag.message == message && selection.focus.is_focused(window)
                        }) else {
                            return;
                        };
                        for (i, fragment) in fragments.iter().enumerate() {
                            let range = drag.range(i, &fragment.text);
                            if !range.is_empty() {
                                fragment.paint(range, color, window);
                            }
                        }
                        if !drag.pressed {
                            return;
                        }
                        window.on_mouse_event({
                            let fragments = fragments.clone();
                            let selection = selection.clone();
                            move |event: &MouseMoveEvent, phase, window, _| {
                                if phase != DispatchPhase::Capture {
                                    return;
                                }
                                let mut active = selection.active.borrow_mut();
                                let Some(drag) = active
                                    .as_mut()
                                    .filter(|drag| drag.message == message && drag.pressed)
                                else {
                                    return;
                                };
                                if event.pressed_button != Some(MouseButton::Left) {
                                    drag.pressed = false;
                                    return;
                                }
                                let head = locate(&fragments, event.position);
                                if drag.head != head {
                                    drag.head = head;
                                    drag.moved = true;
                                    drag.fragments = fragments.clone();
                                    window.refresh();
                                }
                            }
                        });
                        let selection = selection.clone();
                        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, _| {
                            if phase == DispatchPhase::Capture && event.button == MouseButton::Left
                            {
                                if let Some(drag) = selection.active.borrow_mut().as_mut() {
                                    drag.pressed = false;
                                }
                            }
                        });
                    },
                )
                .absolute()
                .inset_0(),
            )
    }
}

// When the pointer is in a gap, use the closest text fragment. This also
// handles a drag past either end of the message and side-by-side table cells.
fn locate(fragments: &[Fragment], at: Point<Pixels>) -> Position {
    let (fragment, hit) = fragments
        .iter()
        .enumerate()
        .min_by_key(|(_, f)| {
            let bounds = f.layout.bounds();
            let dy = (bounds.top() - at.y)
                .max(at.y - bounds.bottom())
                .max(px(0.));
            let dx = (bounds.left() - at.x)
                .max(at.x - bounds.right())
                .max(px(0.));
            (dy, dx)
        })
        .expect("a selectable message has text");
    let origin = hit.layout.bounds().origin;
    let height = hit.layout.line_height();
    let mut top = origin.y;
    let mut offset = 0;
    if at.y >= top {
        for line in hit.layout.line_layouts() {
            let line_height = height * (line.wrap_boundaries.len() + 1) as f32;
            if at.y < top + line_height {
                let local = point(at.x - origin.x, at.y - top);
                let byte = line
                    .closest_index_for_position(local, height)
                    .unwrap_or_else(|i| i);
                return Position {
                    fragment,
                    byte: offset + byte,
                };
            }
            top += line_height;
            offset += line.len() + 1;
        }
        offset = hit.text.len();
    }
    Position {
        fragment,
        byte: offset,
    }
}

impl Fragment {
    fn paint(&self, range: Range<usize>, color: Hsla, window: &mut Window) {
        let origin = self.layout.bounds().origin;
        let height = self.layout.line_height();
        let mut y = origin.y;
        let mut offset = 0;
        for line in self.layout.line_layouts() {
            if offset >= range.end {
                break;
            }
            let layout = &line.unwrapped_layout;
            let ends = line
                .wrap_boundaries
                .iter()
                .map(|boundary| layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix].index)
                .chain(std::iter::once(line.len()));
            let mut row_start = 0;
            for row_end in ends {
                let start = row_start.max(range.start.saturating_sub(offset));
                let end = row_end.min(range.end.saturating_sub(offset));
                if start < end {
                    let left = layout.x_for_index(start) - layout.x_for_index(row_start);
                    let right = layout.x_for_index(end) - layout.x_for_index(row_start);
                    let bounds = Bounds::new(point(origin.x + left, y), size(right - left, height));
                    window.paint_quad(fill(bounds, color));
                }
                row_start = row_end;
                y += height;
            }
            offset += line.len() + 1;
        }
    }
}
