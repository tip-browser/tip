//! Table layout (automatic algorithm, simplified: no rowspan).

use super::block::{self, Avail, WidthMode};
use super::{intrinsic, Inner, LayoutBox, State};
use crate::raster::Rect;
use crate::style::VAlign;

/// Paths to row boxes: (child index, Some(row index within group)).
fn rows_of(b: &LayoutBox) -> Vec<(usize, Option<usize>)> {
    let mut out = Vec::new();
    for (i, c) in b.children.iter().enumerate() {
        match c.inner {
            Inner::TableRow => out.push((i, None)),
            Inner::TableRowGroup => {
                for (j, r) in c.children.iter().enumerate() {
                    if r.inner == Inner::TableRow {
                        out.push((i, Some(j)));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn row<'a>(b: &'a LayoutBox, p: (usize, Option<usize>)) -> &'a LayoutBox {
    match p.1 {
        Some(j) => &b.children[p.0].children[j],
        None => &b.children[p.0],
    }
}

fn row_mut<'a>(b: &'a mut LayoutBox, p: (usize, Option<usize>)) -> &'a mut LayoutBox {
    match p.1 {
        Some(j) => &mut b.children[p.0].children[j],
        None => &mut b.children[p.0],
    }
}

fn spacing(b: &LayoutBox) -> f32 {
    if b.style.border_collapse { 0.0 } else { b.style.border_spacing }
}

fn ncols(b: &LayoutBox, rows: &[(usize, Option<usize>)]) -> usize {
    rows.iter().map(|&p| row(b, p).children.iter().map(|c| c.colspan).sum::<usize>()).max().unwrap_or(0)
}

/// Per-column (min, max) border-box widths.
fn column_metrics(b: &LayoutBox, st: &State) -> Vec<(f32, f32)> {
    let rows = rows_of(b);
    let n = ncols(b, &rows);
    let mut cols = vec![(0f32, 0f32); n];
    let sp = spacing(b);
    let mut spans: Vec<(usize, usize, f32, f32)> = Vec::new();
    for &p in &rows {
        let mut c = 0;
        for cell in &row(b, p).children {
            let (mn, mx) = intrinsic::intrinsic(cell, st);
            if cell.colspan == 1 {
                if c < n {
                    cols[c].0 = cols[c].0.max(mn);
                    cols[c].1 = cols[c].1.max(mx);
                }
            } else {
                spans.push((c, cell.colspan, mn, mx));
            }
            c += cell.colspan;
        }
    }
    for (start, span, mn, mx) in spans {
        let end = (start + span).min(n);
        if end <= start {
            continue;
        }
        let k = (end - start) as f32;
        let have_min: f32 = cols[start..end].iter().map(|c| c.0).sum::<f32>() + sp * (k - 1.0);
        let have_max: f32 = cols[start..end].iter().map(|c| c.1).sum::<f32>() + sp * (k - 1.0);
        if mn > have_min {
            let add = (mn - have_min) / k;
            for c in &mut cols[start..end] {
                c.0 += add;
            }
        }
        if mx > have_max {
            let add = (mx - have_max) / k;
            for c in &mut cols[start..end] {
                c.1 += add;
            }
        }
    }
    for c in &mut cols {
        c.1 = c.1.max(c.0);
    }
    cols
}

pub(crate) fn table_intrinsic(b: &LayoutBox, st: &State) -> (f32, f32) {
    let cols = column_metrics(b, st);
    let sp = spacing(b) * (cols.len() + 1) as f32;
    let mn: f32 = cols.iter().map(|c| c.0).sum::<f32>() + sp;
    let mx: f32 = cols.iter().map(|c| c.1).sum::<f32>() + sp;
    let cap = b.children.iter().filter(|c| c.inner == Inner::TableCaption).map(|c| intrinsic::outer(c, st).0).fold(0.0, f32::max);
    (mn.max(cap), mx.max(cap))
}

/// Content width of the table box.
pub(crate) fn table_width(b: &LayoutBox, st: &State, avail_outer: f32, _mode: WidthMode) -> f32 {
    let s = &b.style;
    let pb = block::resolve_edges(&s.padding, avail_outer).h() + block::border_edges(b).h();
    let (mn, mx) = table_intrinsic(b, st);
    match s.width.resolve(avail_outer) {
        Some(w) => {
            let w = if s.border_box { w - pb } else { w };
            w.max(mn)
        }
        None => mx.min((avail_outer - pb).max(0.0)).max(mn),
    }
}

fn shift_contents(b: &mut LayoutBox, dy: f32) {
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

/// In the collapsing border model adjacent cells share borders: overlap them by the cell border width.
fn collapse_overlap(b: &LayoutBox) -> f32 {
    if !b.style.border_collapse {
        return 0.0;
    }
    let mut w: f32 = 0.0;
    for &p in &rows_of(b) {
        for c in &row(b, p).children {
            w = w.max(c.style.effective_border(3)).max(c.style.effective_border(0));
        }
    }
    w
}

pub(crate) fn layout_table(b: &mut LayoutBox, st: &mut State, def_h: Option<f32>) -> f32 {
    let (cx, cy, cw) = (b.content.x, b.content.y, b.content.w);
    let overlap = collapse_overlap(b);
    let sp = if overlap > 0.0 { -overlap } else { spacing(b) };
    let edge = if overlap > 0.0 { 0.0 } else { sp };
    let cols = column_metrics(b, st);
    let n = cols.len();
    let avail = (cw - sp * n.saturating_sub(1) as f32 - edge * 2.0).max(0.0);
    let smin: f32 = cols.iter().map(|c| c.0).sum();
    let smax: f32 = cols.iter().map(|c| c.1).sum();
    let widths: Vec<f32> = if n == 0 {
        vec![]
    } else if smax <= avail {
        let extra = avail - smax;
        if smax > 0.0 {
            cols.iter().map(|c| c.1 + extra * c.1 / smax).collect()
        } else {
            vec![avail / n as f32; n]
        }
    } else if smin <= avail {
        let ratio = if smax > smin { (avail - smin) / (smax - smin) } else { 0.0 };
        cols.iter().map(|c| c.0 + (c.1 - c.0) * ratio).collect()
    } else {
        cols.iter().map(|c| c.0).collect()
    };
    let mut y = cy;
    // captions first
    for c in b.children.iter_mut().filter(|c| c.inner == Inner::TableCaption) {
        let a = Avail::fill(cw, None);
        block::layout_box(c, st, cx, y, &a);
        y = c.margin_box().bottom();
    }
    y += edge;
    let rows = rows_of(b);
    let mut group_bounds: Vec<(usize, Rect)> = Vec::new();
    for &p in &rows {
        let row_y = y;
        let r = row_mut(b, p);
        let row_style_h = r.style.height.resolve(0.0).unwrap_or(0.0);
        let mut x = cx + edge;
        let mut col = 0;
        let mut row_h: f32 = row_style_h;
        let mut cell_hs = Vec::new();
        for cell in &mut r.children {
            let span = cell.colspan.min(n.saturating_sub(col)).max(1);
            let w: f32 = widths.get(col..col + span).map(|s| s.iter().sum::<f32>()).unwrap_or(0.0) + sp * (span - 1) as f32;
            let a = Avail { cb_w: w, cb_h: None, mode: WidthMode::Exact(w), forced_h: None, bfc: true };
            block::layout_box(cell, st, x, row_y, &a);
            let h = cell.border_box().h;
            row_h = row_h.max(h);
            cell_hs.push(h);
            x += w + sp;
            col += span;
        }
        for (cell, h) in r.children.iter_mut().zip(cell_hs) {
            let extra = row_h - h;
            if extra > 0.0 {
                let off = match cell.style.vertical_align {
                    VAlign::Middle => extra / 2.0,
                    VAlign::Bottom => extra,
                    _ => 0.0,
                };
                cell.content.h += extra;
                shift_contents(cell, off);
            }
        }
        r.padding = Default::default();
        r.border = Default::default();
        r.margin = Default::default();
        r.content = Rect::new(cx, row_y, cw, row_h);
        let rb = r.content;
        match group_bounds.iter_mut().find(|g| g.0 == p.0 && p.1.is_some()) {
            Some(g) => g.1 = g.1.union(&rb),
            None if p.1.is_some() => group_bounds.push((p.0, rb)),
            None => {}
        }
        y += row_h + sp;
    }
    y += edge - sp;
    for (gi, rect) in group_bounds {
        let g = &mut b.children[gi];
        g.padding = Default::default();
        g.border = Default::default();
        g.margin = Default::default();
        g.content = rect;
    }
    let h = y - cy;
    def_h.map(|d| d.max(h)).unwrap_or(h)
}
