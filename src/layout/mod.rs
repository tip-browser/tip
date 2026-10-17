//! Layout: box tree construction and geometry.

mod block;
mod build;
mod flex;
mod grid;
mod inline;
mod intrinsic;
mod table;

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::dom::{Document, NodeId};
use crate::raster::Rect;
use crate::style::{ComputedStyle, Position, Styles};

pub use build::build_tree;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Edges {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Edges {
    pub fn h(&self) -> f32 {
        self.left + self.right
    }
    pub fn v(&self) -> f32 {
        self.top + self.bottom
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Replaced {
    Image,
    TextInput,
    TextArea,
    Checkbox,
    Radio,
    Button(String),
    Select,
    Range,
    Placeholder,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Inner {
    Flow,
    Flex,
    Grid,
    Table,
    TableRowGroup,
    TableRow,
    TableCell,
    TableCaption,
    InlineFlow,
    Text(String),
    Break,
    Replaced(Replaced),
}

#[derive(Debug, Clone)]
pub struct TextFrag {
    pub x: f32,
    pub baseline: f32,
    pub w: f32,
    pub ascent: f32,
    pub descent: f32,
    pub text: String,
    pub style: Rc<ComputedStyle>,
    pub node: Option<NodeId>,
}

impl TextFrag {
    pub fn rect(&self) -> Rect {
        Rect::new(self.x, self.baseline - self.ascent, self.w, self.ascent + self.descent)
    }
}

#[derive(Debug, Clone)]
pub struct InlineDeco {
    /// Border box of this inline fragment.
    pub rect: Rect,
    pub style: Rc<ComputedStyle>,
    pub node: Option<NodeId>,
    pub first: bool,
    pub last: bool,
}

#[derive(Debug, Clone, Default)]
pub struct LineBox {
    pub rect: Rect,
    pub baseline: f32,
    pub texts: Vec<TextFrag>,
    pub decos: Vec<InlineDeco>,
}

#[derive(Debug, Clone)]
pub struct LayoutBox {
    pub node: Option<NodeId>,
    pub style: Rc<ComputedStyle>,
    pub inline_level: bool,
    pub inner: Inner,
    pub children: Vec<LayoutBox>,
    pub content: Rect,
    pub padding: Edges,
    pub border: Edges,
    pub margin: Edges,
    pub lines: Vec<LineBox>,
    pub marker: Option<String>,
    pub fixed: bool,
    pub static_pos: (f32, f32),
    pub scroll_max: (f32, f32),
    pub scroll: (f32, f32),
    pub is_pseudo: bool,
    pub colspan: usize,
    /// Top margin has been collapsed into the parent.
    pub collapsed_top: bool,
    /// `<fieldset>` with a rendered legend: the legend's x-range relative to the border box
    /// (where the top border is interrupted) and how far below the box top the border is drawn.
    pub legend_gap: Option<(f32, f32, f32)>,
    pub(crate) intrinsic: Cell<Option<(f32, f32)>>,
}

impl LayoutBox {
    pub fn new(node: Option<NodeId>, style: Rc<ComputedStyle>, inner: Inner, inline_level: bool) -> LayoutBox {
        LayoutBox {
            node,
            style,
            inline_level,
            inner,
            children: vec![],
            content: Rect::default(),
            padding: Edges::default(),
            border: Edges::default(),
            margin: Edges::default(),
            lines: vec![],
            marker: None,
            fixed: false,
            static_pos: (0.0, 0.0),
            scroll_max: (0.0, 0.0),
            scroll: (0.0, 0.0),
            is_pseudo: false,
            colspan: 1,
            collapsed_top: false,
            legend_gap: None,
            intrinsic: Cell::new(None),
        }
    }

    pub fn padding_box(&self) -> Rect {
        Rect::new(
            self.content.x - self.padding.left,
            self.content.y - self.padding.top,
            self.content.w + self.padding.h(),
            self.content.h + self.padding.v(),
        )
    }

    pub fn border_box(&self) -> Rect {
        let p = self.padding_box();
        Rect::new(p.x - self.border.left, p.y - self.border.top, p.w + self.border.h(), p.h + self.border.v())
    }

    pub fn margin_box(&self) -> Rect {
        let b = self.border_box();
        let mt = if self.collapsed_top { 0.0 } else { self.margin.top };
        Rect::new(b.x - self.margin.left, b.y - mt, b.w + self.margin.h(), b.h + mt + self.margin.bottom)
    }

    pub fn is_text(&self) -> bool {
        matches!(self.inner, Inner::Text(_))
    }

    /// Text and line-break boxes share their element's style but are never floated or positioned.
    fn is_principal(&self) -> bool {
        !matches!(self.inner, Inner::Text(_) | Inner::Break)
    }

    pub fn is_out_of_flow(&self) -> bool {
        self.is_principal() && self.style.is_out_of_flow()
    }

    pub fn is_float(&self) -> bool {
        self.is_principal() && self.style.float != crate::style::Float::None && !self.style.is_out_of_flow()
    }

    pub fn is_positioned(&self) -> bool {
        self.is_principal() && self.style.position != Position::Static
    }

    pub fn is_scroll_container(&self) -> bool {
        self.style.overflow_y.clips() || self.style.overflow_x.clips()
    }

    /// Shift this box and all descendants.
    pub fn translate(&mut self, dx: f32, dy: f32) {
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        self.content.x += dx;
        self.content.y += dy;
        self.static_pos.0 += dx;
        self.static_pos.1 += dy;
        for l in &mut self.lines {
            l.rect.x += dx;
            l.rect.y += dy;
            l.baseline += dy;
            for t in &mut l.texts {
                t.x += dx;
                t.baseline += dy;
            }
            for d in &mut l.decos {
                d.rect.x += dx;
                d.rect.y += dy;
            }
        }
        for c in &mut self.children {
            if !c.fixed {
                c.translate(dx, dy);
            }
        }
    }

    /// Baseline of the first line box, in absolute coordinates.
    pub fn first_baseline(&self) -> Option<f32> {
        if let Some(l) = self.lines.first() {
            return Some(l.baseline);
        }
        if matches!(self.inner, Inner::Replaced(_)) {
            return None;
        }
        for c in &self.children {
            if c.is_out_of_flow() || c.is_float() {
                continue;
            }
            if let Some(b) = c.first_baseline() {
                return Some(b);
            }
        }
        None
    }

    pub fn last_baseline(&self) -> Option<f32> {
        if let Some(l) = self.lines.last() {
            return Some(l.baseline);
        }
        if matches!(self.inner, Inner::Replaced(_)) || self.style.overflow_y.clips() {
            return None;
        }
        for c in self.children.iter().rev() {
            if c.is_out_of_flow() || c.is_float() {
                continue;
            }
            if let Some(b) = c.last_baseline() {
                return Some(b);
            }
        }
        None
    }
}

/// Information the layout engine needs from the embedding page.
pub trait LayoutEnv {
    fn image_size(&self, node: NodeId) -> Option<(f32, f32)>;
    fn scroll_offset(&self, node: NodeId) -> (f32, f32);
}

pub struct NoEnv;

impl LayoutEnv for NoEnv {
    fn image_size(&self, _: NodeId) -> Option<(f32, f32)> {
        None
    }
    fn scroll_offset(&self, _: NodeId) -> (f32, f32) {
        (0.0, 0.0)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct FloatRect {
    pub rect: Rect,
    pub left: bool,
}

pub(crate) struct State<'a> {
    pub env: &'a dyn LayoutEnv,
    pub doc: &'a Document,
    pub vw: f32,
    pub vh: f32,
    pub floats: Vec<Vec<FloatRect>>,
}

impl State<'_> {
    /// Horizontal space available at [y, y+h) within [left, right].
    pub fn float_space(&self, y: f32, h: f32, left: f32, right: f32) -> (f32, f32) {
        let mut l = left;
        let mut r = right;
        if let Some(fs) = self.floats.last() {
            for f in fs {
                if f.rect.y < y + h.max(1.0) && f.rect.bottom() > y {
                    if f.left {
                        l = l.max(f.rect.right());
                    } else {
                        r = r.min(f.rect.x);
                    }
                }
            }
        }
        (l, r.max(l))
    }

    pub fn float_bottom(&self, left: bool, right: bool) -> f32 {
        let mut b = f32::MIN;
        if let Some(fs) = self.floats.last() {
            for f in fs {
                if (f.left && left) || (!f.left && right) {
                    b = b.max(f.rect.bottom());
                }
            }
        }
        b
    }
}

pub struct LayoutResult {
    pub root: Option<LayoutBox>,
    pub doc_width: f32,
    pub doc_height: f32,
    /// Border boxes per element (inline elements may have several fragments).
    pub rects: HashMap<NodeId, Vec<Rect>>,
    pub canvas_background: crate::css::Color,
}

impl LayoutResult {
    pub fn bounding_rect(&self, node: NodeId) -> Option<Rect> {
        let rs = self.rects.get(&node)?;
        let mut it = rs.iter();
        let first = *it.next()?;
        Some(it.fold(first, |acc, r| acc.union(r)))
    }

    pub fn find_box(&self, node: NodeId) -> Option<&LayoutBox> {
        fn walk(b: &LayoutBox, n: NodeId) -> Option<&LayoutBox> {
            if b.node == Some(n) && !b.is_text() && !b.is_pseudo {
                return Some(b);
            }
            b.children.iter().find_map(|c| walk(c, n))
        }
        walk(self.root.as_ref()?, node)
    }
}

/// Lay out a document for a viewport.
pub fn layout_document(doc: &Document, styles: &Styles, env: &dyn LayoutEnv, vw: f32, vh: f32) -> LayoutResult {
    let mut root = build_tree(doc, styles);
    let mut st = State { env, doc, vw, vh, floats: vec![vec![]] };
    let mut doc_h = vh;
    let mut doc_w = vw;
    let mut bg = crate::css::Color::WHITE;
    if let Some(r) = root.as_mut() {
        block::layout_block_level(r, &mut st, vw, Some(vh), 0.0, 0.0, false);
        block::layout_positioned(r, &mut st, Rect::new(0.0, 0.0, vw, vh));
        let mb = r.margin_box();
        let extent = content_extent(r);
        doc_h = mb.bottom().max(extent.1).max(vh);
        doc_w = mb.right().max(extent.0).max(vw);
        // Canvas background: html's, else body's.
        bg = r.style.background_color;
        if bg.a == 0 {
            if let Some(body) = r.children.iter().find(|c| c.node.is_some_and(|n| doc.tag_is(n, "body"))) {
                bg = body.style.background_color;
            }
        }
        if bg.a < 255 {
            // composite on white
            let a = bg.a as f32 / 255.0;
            let mix = |c: u8| (c as f32 * a + 255.0 * (1.0 - a)) as u8;
            bg = crate::css::Color::rgb(mix(bg.r), mix(bg.g), mix(bg.b));
        }
    }
    let mut rects = HashMap::new();
    if let Some(r) = &root {
        collect_rects(r, &mut rects);
    }
    LayoutResult { root, doc_width: doc_w, doc_height: doc_h, rects, canvas_background: bg }
}

/// Max right/bottom of all non-fixed descendants not clipped by a scroll container.
fn content_extent(b: &LayoutBox) -> (f32, f32) {
    let mb = b.margin_box();
    let mut ext = (mb.right(), mb.bottom());
    if b.is_scroll_container() && b.node.is_some() {
        return ext;
    }
    for l in &b.lines {
        ext.0 = ext.0.max(l.rect.right());
        ext.1 = ext.1.max(l.rect.bottom());
    }
    for c in &b.children {
        if c.fixed {
            continue;
        }
        let e = content_extent(c);
        ext.0 = ext.0.max(e.0);
        ext.1 = ext.1.max(e.1);
    }
    ext
}

fn collect_rects(b: &LayoutBox, out: &mut HashMap<NodeId, Vec<Rect>>) {
    if let Some(n) = b.node {
        if !b.is_text() && !b.is_pseudo && b.inner != Inner::InlineFlow {
            out.entry(n).or_default().push(b.border_box());
        }
    }
    for l in &b.lines {
        for d in &l.decos {
            if let Some(n) = d.node {
                out.entry(n).or_default().push(d.rect);
            }
        }
    }
    for c in &b.children {
        collect_rects(c, out);
    }
}

pub fn dump(b: &LayoutBox, depth: usize, doc: &Document, out: &mut String) {
    let name = match (&b.inner, b.node) {
        (Inner::Text(t), _) => format!("text {:?}", crate::dom::collapse_ws(t)),
        (_, Some(n)) => doc.tag(n).map(|t| format!("<{t}>")).unwrap_or("?".into()),
        _ => "anon".into(),
    };
    let r = b.border_box();
    out.push_str(&format!(
        "{}{} {:?} [{:.1},{:.1} {:.1}x{:.1}]{}\n",
        "  ".repeat(depth),
        name,
        match &b.inner {
            Inner::Text(_) => "Text".to_string(),
            i => format!("{:?}", i),
        },
        r.x,
        r.y,
        r.w,
        r.h,
        if b.inline_level { " inline" } else { "" }
    ));
    for (i, l) in b.lines.iter().enumerate() {
        let txt: Vec<&str> = l.texts.iter().map(|t| t.text.as_str()).collect();
        out.push_str(&format!("{}  line {i} y={:.1} h={:.1}: {:?}\n", "  ".repeat(depth), l.rect.y, l.rect.h, txt.join("")));
    }
    for c in &b.children {
        if !c.is_text() {
            dump(c, depth + 1, doc, out);
        }
    }
}

pub(crate) fn is_positioned(b: &LayoutBox) -> bool {
    b.is_positioned()
}
