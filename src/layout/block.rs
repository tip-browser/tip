//! Block-level layout: sizing, normal flow, floats and positioning.

use super::{flex, grid, inline, intrinsic, table, FloatRect, Inner, LayoutBox, Replaced, State};
use crate::raster::Rect;
use crate::style::{Float, Len, Position};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum WidthMode {
    Fill,
    Shrink,
    /// Border-box width imposed by the parent (flex/grid/table).
    Exact(f32),
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Avail {
    pub cb_w: f32,
    pub cb_h: Option<f32>,
    pub mode: WidthMode,
    /// Border-box height imposed by the parent.
    pub forced_h: Option<f32>,
    pub bfc: bool,
}

impl Avail {
    pub fn fill(cb_w: f32, cb_h: Option<f32>) -> Avail {
        Avail { cb_w, cb_h, mode: WidthMode::Fill, forced_h: None, bfc: false }
    }
}

pub(crate) fn layout_block_level(b: &mut LayoutBox, st: &mut State, cb_w: f32, cb_h: Option<f32>, x: f32, y: f32, shrink: bool) {
    let mut a = Avail::fill(cb_w, cb_h);
    if shrink {
        a.mode = WidthMode::Shrink;
    }
    a.bfc = true;
    layout_box(b, st, x, y, &a);
}

pub(crate) fn resolve_edges(v: &[Len; 4], base: f32) -> super::Edges {
    super::Edges { top: v[0].or_zero(base), right: v[1].or_zero(base), bottom: v[2].or_zero(base), left: v[3].or_zero(base) }
}

pub(crate) fn border_edges(b: &LayoutBox) -> super::Edges {
    let s = &b.style;
    super::Edges { top: s.effective_border(0), right: s.effective_border(1), bottom: s.effective_border(2), left: s.effective_border(3) }
}

fn establishes_bfc(b: &LayoutBox) -> bool {
    b.style.overflow_y.clips()
        || b.is_float()
        || b.is_out_of_flow()
        || b.inline_level
        || matches!(b.inner, Inner::Flex | Inner::Grid | Inner::Table | Inner::TableCell | Inner::TableCaption | Inner::Replaced(_))
}

/// Layout a box with its margin-box top-left at (x, y).
/// Inputs that determine the layout of an independent formatting context.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct LayoutKey {
    cb_w: u32,
    cb_h: Option<u32>,
    mode: WidthMode,
    forced_h: Option<u32>,
    collapsed_top: bool,
    /// Border-box origin relative to the requested (x, y) after the last layout.
    dx: f32,
    dy: f32,
}

/// Lay out `b` at (x, y). Boxes laid out as independent formatting contexts (flex/grid items,
/// table cells, ...) are often laid out several times with the same constraints while their
/// parent measures them; those repeats reuse the previous result and are just moved.
pub(crate) fn layout_box(b: &mut LayoutBox, st: &mut State, x: f32, y: f32, a: &Avail) {
    let key = LayoutKey {
        cb_w: a.cb_w.to_bits(),
        cb_h: a.cb_h.map(f32::to_bits),
        mode: a.mode,
        forced_h: a.forced_h.map(f32::to_bits),
        collapsed_top: b.collapsed_top,
        dx: 0.0,
        dy: 0.0,
    };
    // move a previously laid-out box so its border box sits where a fresh layout would put it
    let place = |b: &mut LayoutBox, prev: &LayoutKey| {
        let bb = b.border_box();
        b.translate(x + prev.dx - bb.x, y + prev.dy - bb.y);
    };
    if a.bfc && !b.is_out_of_flow() {
        if let Some(prev) = b.layout_key {
            let same = LayoutKey { dx: prev.dx, dy: prev.dy, ..key } == prev;
            if same {
                place(b, &prev);
                return;
            }
            // only the imposed height changed (e.g. align-items: stretch) on a block whose
            // content doesn't depend on its height: resize without laying out again
            let only_height = LayoutKey { dx: prev.dx, dy: prev.dy, forced_h: prev.forced_h, ..key } == prev;
            if let (true, Some(fh)) = (only_height, a.forced_h) {
                if b.inner == Inner::Flow && !b.is_scroll_container() && !height_dependent(b) && !b.node.is_some_and(|n| st.doc.tag_is(n, "button")) {
                    place(b, &prev);
                    b.content.h = (fh - b.padding.v() - b.border.v()).max(0.0);
                    b.layout_key = Some(LayoutKey { dx: prev.dx, dy: prev.dy, ..key });
                    return;
                }
            }
        }
    }
    layout_box_uncached(b, st, x, y, a);
    b.layout_key = if a.bfc {
        let bb = b.border_box();
        Some(LayoutKey { dx: bb.x - x, dy: bb.y - y, ..key })
    } else {
        None
    };
}

/// Whether a block's in-flow children size themselves against its height.
fn height_dependent(b: &LayoutBox) -> bool {
    let pct = |l: &crate::style::Len| matches!(l, crate::style::Len::Val(_, p) if *p != 0.0);
    b.style.min_height.resolve(0.0).is_some() && pct(&b.style.min_height)
        || b.children.iter().any(|c| {
            let s = &c.style;
            !c.is_out_of_flow() && (pct(&s.height) || pct(&s.min_height) || pct(&s.max_height) || c.style.aspect_ratio.is_some() && s.width.is_auto())
        })
}

fn layout_box_uncached(b: &mut LayoutBox, st: &mut State, x: f32, y: f32, a: &Avail) {
    let style = b.style.clone();
    let cb_w = a.cb_w.max(0.0);
    b.padding = resolve_edges(&style.padding, cb_w);
    b.border = border_edges(b);
    let m = &style.margin;
    let mut margin = resolve_edges(m, cb_w);
    let pb_h = b.padding.h() + b.border.h();
    let pb_v = b.padding.v() + b.border.v();

    // ---- width
    let is_replaced = matches!(b.inner, Inner::Replaced(_));
    let mut replaced_h = None;
    let content_w = if style.width_kw != crate::style::SizeKw::None && !is_replaced && b.inner != Inner::Table {
        // `width: fit-content | max-content | min-content` wins over stretching by a grid/flex parent
        let (mn, mx) = intrinsic::intrinsic(b, st);
        let (mn, mx) = ((mn - pb_h).max(0.0), (mx - pb_h).max(0.0));
        let avail = match a.mode {
            WidthMode::Exact(bw) => (bw - pb_h).max(0.0),
            _ => (cb_w - margin.h() - pb_h).max(0.0),
        };
        match style.width_kw {
            crate::style::SizeKw::MaxContent => mx,
            crate::style::SizeKw::MinContent => mn,
            _ => mn.max(avail.min(mx)),
        }
    } else if let WidthMode::Exact(bw) = a.mode {
        if is_replaced {
            let (w, h) = replaced_size(b, st, cb_w, a.cb_h);
            let cw = (bw - pb_h).max(0.0);
            // images keep their aspect ratio when only the width was imposed
            let keeps_ratio = matches!(b.inner, Inner::Replaced(Replaced::Image) | Inner::Replaced(Replaced::Placeholder)) || style.aspect_ratio.is_some();
            replaced_h = Some(if keeps_ratio && style.height.is_auto() && w > 0.0 { h * cw / w } else { h });
        }
        (bw - pb_h).max(0.0)
    } else if is_replaced {
        let (w, h) = replaced_size(b, st, cb_w, a.cb_h);
        replaced_h = Some(h);
        w
    } else if b.inner == Inner::Table {
        table::table_width(b, st, cb_w - margin.h(), a.mode)
    } else {
        let specified = style.width.resolve(cb_w).map(|w| if style.border_box { w - pb_h } else { w });
        match specified {
            Some(w) => w,
            None => {
                let avail = (cb_w - margin.h() - pb_h).max(0.0);
                if a.mode == WidthMode::Shrink {
                    let (mn, mx) = intrinsic::intrinsic(b, st);
                    (mn - pb_h).max(avail.min(mx - pb_h)).max(0.0)
                } else {
                    avail
                }
            }
        }
    };
    let content_w = clamp_w(&style, content_w, cb_w, pb_h);

    // auto margins (only meaningful in Fill mode for block-level boxes)
    if a.mode == WidthMode::Fill && !b.inline_level && !b.is_float() {
        let used = content_w + pb_h + margin.h();
        let free = cb_w - used;
        let (la, ra) = (m[3].is_auto(), m[1].is_auto());
        if free > 0.0 {
            if la && ra {
                margin.left = free / 2.0;
                margin.right = free / 2.0;
            } else if la {
                margin.left = free;
            } else if ra {
                margin.right = free;
            } else if style.text_align == crate::style::TextAlign::WebkitCenter && margin.left == 0.0 && margin.right == 0.0 {
                // legacy centring inherited from <center> / align=center
                margin.left = free / 2.0;
                margin.right = free / 2.0;
            }
        }
    }

    // ---- parent/first-child top margin collapsing
    let bfc = a.bfc || establishes_bfc(b);
    if !bfc && b.inner == Inner::Flow && b.padding.top == 0.0 && b.border.top == 0.0 && !has_inline_content(b) {
        if let Some(first) = b.children.iter_mut().find(|c| !c.is_out_of_flow() && !c.is_float() && !phantom_block(c)) {
            if !first.inline_level && first.inner == Inner::Flow && !establishes_bfc(first) {
                let fm = first.style.margin[0].or_zero(content_w);
                if fm > 0.0 && margin.top >= 0.0 {
                    margin.top = margin.top.max(fm);
                    first.collapsed_top = true;
                }
            }
        }
    }
    b.margin = margin;
    let mt = if b.collapsed_top { 0.0 } else { margin.top };
    b.content = Rect::new(x + margin.left + b.border.left + b.padding.left, y + mt + b.border.top + b.padding.top, content_w, 0.0);

    // ---- definite height (for percentage resolution by children)
    let specified_h = match a.forced_h {
        Some(fh) => Some((fh - pb_v).max(0.0)),
        None => style.height.resolve_opt(a.cb_h).map(|h| if style.border_box { (h - pb_v).max(0.0) } else { h }),
    };
    let specified_h = specified_h.or_else(|| {
        if replaced_h.is_none() {
            style.aspect_ratio.map(|r| content_w / r.max(0.01))
        } else {
            None
        }
    });
    let def_h = specified_h.map(|h| clamp_h(&style, h, a.cb_h, pb_v));
    // min-height lets flex/grid containers distribute free space (e.g. vertical centring)
    let min_h = if def_h.is_none() {
        style.min_height.resolve_opt(a.cb_h).map(|h| if style.border_box { h - pb_v } else { h }).filter(|h| *h > 0.0)
    } else {
        None
    };

    // ---- fieldset: the first <legend> sits on the top border
    b.legend_gap = None;
    let mut legend: Option<(usize, LayoutBox)> = None;
    if matches!(b.inner, Inner::Flow | Inner::Grid | Inner::Flex) && b.node.is_some_and(|n| st.doc.tag_is(n, "fieldset")) {
        if let Some(i) = b.children.iter().position(|c| !c.is_float() && !c.is_out_of_flow() && c.node.is_some_and(|n| st.doc.tag_is(n, "legend"))) {
            let mut lg = b.children.remove(i);
            let border_top_y = b.content.y - b.padding.top - b.border.top;
            let la = Avail { cb_w: content_w, cb_h: None, mode: WidthMode::Shrink, forced_h: None, bfc: true };
            layout_box(&mut lg, st, b.content.x, border_top_y, &la);
            let lh = lg.margin_box().h;
            let bt = b.border.top;
            if lh < bt {
                lg.translate(0.0, (bt - lh) / 2.0);
            }
            let extra = (lh - bt).max(0.0);
            b.padding.top += extra;
            b.content.y += extra;
            let lb = lg.border_box();
            let bx = b.content.x - b.padding.left - b.border.left;
            b.legend_gap = Some((lb.x - bx, lb.right() - bx, ((lh - bt) / 2.0).max(0.0)));
            legend = Some((i, lg));
        }
    }

    // ---- content
    if bfc {
        st.floats.push(vec![]);
    }
    let content_h = match &b.inner {
        Inner::Replaced(_) => replaced_h.unwrap_or(0.0),
        Inner::Flex => flex::layout_flex(b, st, def_h, min_h, a),
        Inner::Grid => grid::layout_grid(b, st, def_h, min_h),
        Inner::Table => table::layout_table(b, st, def_h),
        _ => {
            if has_inline_content(b) {
                inline::layout_inline(b, st, def_h)
            } else {
                layout_flow_children(b, st, def_h, bfc)
            }
        }
    };
    let mut h = def_h.unwrap_or(content_h);
    if bfc {
        let fs = st.floats.pop().unwrap_or_default();
        if def_h.is_none() {
            for f in fs {
                h = h.max(f.rect.bottom() - b.content.y);
            }
        }
    }
    if def_h.is_none() {
        h = clamp_h(&style, h, a.cb_h, pb_v);
    }
    // buttons centre their content vertically
    if h > content_h + 0.5 && b.node.is_some_and(|n| st.doc.tag_is(n, "button")) && !matches!(b.inner, Inner::Flex | Inner::Grid) {
        let dy = ((h - content_h) / 2.0).floor();
        for l in &mut b.lines {
            l.rect.y += dy;
            l.baseline += dy;
            for t in &mut l.texts {
                t.baseline += dy;
            }
            for d in &mut l.decos {
                d.rect.y += dy;
            }
        }
        for c in &mut b.children {
            c.translate(0.0, dy);
        }
    }
    b.content.h = h.max(0.0);
    if let Some((i, lg)) = legend {
        let i = i.min(b.children.len());
        b.children.insert(i, lg);
    }

    // ---- scroll containers
    if b.is_scroll_container() {
        let pbx = b.padding_box();
        let mut ext = (pbx.right(), pbx.bottom());
        for c in &b.children {
            let e = extent_of(c);
            ext.0 = ext.0.max(e.0 + b.padding.right);
            ext.1 = ext.1.max(e.1 + b.padding.bottom);
        }
        for l in &b.lines {
            ext.0 = ext.0.max(l.rect.right() + b.padding.right);
            ext.1 = ext.1.max(l.rect.bottom() + b.padding.bottom);
        }
        b.scroll_max = ((ext.0 - pbx.right()).max(0.0), (ext.1 - pbx.bottom()).max(0.0));
        if let Some(n) = b.node {
            let (sx, sy) = st.env.scroll_offset(n);
            let sx = sx.clamp(0.0, b.scroll_max.0);
            let sy = sy.clamp(0.0, b.scroll_max.1);
            b.scroll = (sx, sy);
            if sx != 0.0 || sy != 0.0 {
                for l in &mut b.lines {
                    l.rect.x -= sx;
                    l.rect.y -= sy;
                    l.baseline -= sy;
                    for t in &mut l.texts {
                        t.x -= sx;
                        t.baseline -= sy;
                    }
                    for d in &mut l.decos {
                        d.rect.x -= sx;
                        d.rect.y -= sy;
                    }
                }
                for c in &mut b.children {
                    c.translate(-sx, -sy);
                }
            }
        }
    }

    // ---- relative positioning and translation
    let mut dx = 0.0;
    let mut dy = 0.0;
    if style.position == Position::Relative {
        let ch = a.cb_h.unwrap_or(0.0);
        dx = style.inset[3].resolve(cb_w).or_else(|| style.inset[1].resolve(cb_w).map(|v| -v)).unwrap_or(0.0);
        dy = style.inset[0].resolve(ch).or_else(|| style.inset[2].resolve(ch).map(|v| -v)).unwrap_or(0.0);
    }
    let bb = b.border_box();
    dx += style.translate.0.or_zero(bb.w);
    dy += style.translate.1.or_zero(bb.h);
    if dx != 0.0 || dy != 0.0 {
        b.translate(dx, dy);
    }
}

fn extent_of(b: &LayoutBox) -> (f32, f32) {
    if b.is_out_of_flow() {
        return (f32::MIN, f32::MIN);
    }
    // text and inline boxes are positioned through their line boxes, not their own rects
    let mut e = match b.inner {
        Inner::Text(_) | Inner::Break => return (f32::MIN, f32::MIN),
        Inner::InlineFlow => (f32::MIN, f32::MIN),
        _ => {
            let mb = b.margin_box();
            (mb.right(), mb.bottom())
        }
    };
    if !b.is_scroll_container() {
        for c in &b.children {
            let ce = extent_of(c);
            e.0 = e.0.max(ce.0);
            e.1 = e.1.max(ce.1);
        }
        for l in &b.lines {
            e.0 = e.0.max(l.rect.right());
            e.1 = e.1.max(l.rect.bottom());
        }
    }
    e
}

pub(crate) fn clamp_w(style: &crate::style::ComputedStyle, w: f32, cb_w: f32, pb: f32) -> f32 {
    let adj = |v: f32| if style.border_box { v - pb } else { v };
    let mut w = w;
    if let Some(mx) = style.max_width.resolve(cb_w) {
        w = w.min(adj(mx));
    }
    if let Some(mn) = style.min_width.resolve(cb_w) {
        w = w.max(adj(mn));
    }
    w.max(0.0)
}

pub(crate) fn clamp_h(style: &crate::style::ComputedStyle, h: f32, cb_h: Option<f32>, pb: f32) -> f32 {
    let adj = |v: f32| if style.border_box { v - pb } else { v };
    let mut h = h;
    if let Some(mx) = style.max_height.resolve_opt(cb_h) {
        h = h.min(adj(mx));
    }
    if let Some(mn) = style.min_height.resolve_opt(cb_h) {
        h = h.max(adj(mn));
    }
    h.max(0.0)
}

pub(crate) fn has_inline_content(b: &LayoutBox) -> bool {
    b.children.iter().any(|c| c.inline_level && !c.is_out_of_flow() && !c.is_float())
}

/// Normal flow of block-level children. Returns content height.
/// Adjoining margins collapse: the largest positive plus the most negative.
fn collapse_margins(a: f32, b: f32) -> f32 {
    a.max(b).max(0.0) + a.min(b).min(0.0)
}

/// An anonymous block holding only empty inline boxes and collapsible whitespace:
/// it produces a zero-height phantom line and doesn't separate margins.
pub(crate) fn phantom_block(b: &LayoutBox) -> bool {
    fn inline_empty(c: &LayoutBox) -> bool {
        match &c.inner {
            Inner::Text(t) => t.chars().all(|ch| matches!(ch, ' ' | '\t' | '\n' | '\r')) && c.style.white_space.collapses(),
            Inner::InlineFlow => {
                let s = &c.style;
                let zero = |l: &crate::style::Len| l.resolve(0.0).unwrap_or(0.0) == 0.0;
                zero(&s.padding[1]) && zero(&s.padding[3]) && zero(&s.margin[1]) && zero(&s.margin[3]) && s.effective_border(1) == 0.0 && s.effective_border(3) == 0.0 && c.children.iter().all(inline_empty)
            }
            _ => c.is_out_of_flow(),
        }
    }
    b.node.is_none() && b.inner == Inner::Flow && !b.children.is_empty() && b.children.iter().all(|c| c.inline_level || c.is_out_of_flow()) && b.children.iter().all(inline_empty)
}

fn layout_flow_children(b: &mut LayoutBox, st: &mut State, def_h: Option<f32>, bfc: bool) -> f32 {
    let cx = b.content.x;
    let cw = b.content.w;
    let top = b.content.y;
    let mut cursor = top;
    let mut prev_mb: f32 = 0.0;
    let mut have_prev = false;
    let n = b.children.len();
    for i in 0..n {
        let child = &mut b.children[i];
        if child.is_out_of_flow() {
            child.static_pos = (cx, cursor + if have_prev { prev_mb } else { 0.0 });
            continue;
        }
        if child.is_float() {
            place_float(child, st, cx, cw, cursor + if have_prev { prev_mb.max(0.0) } else { 0.0 }, def_h);
            continue;
        }
        let mt = if child.collapsed_top { 0.0 } else { child.style.margin[0].or_zero(cw) };
        if child.style.clear {
            let fb = st.float_bottom(true, true);
            if fb > cursor + prev_mb.max(mt) {
                cursor = fb;
                prev_mb = 0.0;
                have_prev = false;
            }
        }
        let gap = if have_prev {
            if prev_mb >= 0.0 && mt >= 0.0 {
                prev_mb.max(mt)
            } else if prev_mb < 0.0 && mt < 0.0 {
                prev_mb.min(mt)
            } else {
                prev_mb + mt
            }
        } else {
            mt
        };
        let border_top_y = cursor + gap;
        let mut avail = Avail::fill(cw, def_h);
        let mut x = cx;
        if establishes_bfc(child) {
            // Avoid floats.
            let (l, r) = st.float_space(border_top_y, 1.0, cx, cx + cw);
            if l > cx || r < cx + cw {
                avail.cb_w = r - l;
                x = l;
            }
        }
        layout_box(child, st, x, border_top_y - mt, &avail);
        // An empty box (no height, padding or border) lets margins collapse through it.
        let bb = child.border_box();
        if bb.h < 0.01 && child.padding.top == 0.0 && child.padding.bottom == 0.0 && child.border.top == 0.0 && child.border.bottom == 0.0 && !establishes_bfc(child) && !child.style.clear {
            let through = collapse_margins(gap, child.margin.bottom);
            if have_prev {
                // the gap already includes prev_mb; the cursor stays where the previous box ended
                prev_mb = through;
            } else {
                prev_mb = through;
                have_prev = true;
            }
            continue;
        }
        cursor = bb.bottom();
        prev_mb = child.margin.bottom;
        have_prev = true;
    }
    // bottom margin collapsing with the parent
    let collapse_bottom = !bfc && b.padding.bottom == 0.0 && b.border.bottom == 0.0 && def_h.is_none() && b.style.height.is_auto();
    if have_prev {
        if collapse_bottom && prev_mb > 0.0 {
            b.margin.bottom = b.margin.bottom.max(prev_mb);
        } else {
            cursor += prev_mb;
        }
    }
    let _ = bfc;
    (cursor - top).max(0.0)
}

/// Lay out and place a floated box whose top edge must be at or below `y`.
pub(crate) fn place_float(child: &mut LayoutBox, st: &mut State, cx: f32, cw: f32, y: f32, cb_h: Option<f32>) {
    let mut a = Avail::fill(cw, cb_h);
    a.mode = WidthMode::Shrink;
    a.bfc = true;
    layout_box(child, st, 0.0, 0.0, &a);
    let mb = child.margin_box();
    let (w, h) = (mb.w, mb.h);
    let left = child.style.float == Float::Left;
    let mut fy = y;
    if child.style.clear {
        fy = fy.max(st.float_bottom(true, true));
    }
    for _ in 0..200 {
        let (l, r) = st.float_space(fy, h, cx, cx + cw);
        if r - l >= w - 0.01 || (l <= cx && r >= cx + cw) {
            let fx = if left { l } else { r - w };
            child.translate(fx - mb.x, fy - mb.y);
            if let Some(fs) = st.floats.last_mut() {
                fs.push(FloatRect { rect: Rect::new(fx, fy, w, h), left });
            }
            return;
        }
        // move below the highest float bottom overlapping fy
        let mut next = f32::MAX;
        if let Some(fs) = st.floats.last() {
            for f in fs {
                if f.rect.y <= fy + h && f.rect.bottom() > fy {
                    next = next.min(f.rect.bottom());
                }
            }
        }
        if next == f32::MAX || next <= fy {
            break;
        }
        fy = next;
    }
    let fx = if left { cx } else { cx + cw - w };
    child.translate(fx - mb.x, fy - mb.y);
    if let Some(fs) = st.floats.last_mut() {
        fs.push(FloatRect { rect: Rect::new(fx, fy, w, h), left });
    }
}

/// Content-box size of a replaced element.
pub(crate) fn replaced_size(b: &LayoutBox, st: &State, cb_w: f32, cb_h: Option<f32>) -> (f32, f32) {
    let s = &b.style;
    let fs = s.font_size;
    let lh = (fs * 1.2).round();
    let pb_h = b.padding.h() + b.border.h();
    let pb_v = b.padding.v() + b.border.v();
    let node = b.node.unwrap_or(0);
    let key = crate::text::FontKey::new(s.font_family, s.font_weight, s.italic);
    let (iw, ih, has_ratio) = match &b.inner {
        Inner::Replaced(Replaced::Image) => match st.env.image_size(node) {
            Some((w, h)) => (w, h, true),
            None => {
                let alt = st.doc.attr(node, "alt").unwrap_or("");
                if alt.is_empty() {
                    (0.0, 0.0, false)
                } else {
                    (crate::text::measure(key, fs, alt, 0.0) + 4.0, lh, false)
                }
            }
        },
        Inner::Replaced(Replaced::TextInput) => {
            let size = st.doc.attr(node, "size").and_then(|v| v.parse::<f32>().ok());
            (size.map(|n| n * fs * 0.55).unwrap_or(fs * 11.25).round(), lh, false)
        }
        Inner::Replaced(Replaced::TextArea) => {
            let cols = st.doc.attr(node, "cols").and_then(|v| v.parse::<f32>().ok()).unwrap_or(20.0);
            let rows = st.doc.attr(node, "rows").and_then(|v| v.parse::<f32>().ok()).unwrap_or(2.0);
            let cw = crate::text::measure(key, fs, "0", 0.0);
            (cols * cw, rows * lh, false)
        }
        Inner::Replaced(Replaced::Checkbox) | Inner::Replaced(Replaced::Radio) => (13.0, 13.0, false),
        Inner::Replaced(Replaced::Range) => (129.0, 16.0, false),
        Inner::Replaced(Replaced::Button(t)) => (crate::text::measure(key, fs, t, 0.0), lh, false),
        Inner::Replaced(Replaced::Select) => {
            let mut w: f32 = 0.0;
            for o in st.doc.elements_by_tag(node, "option") {
                let t = crate::dom::collapse_ws(&st.doc.text_content(o));
                w = w.max(crate::text::measure(key, fs, &t, 0.0));
            }
            (w.max(20.0), lh, false)
        }
        _ => {
            let tag = st.doc.tag(node).unwrap_or("");
            match tag {
                "progress" | "meter" => (160.0, 16.0, false),
                "svg" => {
                    let (w, h) = crate::svg::intrinsic_size(st.doc, node, s.font_size);
                    let sized = st.doc.attr(node, "width").is_some_and(|v| !v.trim().ends_with('%')) || st.doc.attr(node, "height").is_some_and(|v| !v.trim().ends_with('%'));
                    let avail = cb_w - pb_h - b.margin.h();
                    if !sized && avail > 0.0 {
                        // no intrinsic size, only a ratio: fill the available width
                        let ratio = if h > 0.0 { w / h } else { 2.0 };
                        (avail, avail / ratio, true)
                    } else {
                        (w, h, true)
                    }
                }
                _ => (300.0, 150.0, true),
            }
        }
    };
    let sw = s.width.resolve(cb_w).map(|w| if s.border_box { (w - pb_h).max(0.0) } else { w });
    let sh = s.height.resolve_opt(cb_h).map(|h| if s.border_box { (h - pb_v).max(0.0) } else { h });
    let ratio = if has_ratio && ih > 0.0 { Some(iw / ih) } else { s.aspect_ratio };
    let (mut w, mut h) = match (sw, sh) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, ratio.map(|r| w / r).unwrap_or(ih)),
        (None, Some(h)) => (ratio.map(|r| h * r).unwrap_or(iw), h),
        (None, None) => (iw, ih),
    };
    let w0 = w;
    w = clamp_w(s, w, cb_w, pb_h);
    if w != w0 && sh.is_none() {
        if let Some(r) = ratio {
            h = w / r;
        }
    }
    let h0 = h;
    h = clamp_h(s, h, cb_h, pb_v);
    if h != h0 && sw.is_none() {
        if let Some(r) = ratio {
            w = h * r;
        }
    }
    (w.max(0.0), h.max(0.0))
}

/// Lay out absolutely/fixed positioned boxes, recursively.
pub(crate) fn layout_positioned(b: &mut LayoutBox, st: &mut State, cb: Rect) {
    let vp = Rect::new(0.0, 0.0, st.vw, st.vh);
    for c in &mut b.children {
        if c.is_out_of_flow() {
            let r = if c.style.position == Position::Fixed { vp } else { cb };
            layout_abs(c, st, r);
        }
        let next = if super::is_positioned(c) { c.padding_box() } else { cb };
        layout_positioned(c, st, next);
    }
}

fn layout_abs(b: &mut LayoutBox, st: &mut State, cb: Rect) {
    let s = b.style.clone();
    let left = s.inset[3].resolve(cb.w);
    let right = s.inset[1].resolve(cb.w);
    let top = s.inset[0].resolve(cb.h);
    let bottom = s.inset[2].resolve(cb.h);
    let ml = s.margin[3].or_zero(cb.w);
    let mr = s.margin[1].or_zero(cb.w);
    let mt = s.margin[0].or_zero(cb.w);
    let mb = s.margin[2].or_zero(cb.w);
    let mut a = Avail { cb_w: cb.w, cb_h: Some(cb.h), mode: WidthMode::Fill, forced_h: None, bfc: true };
    if s.width.is_auto() {
        match (left, right) {
            (Some(l), Some(r)) => a.mode = WidthMode::Exact((cb.w - l - r - ml - mr).max(0.0)),
            _ => {
                a.mode = WidthMode::Shrink;
                a.cb_w = (cb.w - left.unwrap_or(0.0) - right.unwrap_or(0.0)).max(0.0);
            }
        }
    }
    if s.height.is_auto() {
        if let (Some(t), Some(bm)) = (top, bottom) {
            a.forced_h = Some((cb.h - t - bm - mt - mb).max(0.0));
        }
    }
    let (sx, sy) = b.static_pos;
    let saved_cb_w = a.cb_w;
    if let WidthMode::Shrink = a.mode {
        a.cb_w = saved_cb_w;
    }
    layout_box(b, st, 0.0, 0.0, &a);
    // percentages in margins were resolved against a.cb_w; fine for our purposes
    let m = b.margin_box();
    let x = match (left, right) {
        (Some(l), _) => cb.x + l,
        (None, Some(r)) => cb.right() - r - m.w,
        _ => sx,
    };
    let y = match (top, bottom) {
        (Some(t), _) => cb.y + t,
        (None, Some(bt)) => cb.bottom() - bt - m.h,
        _ => sy,
    };
    b.translate(x - m.x, y - m.y);
    if s.position == Position::Fixed {
        b.fixed = true;
    }
}
