//! Split panes: a binary tree of panes over the main area.
//!
//! Every pane shows one tab of the shared tab list. The focused pane is the
//! one keys go to; its tab is the selected tab and its view (cursor, scroll)
//! lives in the live editor. The other panes remember their own view, so two
//! panes can show one buffer with separate cursors.
//!
//! Tree shape and the ratio arithmetic follow the usual tiling layout found
//! in zellij and WezTerm (MIT): a split node owns two children and the share
//! of the first one; rectangles are computed top-down each frame.

use ratatui::layout::Rect;
use skink_core::ViewState;

/// Splitting direction: side by side, or stacked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Axis {
    /// Panes side by side, a vertical divider between them.
    Columns,
    /// Panes stacked, each with its own title row.
    Rows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PaneId(u32);

/// Share of the first child, in thousandths.
const MIN_SHARE: u16 = 100;
const MAX_SHARE: u16 = 900;
pub const RESIZE_STEP: i32 = 50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Leaf(PaneId),
    Split {
        axis: Axis,
        share: u16,
        first: Box<Node>,
        second: Box<Node>,
    },
}

/// What an unfocused pane remembers about its tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneView {
    /// Focused: the live editor holds the view.
    Live,
    /// Unfocused, with its own cursor and scroll for the document.
    Remembered(ViewState),
    /// Unfocused and showing whatever the document's editor shows (a
    /// terminal, or a document this pane never had its own view of).
    Shared,
}

#[derive(Debug, Clone)]
pub struct Pane {
    pub id: PaneId,
    /// Index into the app's tab list.
    pub tab: usize,
    pub view: PaneView,
}

/// One branch of a split, to address a node from the root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    First,
    Second,
}

/// A pane's place on screen from the last layout.
#[derive(Debug, Clone, Copy)]
pub struct PaneRect {
    pub id: PaneId,
    /// Title row (when split) plus content.
    pub outer: Rect,
    /// Where the pane's tab is drawn.
    pub content: Rect,
}

/// A draggable boundary between two children of a split.
#[derive(Debug, Clone)]
pub struct Divider {
    pub path: Vec<Side>,
    pub axis: Axis,
    /// The split node's whole rectangle (to turn a mouse position into a share).
    pub node: Rect,
    /// The cells that grab the divider.
    pub grip: Rect,
}

/// Dragging a divider with the mouse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Drag {
    Idle,
    Resizing { path: Vec<Side> },
}

#[derive(Debug, Clone)]
pub struct Panes {
    root: Node,
    panes: Vec<Pane>,
    focused: PaneId,
    next: u32,
    pub drag: Drag,
}

impl Panes {
    pub fn new(tab: usize) -> Self {
        Panes {
            root: Node::Leaf(PaneId(0)),
            panes: vec![Pane {
                id: PaneId(0),
                tab,
                view: PaneView::Live,
            }],
            focused: PaneId(0),
            next: 1,
            drag: Drag::Idle,
        }
    }

    pub fn is_split(&self) -> bool {
        self.panes.len() > 1
    }

    pub fn focused(&self) -> PaneId {
        self.focused
    }

    pub fn pane(&self, id: PaneId) -> Option<&Pane> {
        self.panes.iter().find(|p| p.id == id)
    }

    fn pane_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|p| p.id == id)
    }

    #[cfg(test)]
    pub fn panes(&self) -> &[Pane] {
        &self.panes
    }

    /// Panes other than the focused one that show tab `tab`.
    pub fn others_showing(&self, tab: usize) -> bool {
        self.panes
            .iter()
            .any(|p| p.id != self.focused && p.tab == tab)
    }

    /// Store an unfocused pane's view after drawing it (its scroll may have
    /// followed its cursor).
    pub fn remember(&mut self, id: PaneId, view: ViewState) {
        if id == self.focused {
            return;
        }
        if let Some(p) = self.pane_mut(id) {
            p.view = PaneView::Remembered(view);
        }
    }

    /// The focused pane now shows `tab` (the selected tab changed).
    pub fn show_in_focused(&mut self, tab: usize) {
        let f = self.focused;
        if let Some(p) = self.pane_mut(f) {
            p.tab = tab;
            p.view = PaneView::Live;
        }
    }

    /// Split the focused pane. The new pane takes `tab` and `view` and gets
    /// the focus; the old one keeps `old_view` (what the editor showed).
    pub fn split(&mut self, axis: Axis, tab: usize, old_view: PaneView) -> PaneId {
        let new = PaneId(self.next);
        self.next += 1;
        let old = self.focused;
        replace_leaf(&mut self.root, old, &mut |leaf| Node::Split {
            axis,
            share: 500,
            first: Box::new(leaf),
            second: Box::new(Node::Leaf(new)),
        });
        if let Some(p) = self.pane_mut(old) {
            p.view = old_view;
        }
        self.panes.push(Pane {
            id: new,
            tab,
            view: PaneView::Live,
        });
        self.focused = new;
        new
    }

    /// Move the focus to `id`. Returns the pane's tab and the view it had
    /// remembered; the previously focused pane keeps `leaving`.
    pub fn focus(&mut self, id: PaneId, leaving: PaneView) -> Option<(usize, PaneView)> {
        if id == self.focused || self.pane(id).is_none() {
            return None;
        }
        let old = self.focused;
        if let Some(p) = self.pane_mut(old) {
            p.view = leaving;
        }
        self.focused = id;
        let p = self.pane_mut(id)?;
        let view = std::mem::replace(&mut p.view, PaneView::Live);
        Some((p.tab, view))
    }

    /// Close the focused pane. Returns the pane that takes the focus, its
    /// tab and its remembered view; `None` when it is the only pane.
    pub fn close_focused(&mut self) -> Option<(usize, PaneView)> {
        if !self.is_split() {
            return None;
        }
        let gone = self.focused;
        remove_leaf(&mut self.root, gone);
        self.panes.retain(|p| p.id != gone);
        let next = first_leaf(&self.root);
        self.focused = next;
        let p = self.pane_mut(next)?;
        let view = std::mem::replace(&mut p.view, PaneView::Live);
        Some((p.tab, view))
    }

    /// Keep only the focused pane.
    pub fn close_others(&mut self) {
        let f = self.focused;
        self.root = Node::Leaf(f);
        self.panes.retain(|p| p.id == f);
    }

    /// The pane after (or before) the focused one, in reading order.
    pub fn cycle(&self, delta: isize) -> PaneId {
        let order = leaves(&self.root);
        let i = order.iter().position(|&p| p == self.focused).unwrap_or(0) as isize;
        order[(i + delta).rem_euclid(order.len() as isize) as usize]
    }

    /// Grow (positive) or shrink the focused pane inside its parent split.
    pub fn resize_focused(&mut self, delta: i32) {
        let Some(path) = path_to(&self.root, self.focused) else {
            return;
        };
        let Some((last, parent)) = path.split_last() else {
            return;
        };
        let delta = if *last == Side::First { delta } else { -delta };
        if let Some(Node::Split { share, .. }) = node_at_mut(&mut self.root, parent) {
            *share = clamp_share(i32::from(*share) + delta);
        }
    }

    /// Set the share of the split at `path` from a mouse position.
    pub fn drag_to(&mut self, path: &[Side], node: Rect, col: u16, row: u16) {
        if let Some(Node::Split { axis, share, .. }) = node_at_mut(&mut self.root, path) {
            let (pos, origin, len) = match axis {
                Axis::Columns => (col, node.x, node.width),
                Axis::Rows => (row, node.y, node.height),
            };
            if len > 2 {
                let rel = i32::from(pos.saturating_sub(origin));
                *share = clamp_share(rel * 1000 / i32::from(len));
            }
        }
    }

    /// The tab list lost entry `removed`; `fallback` is what a pane that
    /// showed it shows instead. Indices after it shift down by one.
    pub fn tab_removed(&mut self, removed: usize, fallback: usize) {
        for p in &mut self.panes {
            if p.tab == removed {
                p.tab = fallback;
                if p.view != PaneView::Live {
                    p.view = PaneView::Shared;
                }
            } else if p.tab > removed {
                p.tab -= 1;
            }
        }
    }

    /// Compute every pane's rectangle and the dividers for `area`.
    pub fn layout(&self, area: Rect) -> (Vec<PaneRect>, Vec<Divider>) {
        let mut rects = Vec::new();
        let mut dividers = Vec::new();
        let titled = self.is_split();
        walk(
            &self.root,
            area,
            titled,
            &mut Vec::new(),
            &mut rects,
            &mut dividers,
        );
        (rects, dividers)
    }

    /// The tree's shape, for saving the session.
    pub fn shape(&self) -> Shape {
        shape_of(&self.root, &self.panes, self.focused)
    }

    /// Rebuild from a saved shape; `None` if it names no pane.
    pub fn from_shape(shape: &Shape, tabs: usize) -> Option<Self> {
        let mut panes = Vec::new();
        let mut focused = None;
        let mut next = 0;
        let root = build(shape, tabs, &mut panes, &mut focused, &mut next);
        let focused = focused.unwrap_or_else(|| first_leaf(&root));
        for p in &mut panes {
            p.view = if p.id == focused {
                PaneView::Live
            } else {
                PaneView::Shared
            };
        }
        (!panes.is_empty()).then_some(Panes {
            root,
            panes,
            focused,
            next,
            drag: Drag::Idle,
        })
    }
}

/// Saved form of the pane tree (session restore).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Shape {
    Pane {
        tab: usize,
        focused: bool,
    },
    Split {
        axis: Axis,
        share: u16,
        first: Box<Shape>,
        second: Box<Shape>,
    },
}

fn shape_of(n: &Node, panes: &[Pane], focused: PaneId) -> Shape {
    match n {
        Node::Leaf(id) => Shape::Pane {
            tab: panes.iter().find(|p| p.id == *id).map_or(0, |p| p.tab),
            focused: *id == focused,
        },
        Node::Split {
            axis,
            share,
            first,
            second,
        } => Shape::Split {
            axis: *axis,
            share: *share,
            first: Box::new(shape_of(first, panes, focused)),
            second: Box::new(shape_of(second, panes, focused)),
        },
    }
}

fn build(
    s: &Shape,
    tabs: usize,
    panes: &mut Vec<Pane>,
    focused: &mut Option<PaneId>,
    next: &mut u32,
) -> Node {
    match s {
        Shape::Pane { tab, focused: f } => {
            let id = PaneId(*next);
            *next += 1;
            panes.push(Pane {
                id,
                tab: (*tab).min(tabs.saturating_sub(1)),
                view: PaneView::Shared,
            });
            if *f {
                *focused = Some(id);
            }
            Node::Leaf(id)
        }
        Shape::Split {
            axis,
            share,
            first,
            second,
        } => Node::Split {
            axis: *axis,
            share: clamp_share(i32::from(*share)),
            first: Box::new(build(first, tabs, panes, focused, next)),
            second: Box::new(build(second, tabs, panes, focused, next)),
        },
    }
}

fn clamp_share(v: i32) -> u16 {
    v.clamp(i32::from(MIN_SHARE), i32::from(MAX_SHARE)) as u16
}

fn walk(
    n: &Node,
    area: Rect,
    titled: bool,
    path: &mut Vec<Side>,
    rects: &mut Vec<PaneRect>,
    dividers: &mut Vec<Divider>,
) {
    match n {
        Node::Leaf(id) => {
            let title = u16::from(titled && area.height > 1);
            rects.push(PaneRect {
                id: *id,
                outer: area,
                content: Rect {
                    y: area.y + title,
                    height: area.height - title,
                    ..area
                },
            });
        }
        Node::Split {
            axis,
            share,
            first,
            second,
        } => {
            let (a, grip, b) = split_rect(area, *axis, *share);
            dividers.push(Divider {
                path: path.clone(),
                axis: *axis,
                node: area,
                grip,
            });
            path.push(Side::First);
            walk(first, a, titled, path, rects, dividers);
            path.pop();
            path.push(Side::Second);
            walk(second, b, titled, path, rects, dividers);
            path.pop();
        }
    }
}

/// (first, divider grip, second). Columns leave one column for the divider;
/// rows put the grip on the second pane's title row.
pub fn split_rect(area: Rect, axis: Axis, share: u16) -> (Rect, Rect, Rect) {
    match axis {
        Axis::Columns => {
            let usable = area.width.saturating_sub(1);
            let w = (u32::from(usable) * u32::from(share) / 1000) as u16;
            let a = Rect { width: w, ..area };
            let grip = Rect {
                x: area.x + w,
                width: u16::from(area.width > 0),
                ..area
            };
            let b = Rect {
                x: area.x + w + 1,
                width: usable - w,
                ..area
            };
            (a, grip, b)
        }
        Axis::Rows => {
            let h = (u32::from(area.height) * u32::from(share) / 1000) as u16;
            let a = Rect { height: h, ..area };
            let b = Rect {
                y: area.y + h,
                height: area.height - h,
                ..area
            };
            let grip = Rect {
                height: u16::from(b.height > 0),
                ..b
            };
            (a, grip, b)
        }
    }
}

fn replace_leaf(n: &mut Node, id: PaneId, f: &mut dyn FnMut(Node) -> Node) -> bool {
    match n {
        Node::Leaf(l) if *l == id => {
            let leaf = std::mem::replace(n, Node::Leaf(id));
            *n = f(leaf);
            true
        }
        Node::Leaf(_) => false,
        Node::Split { first, second, .. } => {
            replace_leaf(first, id, f) || replace_leaf(second, id, f)
        }
    }
}

/// Remove leaf `id`; its sibling takes the parent's place.
fn remove_leaf(n: &mut Node, id: PaneId) -> bool {
    let Node::Split { first, second, .. } = n else {
        return false;
    };
    if **first == Node::Leaf(id) {
        *n = std::mem::replace(second.as_mut(), Node::Leaf(id));
        return true;
    }
    if **second == Node::Leaf(id) {
        *n = std::mem::replace(first.as_mut(), Node::Leaf(id));
        return true;
    }
    remove_leaf(first, id) || remove_leaf(second, id)
}

fn first_leaf(n: &Node) -> PaneId {
    match n {
        Node::Leaf(id) => *id,
        Node::Split { first, .. } => first_leaf(first),
    }
}

fn leaves(n: &Node) -> Vec<PaneId> {
    match n {
        Node::Leaf(id) => vec![*id],
        Node::Split { first, second, .. } => {
            let mut v = leaves(first);
            v.extend(leaves(second));
            v
        }
    }
}

fn path_to(n: &Node, id: PaneId) -> Option<Vec<Side>> {
    match n {
        Node::Leaf(l) => (*l == id).then(Vec::new),
        Node::Split { first, second, .. } => {
            if let Some(mut p) = path_to(first, id) {
                p.insert(0, Side::First);
                Some(p)
            } else {
                let mut p = path_to(second, id)?;
                p.insert(0, Side::Second);
                Some(p)
            }
        }
    }
}

fn node_at_mut<'a>(n: &'a mut Node, path: &[Side]) -> Option<&'a mut Node> {
    let Some((head, rest)) = path.split_first() else {
        return Some(n);
    };
    match n {
        Node::Leaf(_) => None,
        Node::Split { first, second, .. } => match head {
            Side::First => node_at_mut(first, rest),
            Side::Second => node_at_mut(second, rest),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_close_and_resize_keep_the_tree_consistent() {
        let mut p = Panes::new(0);
        let b = p.split(Axis::Columns, 0, PaneView::Shared);
        let c = p.split(Axis::Rows, 1, PaneView::Shared);
        assert_eq!(p.panes().len(), 3);
        assert_eq!(p.focused(), c);
        let (rects, divs) = p.layout(Rect::new(0, 0, 81, 20));
        assert_eq!(rects.len(), 3);
        assert_eq!(divs.len(), 2);
        // Nothing overlaps; the column divider is one cell wide.
        assert_eq!(rects[0].outer.width + 1 + rects[1].outer.width, 81);
        p.resize_focused(RESIZE_STEP);
        let (r2, _) = p.layout(Rect::new(0, 0, 81, 20));
        assert!(r2[2].outer.height > rects[2].outer.height, "focused grew");
        // Closing the focused pane hands the focus to a remaining pane.
        let (tab, _) = p.close_focused().unwrap();
        assert!(p.pane(p.focused()).is_some());
        assert_eq!(tab, 0);
        assert_eq!(p.panes().len(), 2);
        assert!(p.pane(b).is_some());
        // Removing a tab re-points panes that showed it.
        p.tab_removed(0, 0);
        assert!(p.panes().iter().all(|x| x.tab == 0));
    }

    #[test]
    fn shape_round_trips() {
        let mut p = Panes::new(0);
        p.split(Axis::Columns, 2, PaneView::Shared);
        let s = p.shape();
        let q = Panes::from_shape(&s, 3).unwrap();
        assert_eq!(q.shape(), s);
    }
}
