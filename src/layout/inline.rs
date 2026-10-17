//! Inline formatting context: line breaking and line box construction.

use std::rc::Rc;

use super::block::{self, Avail, WidthMode};
use super::{InlineDeco, Inner, LayoutBox, LineBox, State, TextFrag};
use crate::dom::NodeId;
use crate::raster::Rect;
use crate::style::{ComputedStyle, TextAlign, TextTransform, VAlign, WhiteSpace};
use crate::text::{self, FontKey};

enum Item {
    Word {
        text: String,
        w: f32,
        style: Rc<ComputedStyle>,
        node: Option<NodeId>,
        space: bool,
        collapsible: bool,
        soft_before: bool,
        wrap: bool,
    },
    Atomic {
        path: Vec<usize>,
        w: f32,
        h: f32,
        base: f32,
        valign: VAlign,
        wrap: bool,
    },
    Open {
        path: Vec<usize>,
        extra: f32,
        ml: f32,
    },
    Close {
        extra: f32,
        mr: f32,
    },
    Break,
    Float {
        path: Vec<usize>,
    },
    OutOfFlow {
        path: Vec<usize>,
    },
}

fn key_of(s: &ComputedStyle) -> FontKey {
    FontKey::new(s.font_family, s.font_weight, s.italic)
}

fn get_mut<'a>(children: &'a mut [LayoutBox], path: &[usize]) -> &'a mut LayoutBox {
    let mut b = &mut children[path[0]];
    for &i in &path[1..] {
        b = &mut b.children[i];
    }
    b
}

fn get<'a>(children: &'a [LayoutBox], path: &[usize]) -> &'a LayoutBox {
    let mut b = &children[path[0]];
    for &i in &path[1..] {
        b = &b.children[i];
    }
    b
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x2E80..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)
}

fn transform(text: &str, t: TextTransform, prev_space: bool) -> String {
    match t {
        TextTransform::None => text.to_string(),
        TextTransform::Upper => text.to_uppercase(),
        TextTransform::Lower => text.to_lowercase(),
        TextTransform::Capitalize => {
            let mut out = String::with_capacity(text.len());
            let mut at_start = prev_space;
            for c in text.chars() {
                if at_start && c.is_alphabetic() {
                    out.extend(c.to_uppercase());
                    at_start = false;
                } else {
                    out.push(c);
                    if c.is_whitespace() {
                        at_start = true;
                    } else if c.is_alphanumeric() {
                        at_start = false;
                    }
                }
            }
            out
        }
    }
}

struct Collector<'a, 'b> {
    st: &'a mut State<'b>,
    items: Vec<Item>,
    prev_space: bool,
    avail_w: f32,
}

impl Collector<'_, '_> {
    fn push_word(&mut self, text: String, style: &Rc<ComputedStyle>, node: Option<NodeId>, soft_before: bool) {
        if text.is_empty() {
            return;
        }
        let w = text::measure(key_of(style), style.font_size, &text, style.letter_spacing);
        self.items.push(Item::Word { text, w, style: style.clone(), node, space: false, collapsible: false, soft_before, wrap: style.white_space.wraps() });
        self.prev_space = false;
    }

    fn push_space(&mut self, text: &str, style: &Rc<ComputedStyle>, node: Option<NodeId>, collapsible: bool) {
        let w = text::measure(key_of(style), style.font_size, text, style.letter_spacing) + style.word_spacing * text.chars().count() as f32;
        self.items.push(Item::Word { text: text.to_string(), w, style: style.clone(), node, space: true, collapsible, soft_before: false, wrap: style.white_space.wraps() });
        self.prev_space = true;
    }

    fn text(&mut self, raw: &str, style: &Rc<ComputedStyle>, node: Option<NodeId>) {
        let ws = style.white_space;
        let raw = transform(raw, style.text_transform, self.prev_space);
        match ws {
            WhiteSpace::Pre => {
                for (i, line) in raw.split('\n').enumerate() {
                    if i > 0 {
                        self.items.push(Item::Break);
                        self.prev_space = true;
                    }
                    let line = line.replace('\t', "        ").replace('\r', "");
                    if !line.is_empty() {
                        self.push_word(line, style, node, false);
                    }
                }
            }
            WhiteSpace::PreWrap => {
                for (i, line) in raw.split('\n').enumerate() {
                    if i > 0 {
                        self.items.push(Item::Break);
                        self.prev_space = true;
                    }
                    let line = line.replace('\t', "        ").replace('\r', "");
                    let mut word = String::new();
                    let mut spaces = String::new();
                    for c in line.chars() {
                        if c == ' ' {
                            if !word.is_empty() {
                                let soft = self.prev_space;
                                self.push_word(std::mem::take(&mut word), style, node, soft);
                            }
                            spaces.push(c);
                        } else {
                            if !spaces.is_empty() {
                                self.push_space(&std::mem::take(&mut spaces), style, node, false);
                            }
                            word.push(c);
                        }
                    }
                    if !word.is_empty() {
                        let soft = self.prev_space;
                        self.push_word(word, style, node, soft);
                    }
                    if !spaces.is_empty() {
                        self.push_space(&spaces, style, node, false);
                    }
                }
            }
            WhiteSpace::Normal | WhiteSpace::NoWrap | WhiteSpace::PreLine => {
                let mut word = String::new();
                let mut soft_next = false;
                let flush = |this: &mut Self, word: &mut String, soft: bool| {
                    if !word.is_empty() {
                        this.push_word(std::mem::take(word), style, node, soft);
                    }
                };
                for c in raw.chars() {
                    if c == '\n' && ws == WhiteSpace::PreLine {
                        flush(self, &mut word, soft_next);
                        soft_next = false;
                        self.items.push(Item::Break);
                        self.prev_space = true;
                        continue;
                    }
                    if c.is_ascii_whitespace() || c == '\u{200b}' {
                        flush(self, &mut word, soft_next);
                        soft_next = false;
                        if c == '\u{200b}' {
                            soft_next = true;
                            continue;
                        }
                        if !self.prev_space {
                            self.push_space(" ", style, node, true);
                        }
                        continue;
                    }
                    if is_cjk(c) {
                        flush(self, &mut word, soft_next);
                        let mut s = String::new();
                        s.push(c);
                        self.push_word(s, style, node, true);
                        soft_next = true;
                        continue;
                    }
                    word.push(c);
                    if c == '-' && word.chars().count() > 2 {
                        flush(self, &mut word, soft_next);
                        soft_next = true;
                    } else if word.len() == c.len_utf8() && soft_next {
                        // keep soft flag for this word start
                    }
                }
                flush(self, &mut word, soft_next);
            }
        }
    }

    fn collect(&mut self, children: &mut [LayoutBox], path: &mut Vec<usize>) {
        for i in 0..children.len() {
            path.push(i);
            let c = &mut children[i];
            if c.is_out_of_flow() {
                self.items.push(Item::OutOfFlow { path: path.clone() });
            } else if c.is_float() {
                self.items.push(Item::Float { path: path.clone() });
            } else {
                match &c.inner {
                    Inner::Text(t) => {
                        let t = t.clone();
                        let style = c.style.clone();
                        let node = c.node;
                        self.text(&t, &style, node);
                    }
                    Inner::Break => {
                        self.items.push(Item::Break);
                        self.prev_space = true;
                    }
                    Inner::InlineFlow => {
                        let s = c.style.clone();
                        c.padding = block::resolve_edges(&s.padding, self.avail_w);
                        c.border = block::border_edges(c);
                        c.margin = block::resolve_edges(&s.margin, self.avail_w);
                        let ml = c.margin.left;
                        let mr = c.margin.right;
                        let left = ml + c.border.left + c.padding.left;
                        let right = mr + c.border.right + c.padding.right;
                        self.items.push(Item::Open { path: path.clone(), extra: left, ml });
                        let mut kids = std::mem::take(&mut c.children);
                        self.collect(&mut kids, path);
                        children[i].children = kids;
                        self.items.push(Item::Close { extra: right, mr });
                    }
                    _ => {
                        // atomic inline-level box
                        let a = Avail { cb_w: self.avail_w, cb_h: None, mode: WidthMode::Shrink, forced_h: None, bfc: true };
                        block::layout_box(c, self.st, 0.0, 0.0, &a);
                        let mb = c.margin_box();
                        let base = if let Inner::Replaced(super::Replaced::TextInput | super::Replaced::Button(_) | super::Replaced::Select) = &c.inner {
                            // form controls align on their text baseline
                            let m = text::metrics(key_of(&c.style), c.style.font_size);
                            c.content.y - mb.y + (c.content.h - (m.ascent + m.descent)) / 2.0 + m.ascent
                        } else if let Inner::Replaced(super::Replaced::Checkbox | super::Replaced::Radio) = &c.inner {
                            mb.h - c.margin.bottom - 2.0
                        } else if matches!(c.inner, Inner::Replaced(_)) || c.style.overflow_y.clips() {
                            mb.h
                        } else {
                            c.last_baseline().map(|b| b - mb.y).unwrap_or(mb.h)
                        };
                        self.items.push(Item::Atomic { path: path.clone(), w: mb.w, h: mb.h, base, valign: c.style.vertical_align, wrap: true });
                        self.prev_space = false;
                    }
                }
            }
            path.pop();
        }
    }
}

struct Metrics {
    a: f32,
    d: f32,
}

fn text_metrics(s: &ComputedStyle) -> (f32, f32, Metrics) {
    let m = text::metrics(key_of(s), s.font_size);
    let lh = s.line_height_px();
    let half = (lh - (m.ascent + m.descent)) / 2.0;
    let mut a = m.ascent + half;
    let d = lh - a;
    let shift = valign_shift(s);
    a += shift;
    (m.ascent, m.descent, Metrics { a, d: d - shift })
}

fn valign_shift(s: &ComputedStyle) -> f32 {
    match s.vertical_align {
        VAlign::Super => s.font_size * 0.4,
        VAlign::Sub => -s.font_size * 0.25,
        _ => 0.0,
    }
}

/// Lay out inline content of `b`; returns content height.
pub(crate) fn layout_inline(b: &mut LayoutBox, st: &mut State) -> f32 {
    let root_style = b.style.clone();
    let cx = b.content.x;
    let cw = b.content.w;
    let top = b.content.y;
    let mut children = std::mem::take(&mut b.children);
    let items = {
        let mut col = Collector { st, items: Vec::new(), prev_space: true, avail_w: cw };
        col.collect(&mut children, &mut Vec::new());
        col.items
    };

    let (_, _, strut) = text_metrics(&root_style);
    let strut_h = strut.a + strut.d;
    let indent = root_style.text_indent.or_zero(cw);
    let mut lines: Vec<LineBox> = Vec::new();
    let mut y = top;
    let mut i = 0;
    // Inline boxes still open across line breaks: (path, deco style, node)
    let mut open: Vec<(Vec<usize>, Rc<ComputedStyle>, Option<NodeId>)> = Vec::new();
    let n = items.len();

    while i < n {
        let first_line = lines.is_empty();
        let (mut l, mut r) = st.float_space(y, strut_h, cx, cx + cw);
        let ind = if first_line { indent } else { 0.0 };
        let mut avail = r - l - ind;
        let start = i;
        let mut w = 0.0;
        let mut has_content = false;
        let mut end = i;
        let mut ended_by_break = false;
        let mut skip_leading = true;
        let mut prev_is_space = false;
        while end < n {
            match &items[end] {
                Item::Word { w: iw, space, collapsible, soft_before, wrap, .. } => {
                    if *space {
                        if *collapsible && skip_leading {
                            end += 1;
                            continue;
                        }
                        w += iw;
                        prev_is_space = true;
                        end += 1;
                        continue;
                    }
                    let can_break = has_content && (prev_is_space || *soft_before) && *wrap;
                    if can_break && w + iw > avail + 0.01 && trailing_trimmed(&items[start..end], w) + iw > avail + 0.01 {
                        break;
                    }
                    w += iw;
                    has_content = true;
                    skip_leading = false;
                    prev_is_space = false;
                    end += 1;
                }
                Item::Atomic { w: iw, wrap, .. } => {
                    let can_break = has_content && prev_is_space && *wrap;
                    if can_break && trailing_trimmed(&items[start..end], w) + iw > avail + 0.01 {
                        break;
                    }
                    w += iw;
                    has_content = true;
                    skip_leading = false;
                    prev_is_space = false;
                    end += 1;
                }
                Item::Open { extra, .. } | Item::Close { extra, .. } => {
                    w += extra;
                    end += 1;
                }
                Item::Break => {
                    end += 1;
                    ended_by_break = true;
                    break;
                }
                Item::Float { path } => {
                    let fb = get_mut(&mut children, path);
                    block::place_float(fb, st, cx, cw, y, None);
                    let sp = st.float_space(y, strut_h, cx, cx + cw);
                    l = sp.0;
                    r = sp.1;
                    avail = r - l - ind;
                    end += 1;
                }
                Item::OutOfFlow { .. } => {
                    end += 1;
                }
            }
        }
        // Don't leave opening inline boxes dangling at the end of a line.
        if end < n && !ended_by_break {
            while end > start + 1 && matches!(items[end - 1], Item::Open { .. }) {
                end -= 1;
            }
        }
        if end == start {
            end = start + 1; // guarantee progress
        }

        let mut hidden = vec![false; end - start];
        // hide leading collapsible spaces and trailing ones (also those followed only by closes)
        {
            let mut seen_content = false;
            for k in start..end {
                match &items[k] {
                    Item::Word { space: true, collapsible: true, .. } if !seen_content => hidden[k - start] = true,
                    Item::Word { .. } | Item::Atomic { .. } => seen_content = true,
                    _ => {}
                }
            }
            for k in (start..end).rev() {
                match &items[k] {
                    Item::Word { space: true, collapsible: true, .. } => hidden[k - start] = true,
                    Item::Close { .. } | Item::Break | Item::OutOfFlow { .. } | Item::Float { .. } | Item::Open { .. } => {}
                    _ => break,
                }
            }
        }

        // ---- vertical metrics
        let mut la = strut.a;
        let mut ld = strut.d;
        let mut open_styles: Vec<Rc<ComputedStyle>> = open.iter().map(|o| o.1.clone()).collect();
        for s in &open_styles {
            let (_, _, m) = text_metrics(s);
            la = la.max(m.a);
            ld = ld.max(m.d);
        }
        let mut line_w = 0.0;
        let mut spaces = 0;
        for k in start..end {
            if hidden[k - start] {
                continue;
            }
            match &items[k] {
                Item::Word { style, w, space, .. } => {
                    let (_, _, m) = text_metrics(style);
                    la = la.max(m.a);
                    ld = ld.max(m.d);
                    line_w += w;
                    if *space {
                        spaces += 1;
                    }
                }
                Item::Atomic { w, h, base, valign, .. } => {
                    line_w += w;
                    match valign {
                        VAlign::Middle => {
                            let xh = root_style.font_size * 0.25;
                            la = la.max(h / 2.0 + xh);
                            ld = ld.max(h / 2.0 - xh);
                        }
                        VAlign::Top | VAlign::Bottom | VAlign::TextTop => {}
                        _ => {
                            la = la.max(*base);
                            ld = ld.max(h - base);
                        }
                    }
                }
                Item::Open { path, extra, .. } => {
                    line_w += extra;
                    let s = get(&children, path).style.clone();
                    let (_, _, m) = text_metrics(&s);
                    la = la.max(m.a);
                    ld = ld.max(m.d);
                    open_styles.push(s);
                }
                Item::Close { extra, .. } => line_w += extra,
                _ => {}
            }
        }
        // top/bottom aligned atomics may grow the line
        for k in start..end {
            if let Item::Atomic { h, valign: VAlign::Top | VAlign::Bottom | VAlign::TextTop, .. } = &items[k] {
                if la + ld < *h {
                    ld = h - la;
                }
            }
        }
        let line_h = la + ld;
        let baseline = y + la;

        // ---- horizontal alignment
        let room = r - l - ind;
        let is_last = end >= n || ended_by_break;
        let free = room - line_w;
        let (mut x, space_extra) = match root_style.text_align {
            TextAlign::Center | TextAlign::WebkitCenter => (l + ind + (free / 2.0).max(0.0), 0.0),
            TextAlign::Right => (l + ind + free.max(0.0), 0.0),
            TextAlign::Justify if !is_last && spaces > 0 && free > 0.0 => (l + ind, free / spaces as f32),
            _ => (l + ind, 0.0),
        };

        // ---- placement
        let mut lb = LineBox { rect: Rect::new(l, y, r - l, line_h), baseline, texts: vec![], decos: vec![] };
        // continuing inline boxes from the previous line
        let mut deco_stack: Vec<(usize, f32, Rc<ComputedStyle>)> = Vec::new();
        for (_, s, node) in &open {
            lb.decos.push(InlineDeco { rect: Rect::default(), style: s.clone(), node: *node, first: false, last: false });
            deco_stack.push((lb.decos.len() - 1, x, s.clone()));
        }
        for k in start..end {
            if hidden[k - start] {
                continue;
            }
            match &items[k] {
                Item::Word { text, w, style, node, space, .. } => {
                    let (asc, desc, _) = text_metrics(style);
                    if !*space || !style.white_space.collapses() || style.underline || style.line_through {
                        lb.texts.push(TextFrag {
                            x,
                            baseline: baseline - valign_shift(style),
                            w: *w,
                            ascent: asc,
                            descent: desc,
                            text: text.clone(),
                            style: style.clone(),
                            node: *node,
                        });
                    }
                    x += w;
                    if *space {
                        x += space_extra;
                    }
                }
                Item::Atomic { path, w, h, base, valign, .. } => {
                    let ab = get_mut(&mut children, path);
                    let mb = ab.margin_box();
                    let ty = match valign {
                        VAlign::Middle => baseline - root_style.font_size * 0.25 - h / 2.0,
                        VAlign::Top | VAlign::TextTop => y,
                        VAlign::Bottom => y + line_h - h,
                        _ => baseline - base,
                    };
                    ab.translate(x - mb.x, ty - mb.y);
                    x += w;
                }
                Item::Open { path, extra, ml } => {
                    let ib = get(&children, path);
                    lb.decos.push(InlineDeco { rect: Rect::default(), style: ib.style.clone(), node: ib.node, first: true, last: false });
                    deco_stack.push((lb.decos.len() - 1, x + ml, ib.style.clone()));
                    open.push((path.clone(), ib.style.clone(), ib.node));
                    x += extra;
                }
                Item::Close { extra, mr } => {
                    let right = x + extra - mr;
                    if let Some((di, sx, s)) = deco_stack.pop() {
                        lb.decos[di].rect = deco_rect(&s, sx, right, baseline);
                        lb.decos[di].last = true;
                    }
                    open.pop();
                    x += extra;
                }
                Item::OutOfFlow { path } => {
                    let ob = get_mut(&mut children, path);
                    ob.static_pos = (x, y);
                }
                Item::Break | Item::Float { .. } => {}
            }
        }
        // hidden items may include Opens/Closes? no: only spaces are hidden. Close remaining decos at line end.
        while let Some((di, sx, s)) = deco_stack.pop() {
            lb.decos[di].rect = deco_rect(&s, sx, x, baseline);
        }
        lines.push(lb);
        y += line_h;
        i = end;
    }
    b.children = children;
    // Remove empty lines produced solely by leading whitespace.
    let has_visible = lines.iter().any(|l| !l.texts.is_empty() || !l.decos.is_empty());
    let atomic_count = b.children.len();
    let _ = atomic_count;
    if !has_visible && lines.iter().all(|l| l.texts.is_empty()) && !items.iter().any(|it| matches!(it, Item::Atomic { .. } | Item::Break)) {
        b.lines.clear();
        return 0.0;
    }
    b.lines = lines;
    y - top
}

fn trailing_trimmed(items: &[Item], w: f32) -> f32 {
    let mut w = w;
    for it in items.iter().rev() {
        match it {
            Item::Word { space: true, collapsible: true, w: sw, .. } => w -= sw,
            Item::Close { .. } | Item::Open { .. } => {}
            _ => break,
        }
    }
    w
}

fn deco_rect(s: &ComputedStyle, x0: f32, x1: f32, baseline: f32) -> Rect {
    let m = text::metrics(key_of(s), s.font_size);
    let pt = s.padding[0].or_zero(0.0) + s.effective_border(0);
    let pb = s.padding[2].or_zero(0.0) + s.effective_border(2);
    let top = baseline - valign_shift(s) - m.ascent - pt;
    let bottom = baseline - valign_shift(s) + m.descent + pb;
    Rect::new(x0, top, (x1 - x0).max(0.0), bottom - top)
}
