//! A practical subset of CSS Grid: explicit column/row templates, fr units,
//! auto-fill, auto-placement and spans.

use super::block::{self, Avail, WidthMode};
use super::{intrinsic, LayoutBox, State};
use crate::style::{Align, GridTemplate, Track};

/// Resolve lengths that depend on the grid's size (`min(100%, 14rem)`) against `base`;
/// with an indefinite base they behave as `auto`.
fn resolve_track(t: &Track, base: Option<f32>) -> Track {
    match t {
        Track::Len(l) => match l.resolve_opt(base) {
            Some(px) => Track::Px(px.max(0.0)),
            None => Track::Auto,
        },
        Track::MinMax(a, b) => Track::MinMax(Box::new(resolve_track(a, base)), Box::new(resolve_track(b, base))),
        other => other.clone(),
    }
}

fn expand(t: &GridTemplate, avail: Option<f32>, gap: f32) -> Vec<Track> {
    match t {
        GridTemplate::None => vec![],
        GridTemplate::Tracks(v) => v.iter().map(|t| resolve_track(t, avail)).collect(),
        GridTemplate::AutoFill { min, max } => {
            let min = min.resolve_opt(avail).unwrap_or(0.0).max(0.0);
            let n = match avail {
                Some(a) if min + gap > 0.0 => (((a + gap) / (min + gap)).floor() as usize).max(1),
                _ => 1,
            };
            let tr = match max {
                Track::Fr(f) => Track::MinMax(Box::new(Track::Px(min)), Box::new(Track::Fr(*f))),
                other => resolve_track(other, avail),
            };
            vec![tr; n.min(200)]
        }
    }
}

struct Placement {
    idx: usize,
    row: usize,
    col: usize,
    rspan: usize,
    cspan: usize,
}

/// Size tracks given the space and per-track content (min, max) contributions.
fn size_tracks(tracks: &[Track], avail: Option<f32>, gap: f32, content: &[(f32, f32)]) -> Vec<f32> {
    let n = tracks.len();
    let mut base = vec![0f32; n];
    let mut fr = vec![0f32; n];
    let mut growable = vec![false; n];
    let mut limit = vec![f32::INFINITY; n];
    for (i, t) in tracks.iter().enumerate() {
        let (cmin, cmax) = content.get(i).copied().unwrap_or((0.0, 0.0));
        match t {
            Track::Px(p) => {
                base[i] = *p;
                limit[i] = *p;
            }
            Track::Pct(p) => {
                base[i] = avail.map(|a| a * p / 100.0).unwrap_or(cmax);
                limit[i] = base[i];
            }
            // `expand` resolves lengths; anything left over sizes like `auto`
            Track::Auto | Track::Len(_) => {
                base[i] = cmax;
                growable[i] = true;
            }
            Track::MinContent => {
                base[i] = cmin;
                limit[i] = cmin;
            }
            Track::MaxContent => {
                base[i] = cmax;
                limit[i] = cmax;
            }
            Track::Fr(f) => {
                base[i] = cmin;
                fr[i] = *f;
            }
            Track::MinMax(a, b) => {
                base[i] = match **a {
                    Track::Px(p) => p,
                    Track::Pct(p) => avail.map(|x| x * p / 100.0).unwrap_or(0.0),
                    Track::MaxContent => cmax,
                    _ => cmin,
                };
                match **b {
                    Track::Fr(f) => fr[i] = f,
                    Track::Px(p) => limit[i] = p.max(base[i]),
                    Track::Pct(p) => limit[i] = avail.map(|x| x * p / 100.0).unwrap_or(cmax).max(base[i]),
                    Track::Auto => {
                        limit[i] = cmax.max(base[i]);
                        growable[i] = true
                    }
                    Track::MaxContent => limit[i] = cmax.max(base[i]),
                    Track::MinContent => limit[i] = cmin.max(base[i]),
                    _ => {}
                }
            }
        }
    }
    let Some(avail) = avail else { return base };
    let gaps = gap * n.saturating_sub(1) as f32;
    let fixed_sum: f32 = (0..n).filter(|&i| fr[i] == 0.0).map(|i| base[i]).sum();
    let sum_fr: f32 = fr.iter().sum();
    if sum_fr > 0.0 {
        // content-sized tracks grow toward their limits before flexible tracks take the rest
        let mut base = base;
        let mut free = avail - gaps - fixed_sum - (0..n).filter(|&i| fr[i] > 0.0).map(|i| base[i]).sum::<f32>();
        for i in 0..n {
            if free <= 0.0 {
                break;
            }
            if fr[i] == 0.0 && limit[i].is_finite() && limit[i] > base[i] {
                let add = (limit[i] - base[i]).min(free);
                base[i] += add;
                free -= add;
            }
        }
        let fixed_sum: f32 = (0..n).filter(|&i| fr[i] == 0.0).map(|i| base[i]).sum();
        let mut leftover = avail - gaps - fixed_sum;
        let mut active: Vec<usize> = (0..n).filter(|&i| fr[i] > 0.0).collect();
        let mut out = base.clone();
        for _ in 0..n + 1 {
            let f: f32 = active.iter().map(|&i| fr[i]).sum();
            if f <= 0.0 {
                break;
            }
            let unit = (leftover / f).max(0.0);
            let mut changed = false;
            let mut next = Vec::new();
            for &i in &active {
                let w = unit * fr[i];
                if w < base[i] {
                    out[i] = base[i];
                    leftover -= base[i];
                    changed = true;
                } else {
                    out[i] = w;
                    next.push(i);
                }
            }
            active = next;
            if !changed {
                break;
            }
        }
        return out;
    }
    let mut free = avail - gaps - fixed_sum;
    let mut out = base;
    // grow minmax(px, px|auto) tracks toward their limits
    for i in 0..n {
        if free <= 0.0 {
            break;
        }
        if limit[i].is_finite() && limit[i] > out[i] {
            let add = (limit[i] - out[i]).min(free);
            out[i] += add;
            free -= add;
        }
    }
    let g: Vec<usize> = (0..n).filter(|&i| growable[i]).collect();
    if free > 0.0 && !g.is_empty() {
        let each = free / g.len() as f32;
        for i in g {
            out[i] += each;
        }
    } else if free < 0.0 {
        // shrink auto tracks toward min-content
        let shrinkable: Vec<usize> = (0..n).filter(|&i| matches!(tracks[i], Track::Auto)).collect();
        let room: f32 = shrinkable.iter().map(|&i| out[i] - content.get(i).map(|c| c.0).unwrap_or(0.0)).sum();
        if room > 0.0 {
            let ratio = ((-free) / room).min(1.0);
            for i in shrinkable {
                let cmin = content.get(i).map(|c| c.0).unwrap_or(0.0);
                out[i] -= (out[i] - cmin) * ratio;
            }
        }
    }
    out
}

fn is_flexible(t: &Track) -> bool {
    match t {
        Track::Fr(_) => true,
        Track::MinMax(_, b) => matches!(**b, Track::Fr(_)),
        _ => false,
    }
}

/// An item's minimum contribution: its `min-width` if set, zero content for scroll
/// containers, otherwise its min-content size (`mn`, margin box).
fn min_contribution(c: &LayoutBox, mn: f32) -> f32 {
    let s = &c.style;
    let margins = s.margin[1].resolve(0.0).unwrap_or(0.0) + s.margin[3].resolve(0.0).unwrap_or(0.0);
    let edges = intrinsic::px_edges(c);
    if !s.min_width.is_auto() {
        let mw = s.min_width.resolve(0.0).unwrap_or(0.0);
        let w = if s.border_box { mw.max(edges) } else { mw + edges };
        // an explicit width still sets the floor
        let fixed = match s.width {
            crate::style::Len::Val(px, 0.0) => Some(if s.border_box { px } else { px + edges }),
            _ => None,
        };
        return fixed.map_or(w, |f| f.max(w)) + margins;
    }
    if !matches!(s.overflow_x, crate::style::Overflow::Visible) {
        return edges + margins;
    }
    mn
}

fn justify_of(c: &LayoutBox, s: &crate::style::ComputedStyle) -> Align {
    match c.style.justify_self {
        Align::Auto => match s.justify_items {
            Align::Auto => Align::Stretch,
            a => a,
        },
        a => a,
    }
}

fn item_mode(c: &LayoutBox, s: &crate::style::ComputedStyle, w: f32, mh: f32) -> WidthMode {
    let auto_margins = c.style.margin[1].is_auto() || c.style.margin[3].is_auto();
    if c.style.width.is_auto() {
        if justify_of(c, s) == Align::Stretch && !auto_margins { WidthMode::Exact((w - mh).max(0.0)) } else { WidthMode::Shrink }
    } else {
        WidthMode::Fill
    }
}

pub(crate) fn layout_grid(b: &mut LayoutBox, st: &mut State, def_h: Option<f32>, min_h: Option<f32>) -> f32 {
    let s = b.style.clone();
    let cw = b.content.w;
    let (cx, cy) = (b.content.x, b.content.y);
    let cgap = s.column_gap;
    let rgap = s.row_gap;
    let mut cols = expand(&s.grid_columns, Some(cw), cgap);
    if cols.is_empty() {
        cols.push(Track::Fr(1.0));
    }
    let ncols = cols.len();
    let row_tracks = expand(&s.grid_rows, def_h, rgap);

    let items: Vec<usize> = (0..b.children.len()).filter(|&i| !b.children[i].is_out_of_flow()).collect();
    for c in b.children.iter_mut().filter(|c| c.is_out_of_flow()) {
        c.static_pos = (cx, cy);
    }
    let mut occupied: Vec<Vec<bool>> = Vec::new();
    let mut place: Vec<Placement> = Vec::new();
    let (mut cur_r, mut cur_c) = (0usize, 0usize);
    let resolve_start = |start: Option<i32>, n: usize| -> Option<usize> {
        start.map(|s| if s > 0 { (s - 1) as usize } else { ((n as i32 + 1 + s).max(0)) as usize }).map(|v| v.min(n.saturating_sub(1)))
    };
    for &i in &items {
        let cs = &b.children[i].style;
        let mut cspan = if cs.grid_column.span == 0 { ncols } else { cs.grid_column.span as usize };
        let col_start = resolve_start(cs.grid_column.start, ncols);
        if cs.grid_column.span == 0 {
            cspan = ncols - col_start.unwrap_or(0);
        }
        let cspan = cspan.clamp(1, ncols);
        let rspan = (cs.grid_row.span.max(1)) as usize;
        let row_start = cs.grid_row.start.filter(|&r| r > 0).map(|r| (r - 1) as usize);
        let fits = |occ: &Vec<Vec<bool>>, r: usize, c: usize| -> bool {
            if c + cspan > ncols {
                return false;
            }
            for rr in r..r + rspan {
                for cc in c..c + cspan {
                    if occ.get(rr).is_some_and(|row| row[cc]) {
                        return false;
                    }
                }
            }
            true
        };
        let (r, c) = match (row_start, col_start) {
            (Some(r), Some(c)) => (r, c),
            (Some(r), None) => {
                let mut c = 0;
                while !fits(&occupied, r, c) && c + 1 < ncols {
                    c += 1;
                }
                (r, c)
            }
            (None, Some(c)) => {
                let mut r = cur_r;
                if c < cur_c {
                    r += 1;
                }
                while !fits(&occupied, r, c) {
                    r += 1;
                }
                (r, c)
            }
            (None, None) => {
                let (mut r, mut c) = (cur_r, cur_c);
                loop {
                    if c + cspan > ncols {
                        r += 1;
                        c = 0;
                        continue;
                    }
                    if fits(&occupied, r, c) {
                        break;
                    }
                    c += 1;
                }
                (r, c)
            }
        };
        while occupied.len() < r + rspan {
            occupied.push(vec![false; ncols]);
        }
        for rr in r..r + rspan {
            for cc in c..c + cspan {
                occupied[rr][cc] = true;
            }
        }
        if row_start.is_none() {
            cur_r = r;
            cur_c = c + cspan;
        }
        place.push(Placement { idx: i, row: r, col: c, rspan, cspan });
    }
    let nrows = occupied.len().max(row_tracks.len());

    // column content contributions (single-span items)
    let mut ccontent = vec![(0f32, 0f32); ncols];
    for p in &place {
        if p.cspan == 1 {
            let c = &b.children[p.idx];
            let (mn, mx) = intrinsic::outer(c, st);
            let mn = min_contribution(c, mn);
            ccontent[p.col].0 = ccontent[p.col].0.max(mn);
            ccontent[p.col].1 = ccontent[p.col].1.max(mx);
        }
    }
    let col_w = size_tracks(&cols, Some(cw), cgap, &ccontent);
    let col_x: Vec<f32> = col_w.iter().scan(0.0, |acc, w| {
        let x = *acc;
        *acc += w + cgap;
        Some(x)
    }).collect();

    // lay out items with their column widths to find row heights
    let span_w = |p: &Placement| -> f32 { col_w[p.col..p.col + p.cspan].iter().sum::<f32>() + cgap * (p.cspan - 1) as f32 };
    let mut heights = vec![0f32; place.len()];
    for (k, p) in place.iter().enumerate() {
        let c = &mut b.children[p.idx];
        let w = span_w(p);
        let m = block::resolve_edges(&c.style.margin, cw);
        let mode = item_mode(c, &s, w, m.h());
        let a = Avail { cb_w: w, cb_h: None, mode, forced_h: None, bfc: true };
        block::layout_box(c, st, 0.0, 0.0, &a);
        heights[k] = c.margin_box().h;
    }
    let mut rcontent = vec![(0f32, 0f32); nrows];
    for (k, p) in place.iter().enumerate() {
        if p.rspan == 1 {
            rcontent[p.row].0 = rcontent[p.row].0.max(heights[k]);
            rcontent[p.row].1 = rcontent[p.row].1.max(heights[k]);
        }
    }
    let mut rtracks = row_tracks.clone();
    while rtracks.len() < nrows {
        rtracks.push(Track::Auto);
    }
    let has_fr_rows = rtracks.iter().any(is_flexible);
    let mut row_h = size_tracks(&rtracks, if has_fr_rows { def_h } else { None }, rgap, &rcontent);
    // multi-row items: grow the last spanned row if needed
    for (k, p) in place.iter().enumerate() {
        if p.rspan > 1 {
            let end = (p.row + p.rspan).min(nrows);
            let have: f32 = row_h[p.row..end].iter().sum::<f32>() + rgap * (p.rspan - 1) as f32;
            if heights[k] > have {
                row_h[end - 1] += heights[k] - have;
            }
        }
    }
    // auto rows stretch to fill a container that has a definite or minimum height
    let target = def_h.or(min_h);
    if let Some(t) = target {
        let total: f32 = row_h.iter().sum::<f32>() + rgap * nrows.saturating_sub(1) as f32;
        let autos: Vec<usize> = (0..nrows).filter(|&i| matches!(rtracks.get(i), Some(Track::Auto))).collect();
        if t > total && !autos.is_empty() && matches!(s.align_content, Align::Stretch | Align::Auto) {
            let extra = (t - total) / autos.len() as f32;
            for i in autos {
                row_h[i] += extra;
            }
        }
    }
    let row_y: Vec<f32> = row_h.iter().scan(0.0, |acc, h| {
        let y = *acc;
        *acc += h + rgap;
        Some(y)
    }).collect();

    for (k, p) in place.iter().enumerate() {
        let c = &mut b.children[p.idx];
        let end = (p.row + p.rspan).min(nrows);
        let cell_h = row_h[p.row..end].iter().sum::<f32>() + rgap * (end - p.row).saturating_sub(1) as f32;
        let align = match c.style.align_self {
            Align::Auto => s.align_items,
            a => a,
        };
        let w = span_w(p);
        let m = block::resolve_edges(&c.style.margin, cw);
        if align == Align::Stretch && c.style.height.is_auto() && (cell_h - heights[k]).abs() > 0.01 {
            let mode = item_mode(c, &s, w, m.h());
            let a = Avail { cb_w: w, cb_h: Some(cell_h), mode, forced_h: Some((cell_h - m.v()).max(0.0)), bfc: true };
            block::layout_box(c, st, 0.0, 0.0, &a);
        }
        let mb = c.margin_box();
        let dy = match align {
            Align::Center => (cell_h - mb.h) / 2.0,
            Align::End => cell_h - mb.h,
            _ => 0.0,
        };
        let free = (w - mb.w).max(0.0);
        let (la, ra) = (c.style.margin[3].is_auto(), c.style.margin[1].is_auto());
        // auto margins absorb free space first, then justify-self applies
        let dx = if la && ra {
            free / 2.0
        } else if la {
            free
        } else if ra {
            0.0
        } else {
            match justify_of(c, &s) {
                Align::Center => free / 2.0,
                Align::End => free,
                _ => 0.0,
            }
        };
        let tx = cx + col_x[p.col] + dx;
        let ty = cy + row_y.get(p.row).copied().unwrap_or(0.0) + dy;
        c.translate(tx - mb.x, ty - mb.y);
    }
    let total: f32 = row_h.iter().sum::<f32>() + rgap * nrows.saturating_sub(1) as f32;
    def_h.unwrap_or(total)
}

