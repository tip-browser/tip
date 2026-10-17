//! Intrinsic (min-content / max-content) widths.

use super::{block, Inner, LayoutBox, State};
use crate::style::{GridTemplate, Track, WhiteSpace};
use crate::text::{self, FontKey};

/// (min-content, max-content) border-box widths, excluding margins.
pub(crate) fn intrinsic(b: &LayoutBox, st: &State) -> (f32, f32) {
    if let Some(v) = b.intrinsic.get() {
        return v;
    }
    let v = compute(b, st);
    b.intrinsic.set(Some(v));
    v
}

pub(crate) fn px_edges(b: &LayoutBox) -> f32 {
    let s = &b.style;
    let pad = s.padding[1].resolve(0.0).unwrap_or(0.0) + s.padding[3].resolve(0.0).unwrap_or(0.0);
    pad + s.effective_border(1) + s.effective_border(3)
}

/// Outer (margin-box) contribution of a child.
pub(crate) fn outer(b: &LayoutBox, st: &State) -> (f32, f32) {
    let (mn, mx) = intrinsic(b, st);
    let m = b.style.margin[1].resolve(0.0).unwrap_or(0.0) + b.style.margin[3].resolve(0.0).unwrap_or(0.0);
    (mn + m, mx + m)
}

fn compute(b: &LayoutBox, st: &State) -> (f32, f32) {
    let s = &b.style;
    let edges = px_edges(b);
    if let crate::style::Len::Val(px, 0.0) = s.width {
        let w = if s.border_box { px } else { px + edges };
        if !matches!(b.inner, Inner::Table) {
            return (w, w);
        }
    }
    let (mut mn, mut mx) = match &b.inner {
        Inner::Replaced(_) => {
            let (w, _) = block::replaced_size(b, st, 0.0, None);
            if s.width.resolve(100.0) != s.width.resolve(0.0) {
                // percentage width: min-content contribution is zero
                (0.0, w + edges)
            } else {
                (w + edges, w + edges)
            }
        }
        Inner::Text(t) => {
            let (a, b2) = text_intrinsic(t, b);
            (a, b2)
        }
        Inner::Break => (0.0, 0.0),
        Inner::Flex => {
            let mut mn: f32 = 0.0;
            let mut mx: f32 = 0.0;
            let items: Vec<&LayoutBox> = b.children.iter().filter(|c| !c.is_out_of_flow()).collect();
            let gap = s.column_gap;
            if s.flex_direction.is_row() {
                for (i, c) in items.iter().enumerate() {
                    let (a, z) = outer(c, st);
                    let g = if i > 0 { gap } else { 0.0 };
                    mx += z + g;
                    if s.flex_wrap {
                        mn = mn.max(a);
                    } else {
                        mn += a + g;
                    }
                }
            } else {
                for c in items {
                    let (a, z) = outer(c, st);
                    mn = mn.max(a);
                    mx = mx.max(z);
                }
            }
            (mn + edges, mx + edges)
        }
        Inner::Grid => {
            let items: Vec<&LayoutBox> = b.children.iter().filter(|c| !c.is_out_of_flow()).collect();
            let ncols = match &s.grid_columns {
                GridTemplate::Tracks(t) => t.len().max(1),
                _ => 1,
            };
            let fixed: Option<f32> = match &s.grid_columns {
                GridTemplate::Tracks(t) if t.iter().all(|x| matches!(x, Track::Px(_))) => {
                    Some(t.iter().map(|x| if let Track::Px(p) = x { *p } else { 0.0 }).sum::<f32>() + s.column_gap * (t.len().saturating_sub(1)) as f32)
                }
                _ => None,
            };
            let mut cmn: f32 = 0.0;
            let mut cmx: f32 = 0.0;
            for c in items {
                let (a, z) = outer(c, st);
                cmn = cmn.max(a);
                cmx = cmx.max(z);
            }
            let gaps = s.column_gap * (ncols.saturating_sub(1)) as f32;
            match fixed {
                Some(f) => (f + edges, f + edges),
                None => (cmn * ncols as f32 + gaps + edges, cmx * ncols as f32 + gaps + edges),
            }
        }
        Inner::Table => {
            let (a, z) = super::table::table_intrinsic(b, st);
            (a + edges, z + edges)
        }
        _ => {
            if block::has_inline_content(b) || b.inner == Inner::InlineFlow {
                let (a, z) = inline_intrinsic(&b.children, st);
                (a + edges, z + edges)
            } else {
                let mut mn: f32 = 0.0;
                let mut mx: f32 = 0.0;
                for c in &b.children {
                    if c.is_out_of_flow() {
                        continue;
                    }
                    let (a, z) = outer(c, st);
                    mn = mn.max(a);
                    mx = mx.max(z);
                }
                // floats side by side
                let float_sum: f32 = b.children.iter().filter(|c| c.is_float()).map(|c| outer(c, st).1).sum();
                mx = mx.max(float_sum);
                (mn + edges, mx + edges)
            }
        }
    };
    if let Some(m) = s.min_width.resolve(0.0).filter(|_| matches!(s.min_width, crate::style::Len::Val(_, 0.0))) {
        let m = if s.border_box { m } else { m + edges };
        mn = mn.max(m);
        mx = mx.max(m);
    }
    if let crate::style::Len::Val(px, 0.0) = s.max_width {
        let m = if s.border_box { px } else { px + edges };
        mn = mn.min(m);
        mx = mx.min(m);
    }
    (mn, mx.max(mn))
}

fn text_intrinsic(t: &str, b: &LayoutBox) -> (f32, f32) {
    let s = &b.style;
    let key = FontKey::new(s.font_family, s.font_weight, s.italic);
    let ls = s.letter_spacing;
    match s.white_space {
        WhiteSpace::Pre | WhiteSpace::NoWrap => {
            let lines: Vec<String> = if s.white_space == WhiteSpace::Pre {
                t.split('\n').map(|l| l.replace('\t', "        ")).collect()
            } else {
                vec![crate::dom::collapse_ws(t)]
            };
            let w = lines.iter().map(|l| text::measure(key, s.font_size, l, ls)).fold(0.0, f32::max);
            (w, w)
        }
        _ => {
            let mut mn: f32 = 0.0;
            for w in t.split_whitespace() {
                mn = mn.max(text::measure(key, s.font_size, w, ls));
            }
            let collapsed = crate::dom::collapse_ws(t);
            (mn, text::measure(key, s.font_size, &collapsed, ls))
        }
    }
}

/// Intrinsic widths of a run of inline-level content.
pub(crate) fn inline_intrinsic(children: &[LayoutBox], st: &State) -> (f32, f32) {
    let mut acc = InlineAcc { min: 0.0, max: 0.0, line: 0.0, prev_space: true, word: 0.0 };
    walk(children, st, &mut acc);
    acc.end_line();
    (acc.min, acc.max)
}

struct InlineAcc {
    min: f32,
    max: f32,
    line: f32,
    prev_space: bool,
    /// width of the current unbreakable run
    word: f32,
}

impl InlineAcc {
    fn end_line(&mut self) {
        self.max = self.max.max(self.line);
        self.min = self.min.max(self.word);
        self.line = 0.0;
        self.word = 0.0;
    }
}

fn walk(children: &[LayoutBox], st: &State, acc: &mut InlineAcc) {
    for c in children {
        if c.is_out_of_flow() {
            continue;
        }
        match &c.inner {
            Inner::Text(t) => {
                let s = &c.style;
                let key = FontKey::new(s.font_family, s.font_weight, s.italic);
                let space_w = text::measure(key, s.font_size, " ", s.letter_spacing) + s.word_spacing;
                match s.white_space {
                    WhiteSpace::Pre | WhiteSpace::PreWrap => {
                        for (i, line) in t.split('\n').enumerate() {
                            if i > 0 {
                                acc.end_line();
                            }
                            let w = text::measure(key, s.font_size, &line.replace('\t', "        "), s.letter_spacing);
                            acc.line += w;
                            if s.white_space == WhiteSpace::Pre {
                                acc.word += w;
                            } else {
                                for word in line.split(' ') {
                                    let ww = text::measure(key, s.font_size, word, s.letter_spacing);
                                    acc.min = acc.min.max(ww);
                                }
                            }
                        }
                    }
                    _ => {
                        let nowrap = s.white_space == WhiteSpace::NoWrap;
                        let mut cur = String::new();
                        let flush = |cur: &mut String, acc: &mut InlineAcc| {
                            if !cur.is_empty() {
                                let w = text::measure(key, s.font_size, cur, s.letter_spacing);
                                acc.line += w;
                                acc.word += w;
                                acc.prev_space = false;
                                cur.clear();
                            }
                        };
                        for ch in t.chars() {
                            if ch.is_ascii_whitespace() {
                                flush(&mut cur, acc);
                                if !acc.prev_space {
                                    acc.line += space_w;
                                    acc.prev_space = true;
                                    if nowrap {
                                        acc.word += space_w;
                                    } else {
                                        acc.min = acc.min.max(acc.word);
                                        acc.word = 0.0;
                                    }
                                }
                            } else {
                                cur.push(ch);
                            }
                        }
                        flush(&mut cur, acc);
                    }
                }
            }
            Inner::Break => acc.end_line(),
            Inner::InlineFlow => {
                let s = &c.style;
                let l = s.padding[3].or_zero(0.0) + s.effective_border(3) + s.margin[3].or_zero(0.0);
                let r = s.padding[1].or_zero(0.0) + s.effective_border(1) + s.margin[1].or_zero(0.0);
                acc.line += l;
                acc.word += l;
                walk(&c.children, st, acc);
                acc.line += r;
                acc.word += r;
            }
            _ => {
                let (mn, mx) = outer(c, st);
                acc.line += mx;
                if acc.prev_space || c.is_float() {
                    acc.min = acc.min.max(acc.word);
                    acc.word = mn;
                } else {
                    acc.word += mn;
                }
                acc.prev_space = false;
            }
        }
    }
}
