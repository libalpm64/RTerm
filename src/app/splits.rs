//! A split workspace contains references to existing terminal tabs, not new
//! transport owners. Selecting an unshown tab replaces only the focused pane.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Direction {
    Right,
    Down,
}

const MAX_PANES: usize = 4;
const DIVIDER: f32 = 4.0;
const MIN_PANE: egui::Vec2 = vec2(120.0, 90.0);

#[derive(Debug)]
enum Node {
    Leaf(u64),
    Branch {
        id: u64,
        direction: Direction,
        fraction: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
}

impl Node {
    fn contains(&self, tab: u64) -> bool {
        match self {
            Self::Leaf(id) => *id == tab,
            Self::Branch { first, second, .. } => first.contains(tab) || second.contains(tab),
        }
    }

    fn count(&self) -> usize {
        match self {
            Self::Leaf(_) => 1,
            Self::Branch { first, second, .. } => first.count() + second.count(),
        }
    }

    fn first(&self) -> u64 {
        match self {
            Self::Leaf(tab) => *tab,
            Self::Branch { first, .. } => first.first(),
        }
    }

    fn replace(&mut self, old: u64, new: u64) {
        match self {
            Self::Leaf(tab) if *tab == old => *tab = new,
            Self::Leaf(_) => {}
            Self::Branch { first, second, .. } => {
                first.replace(old, new);
                second.replace(old, new);
            }
        }
    }

    fn swap(&mut self, a: u64, b: u64) {
        match self {
            Self::Leaf(tab) if *tab == a => *tab = b,
            Self::Leaf(tab) if *tab == b => *tab = a,
            Self::Leaf(_) => {}
            Self::Branch { first, second, .. } => {
                first.swap(a, b);
                second.swap(a, b);
            }
        }
    }

    fn split(&mut self, anchor: u64, tab: u64, direction: Direction, id: u64) {
        match self {
            Self::Leaf(current) if *current == anchor => {
                *self = Self::Branch {
                    id,
                    direction,
                    fraction: 0.5,
                    first: Box::new(Self::Leaf(anchor)),
                    second: Box::new(Self::Leaf(tab)),
                };
            }
            Self::Leaf(_) => {}
            Self::Branch { first, second, .. } => {
                if first.contains(anchor) {
                    first.split(anchor, tab, direction, id);
                } else {
                    second.split(anchor, tab, direction, id);
                }
            }
        }
    }

    fn remove(self, tab: u64) -> Option<Self> {
        match self {
            Self::Leaf(current) => (current != tab).then_some(Self::Leaf(current)),
            Self::Branch {
                id,
                direction,
                fraction,
                first,
                second,
            } => match (first.remove(tab), second.remove(tab)) {
                (Some(first), Some(second)) => Some(Self::Branch {
                    id,
                    direction,
                    fraction,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(remaining), None) | (None, Some(remaining)) => Some(remaining),
                (None, None) => None,
            },
        }
    }

    fn minimum(&self) -> egui::Vec2 {
        match self {
            Self::Leaf(_) => MIN_PANE,
            Self::Branch {
                direction,
                first,
                second,
                ..
            } => {
                let a = first.minimum();
                let b = second.minimum();
                match direction {
                    Direction::Right => vec2(a.x + b.x + DIVIDER, a.y.max(b.y)),
                    Direction::Down => vec2(a.x.max(b.x), a.y + b.y + DIVIDER),
                }
            }
        }
    }

    fn layout(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        theme: Theme,
        interactive: bool,
        panes: &mut Vec<(u64, Rect)>,
        divider_interacted: &mut bool,
    ) {
        match self {
            Self::Leaf(tab) => panes.push((*tab, rect)),
            Self::Branch {
                id,
                direction,
                fraction,
                first,
                second,
            } => {
                let horizontal = *direction == Direction::Right;
                let axis = usize::from(!horizontal);
                let length = (rect.size()[axis] - DIVIDER).max(1.0);
                let low = first.minimum()[axis] / length;
                let high = 1.0 - second.minimum()[axis] / length;
                // The caller checks the minimum size of the whole tree.
                let split = fraction.clamp(low.min(high), high.max(low));
                let scale = ui.ctx().pixels_per_point();
                let position = ((rect.min[axis] + length * split) * scale).round() / scale;
                let divider = if horizontal {
                    rect.with_min_x(position).with_max_x(position + DIVIDER)
                } else {
                    rect.with_min_y(position).with_max_y(position + DIVIDER)
                };
                let response = ui
                    .interact(
                        divider,
                        ui.id().with(("terminal-split", *id)),
                        if interactive {
                            Sense::DRAG
                        } else {
                            Sense::hover()
                        },
                    )
                    .on_hover_cursor(if horizontal {
                        egui::CursorIcon::ResizeHorizontal
                    } else {
                        egui::CursorIcon::ResizeVertical
                    });
                *divider_interacted |= interactive
                    && (response.dragged()
                        || response.drag_stopped()
                        || response.is_pointer_button_down_on());
                let color = if response.hovered() || response.dragged() {
                    theme.accent
                } else {
                    theme.bg
                };
                theme::band(ui, divider, theme.bg, None);
                let line = if horizontal {
                    Rect::from_center_size(divider.center(), vec2(1.0 / scale, divider.height()))
                } else {
                    Rect::from_center_size(divider.center(), vec2(divider.width(), 1.0 / scale))
                };
                ui.painter().rect_filled(line, 0.0, color);
                if interactive && response.dragged() {
                    // drag_delta is incremental in egui. Keep the divider
                    // constrained by the actual minimum of both subtrees.
                    *fraction = (split + response.drag_delta()[axis] / length)
                        .clamp(low.min(high), high.max(low));
                    ui.ctx().request_repaint();
                }
                let (a, b) = if horizontal {
                    (
                        rect.with_max_x(divider.left()),
                        rect.with_min_x(divider.right()),
                    )
                } else {
                    (
                        rect.with_max_y(divider.top()),
                        rect.with_min_y(divider.bottom()),
                    )
                };
                first.layout(ui, a, theme, interactive, panes, divider_interacted);
                second.layout(ui, b, theme, interactive, panes, divider_interacted);
            }
        }
    }
}

#[derive(Default)]
pub(super) struct Splits {
    root: Option<Node>,
    next_id: u64,
    pub(super) divider_interacted: bool,
}

impl Splits {
    pub(super) fn contains(&self, tab: u64) -> bool {
        self.root.as_ref().is_some_and(|node| node.contains(tab))
    }

    pub(super) fn can_split(&self) -> bool {
        self.root.as_ref().map_or(1, Node::count) < MAX_PANES
    }

    pub(super) fn is_split(&self) -> bool {
        self.root.is_some()
    }

    pub(super) fn split(&mut self, anchor: u64, tab: u64, direction: Direction) -> bool {
        if anchor == tab || self.contains(tab) || !self.can_split() {
            return false;
        }
        let root = self.root.get_or_insert(Node::Leaf(anchor));
        if !root.contains(anchor) {
            return false;
        }
        self.next_id += 1;
        root.split(anchor, tab, direction, self.next_id);
        true
    }

    pub(super) fn select(&mut self, old: Option<u64>, tab: u64) {
        if let Some(root) = &mut self.root
            && !root.contains(tab)
        {
            let old = old
                .filter(|id| root.contains(*id))
                .unwrap_or_else(|| root.first());
            root.replace(old, tab);
        }
    }

    pub(super) fn place(&mut self, target: u64, tab: u64) {
        if let Some(root) = &mut self.root
            && root.contains(target)
            && target != tab
        {
            if root.contains(tab) {
                root.swap(target, tab);
            } else {
                root.replace(target, tab);
            }
        }
    }

    pub(super) fn remove(&mut self, tab: u64) -> Option<u64> {
        let remaining = self.root.take()?.remove(tab);
        let next = remaining.as_ref().map(Node::first);
        // Return to the ordinary one-tab workspace when only one pane remains.
        self.root = remaining.filter(|node| node.count() > 1);
        next
    }

    pub(super) fn clear(&mut self) {
        self.root = None;
    }

    pub(super) fn layout(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        active: Option<u64>,
        theme: Theme,
        interactive: bool,
    ) -> Vec<(u64, Rect)> {
        let mut panes = Vec::with_capacity(MAX_PANES);
        self.divider_interacted = false;
        if let Some(root) = &mut self.root
            && rect.width() >= root.minimum().x
            && rect.height() >= root.minimum().y
        {
            root.layout(
                ui,
                rect,
                theme,
                interactive,
                &mut panes,
                &mut self.divider_interacted,
            );
        } else if let Some(tab) = active {
            // Narrow windows temporarily show the focused tab. Preserve the
            // tree and divider positions when space is available again.
            panes.push((tab, rect));
        }
        panes
    }
}

impl App {
    pub(super) fn split_new(&mut self, direction: Direction) {
        if self.locked || self.show_keys || self.file_workspace_active() {
            return;
        }
        let Some(anchor) = self.active else {
            self.new_local();
            return;
        };
        if self.splits.split(anchor, self.next_tab, direction) {
            self.new_local();
        }
    }

    pub(super) fn split_existing(&mut self, tab: u64, direction: Direction) {
        if self.locked
            || self.show_keys
            || self.file_workspace_active()
            || !self
                .terminals
                .iter()
                .any(|term| term.tab == tab && term.kind != Kind::Sftp)
        {
            return;
        }
        if self.splits.contains(tab) {
            self.activate(tab);
        } else if let Some(anchor) = self.active
            && self.splits.split(anchor, tab, direction)
        {
            self.activate(tab);
        }
    }

    pub(super) fn single_pane(&mut self) {
        self.splits.clear();
        self.focus_terminal_after_dialog();
    }

    pub(super) fn hide_pane(&mut self, tab: u64) {
        if self.splits.contains(tab) {
            let bottom_focused = self.bottom_open && self.bottom_focus;
            let next = self.splits.remove(tab);
            if self.active == Some(tab)
                && let Some(next) = next
            {
                self.activate(next);
            }
            self.bottom_focus = bottom_focused;
            self.focus_terminal_after_dialog();
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/app/splits.rs"]
mod tests;
