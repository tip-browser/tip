//! Flexbox layout.

use super::block::{self, Avail, WidthMode};
use super::{intrinsic, LayoutBox, State};
use crate::style::{Align, Justify, Len};

struct FItem {
    idx: usize,
    base: f32,
    hyp: f32,
    min: f32,
    max: f32,
    grow: f32,
    shrink: f32,
    main: f32,
    m_start: f32,
    m_end: f32,
    auto_start: bool,
    auto_end: bool,
    c_start: f32,
    c_end: f32,
    c_auto_start: bool,
    c_auto_end: bool,
    cross: f32,
    align: Align,
    frozen: bool,
    baseline: f32,
}

fn resolve_main_prop(l: Len, base: Option<f32>) -> Option<f32> {
    l.resolve_opt(base)
}

pub(crate) fn layout_flex(b: &mut LayoutBox, st: &mut State, def_h: Option<f32>, min_h: Option<f32>, _a: &Avail) -> f32 {
    let s = b.style.clone();
    let row = s.flex_direction.is_row();
    let reverse = s.flex_direction.is_reverse();
    let cw = b.content.w;
    let cx = b.content.x;
    let cy = b.content.y;
    let mut main_size: Option<f32> = if row { Some(cw) } else { def_h };
    let cross_size: Option<f32> = if row { def_h } else { Some(cw) };
    let (gap_main, gap_cross) = if row { (s.column_gap, s.row_gap) } else { (s.row_gap, s.column_gap) };

    let mut order: Vec<usize> = (0..b.children.len()).filter(|&i| !b.children[i].is_out_of_flow()).collect();
    order.sort_by_key(|&i| b.children[i].style.order);
    for c in b.children.iter_mut().filter(|c| c.is_out_of_flow()) {
        c.static_pos = (cx, cy);
    }

    let mut items: Vec<FItem> = Vec::with_capacity(order.len());
    for &i in &order {
        let c = &mut b.children[i];
        let cs = c.style.clone();
        let pad = block::resolve_edges(&cs.padding, cw);
        let bor = block::border_edges(c);
        let mar = block::resolve_edges(&cs.margin, cw);
        let (pb_main, pb_cross) = if row { (pad.h() + bor.h(), pad.v() + bor.v()) } else { (pad.v() + bor.v(), pad.h() + bor.h()) };
        let (m_start, m_end, auto_start, auto_end) = if row {
            (mar.left, mar.right, cs.margin[3].is_auto(), cs.margin[1].is_auto())
        } else {
            (mar.top, mar.bottom, cs.margin[0].is_auto(), cs.margin[2].is_auto())
        };
        let (c_start, c_end, c_auto_start, c_auto_end) = if row {
            (mar.top, mar.bottom, cs.margin[0].is_auto(), cs.margin[2].is_auto())
        } else {
            (mar.left, mar.right, cs.margin[3].is_auto(), cs.margin[1].is_auto())
        };
        let to_border_box = |v: f32| if cs.border_box { v } else { v + pb_main };
        let main_prop = if row { cs.width } else { cs.height };
        let mut base = match cs.flex_basis {
            Len::Auto => resolve_main_prop(main_prop, main_size).map(to_border_box),
            l => resolve_main_prop(l, main_size).map(to_border_box),
        };
        let is_replaced = matches!(c.inner, super::Inner::Replaced(_));
        if base.is_none() {
            base = Some(if row {
                if is_replaced {
                    block::replaced_size(c, st, cw, def_h).0 + pb_main
                } else {
                    intrinsic::intrinsic(c, st).1
                }
            } else {
                // measure height at the item's cross size
                let stretch = matches!(item_align(&cs.align_self, s.align_items), Align::Stretch) && cs.width.is_auto();
                let mode = if stretch { WidthMode::Exact((cw - mar.h()).max(0.0)) } else { WidthMode::Shrink };
                let a = Avail { cb_w: cw, cb_h: def_h, mode, forced_h: None, bfc: true };
                block::layout_box(c, st, 0.0, 0.0, &a);
                c.border_box().h
            });
        }
        let base = base.unwrap_or(0.0).max(pb_main);
        let min_prop = if row { cs.min_width } else { cs.min_height };
        let max_prop = if row { cs.max_width } else { cs.max_height };
        let min = match min_prop {
            Len::Auto => {
                if row && !cs.overflow_x.clips() {
                    let content_min = if is_replaced { 0.0 } else { intrinsic::intrinsic(c, st).0 };
                    let spec = resolve_main_prop(main_prop, main_size).map(to_border_box);
                    match spec {
                        Some(sv) => content_min.min(sv),
                        None => content_min,
                    }
                } else {
                    0.0
                }
            }
            l => resolve_main_prop(l, main_size).map(to_border_box).unwrap_or(0.0),
        };
        let max = resolve_main_prop(max_prop, main_size).map(to_border_box).unwrap_or(f32::INFINITY);
        let hyp = base.clamp(min.min(max), max);
        let _ = pb_cross;
        items.push(FItem {
            idx: i,
            base,
            hyp,
            min,
            max,
            grow: cs.flex_grow,
            shrink: cs.flex_shrink,
            main: hyp,
            m_start,
            m_end,
            auto_start,
            auto_end,
            c_start,
            c_end,
            c_auto_start,
            c_auto_end,
            cross: 0.0,
            align: item_align(&cs.align_self, s.align_items),
            frozen: false,
            baseline: 0.0,
        });
    }

    // A column container with only a min-height grows to at least that height.
    if !row && main_size.is_none() {
        if let Some(mh) = min_h {
            let used: f32 = items.iter().map(|it| it.hyp + it.m_start + it.m_end).sum::<f32>() + gap_main * items.len().saturating_sub(1) as f32;
            main_size = Some(used.max(mh));
        }
    }

    // ---- lines
    let mut lines: Vec<Vec<usize>> = Vec::new();
    {
        let mut cur: Vec<usize> = Vec::new();
        let mut used = 0.0;
        for (k, it) in items.iter().enumerate() {
            let outer = it.hyp + it.m_start + it.m_end;
            let g = if cur.is_empty() { 0.0 } else { gap_main };
            if s.flex_wrap && !cur.is_empty() {
                if let Some(ms) = main_size {
                    if used + g + outer > ms + 0.01 {
                        lines.push(std::mem::take(&mut cur));
                        used = 0.0;
                    }
                }
            }
            let g = if cur.is_empty() { 0.0 } else { gap_main };
            used += g + outer;
            cur.push(k);
        }
        if !cur.is_empty() || lines.is_empty() {
            lines.push(cur);
        }
    }

    // ---- resolve flexible lengths
    for line in &lines {
        let Some(ms) = main_size else { break };
        let gaps = gap_main * line.len().saturating_sub(1) as f32;
        for _ in 0..line.len() + 1 {
            let used: f32 = line.iter().map(|&k| {
                let it = &items[k];
                (if it.frozen { it.main } else { it.hyp }) + it.m_start + it.m_end
            }).sum::<f32>() + gaps;
            let free = ms - used;
            let unfrozen: Vec<usize> = line.iter().copied().filter(|&k| !items[k].frozen).collect();
            if unfrozen.is_empty() {
                break;
            }
            let mut any_clamped = false;
            if free > 0.0 {
                let sum_grow: f32 = unfrozen.iter().map(|&k| items[k].grow).sum();
                if sum_grow <= 0.0 {
                    for &k in &unfrozen {
                        items[k].main = items[k].hyp;
                    }
                    break;
                }
                let dist = if sum_grow < 1.0 { free * sum_grow } else { free };
                for &k in &unfrozen {
                    let it = &mut items[k];
                    let target = it.hyp + dist * it.grow / sum_grow;
                    let clamped = target.clamp(it.min.min(it.max), it.max);
                    it.main = clamped;
                    if (clamped - target).abs() > 0.01 {
                        it.frozen = true;
                        any_clamped = true;
                    }
                }
            } else if free < 0.0 {
                let sum_scaled: f32 = unfrozen.iter().map(|&k| items[k].shrink * items[k].base).sum();
                if sum_scaled <= 0.0 {
                    for &k in &unfrozen {
                        items[k].main = items[k].hyp;
                    }
                    break;
                }
                for &k in &unfrozen {
                    let it = &mut items[k];
                    let target = it.hyp + free * (it.shrink * it.base) / sum_scaled;
                    let clamped = target.clamp(it.min.min(it.max), it.max);
                    it.main = clamped;
                    if (clamped - target).abs() > 0.01 {
                        it.frozen = true;
                        any_clamped = true;
                    }
                }
            } else {
                for &k in &unfrozen {
                    items[k].main = items[k].hyp;
                }
                break;
            }
            if !any_clamped {
                break;
            }
        }
    }

    // ---- lay out items at their main size to find cross sizes
    for it in items.iter_mut() {
        let c = &mut b.children[it.idx];
        let stretch_w = !row && it.align == Align::Stretch && c.style.width.is_auto() && !it.c_auto_start && !it.c_auto_end;
        let a = if row {
            Avail { cb_w: cw, cb_h: def_h, mode: WidthMode::Exact(it.main), forced_h: None, bfc: true }
        } else {
            let mode = if stretch_w { WidthMode::Exact((cw - it.c_start - it.c_end).max(0.0)) } else { WidthMode::Shrink };
            Avail { cb_w: cw, cb_h: def_h, mode, forced_h: if main_size.is_some() || it.main != it.hyp || true { Some(it.main) } else { None }, bfc: true }
        };
        block::layout_box(c, st, 0.0, 0.0, &a);
        let bb = c.border_box();
        it.cross = if row { bb.h } else { bb.w };
        it.baseline = c.first_baseline().map(|v| v - bb.y).unwrap_or(bb.h);
    }

    // ---- line cross sizes
    let mut line_cross: Vec<f32> = lines
        .iter()
        .map(|line| {
            let mut mx: f32 = 0.0;
            let mut max_base: f32 = 0.0;
            let mut max_below: f32 = 0.0;
            for &k in line {
                let it = &items[k];
                mx = mx.max(it.cross + it.c_start + it.c_end);
                if it.align == Align::Baseline && row {
                    max_base = max_base.max(it.baseline + it.c_start);
                    max_below = max_below.max(it.cross - it.baseline + it.c_end);
                }
            }
            mx.max(max_base + max_below)
        })
        .collect();
    if lines.len() == 1 {
        if let Some(cs) = cross_size {
            line_cross[0] = cs;
        } else if let (true, Some(mh)) = (row, min_h) {
            line_cross[0] = line_cross[0].max(mh);
        }
    } else if let Some(cs) = cross_size {
        let total: f32 = line_cross.iter().sum::<f32>() + gap_cross * (lines.len() - 1) as f32;
        if s.align_content == Align::Stretch && total < cs {
            let extra = (cs - total) / lines.len() as f32;
            for lc in &mut line_cross {
                *lc += extra;
            }
        }
    }

    // ---- stretch items
    for (li, line) in lines.iter().enumerate() {
        for &k in line {
            let it = &mut items[k];
            if it.align != Align::Stretch || it.c_auto_start || it.c_auto_end {
                continue;
            }
            let c = &mut b.children[it.idx];
            let prop_auto = if row { c.style.height.is_auto() } else { c.style.width.is_auto() };
            if !prop_auto {
                continue;
            }
            let target = (line_cross[li] - it.c_start - it.c_end).max(0.0);
            if row {
                if (target - it.cross).abs() > 0.01 {
                    let a = Avail { cb_w: cw, cb_h: def_h, mode: WidthMode::Exact(it.main), forced_h: Some(target), bfc: true };
                    block::layout_box(c, st, 0.0, 0.0, &a);
                    it.cross = target;
                }
            }
        }
    }

    // ---- positioning
    let total_cross: f32 = line_cross.iter().sum::<f32>() + gap_cross * lines.len().saturating_sub(1) as f32;
    let mut cross_cursor = 0.0;
    if lines.len() > 1 {
        if let Some(cs) = cross_size {
            let free = cs - total_cross;
            if free > 0.0 {
                cross_cursor = match s.align_content {
                    Align::Center => free / 2.0,
                    Align::End => free,
                    _ => 0.0,
                };
            }
        }
    }
    let mut max_main_used: f32 = 0.0;
    for (li, line) in lines.iter().enumerate() {
        let ms = main_size.unwrap_or_else(|| {
            line.iter().map(|&k| items[k].main + items[k].m_start + items[k].m_end).sum::<f32>() + gap_main * line.len().saturating_sub(1) as f32
        });
        let used: f32 = line.iter().map(|&k| items[k].main + items[k].m_start + items[k].m_end).sum::<f32>() + gap_main * line.len().saturating_sub(1) as f32;
        max_main_used = max_main_used.max(used);
        let mut free = ms - used;
        let n_auto: usize = line.iter().map(|&k| items[k].auto_start as usize + items[k].auto_end as usize).sum();
        let mut auto_share = 0.0;
        if n_auto > 0 && free > 0.0 {
            auto_share = free / n_auto as f32;
            free = 0.0;
        }
        let n = line.len() as f32;
        let (mut pos, between) = if free > 0.0 {
            match s.justify_content {
                Justify::Start => (0.0, 0.0),
                Justify::End => (free, 0.0),
                Justify::Center => (free / 2.0, 0.0),
                Justify::SpaceBetween => (0.0, if n > 1.0 { free / (n - 1.0) } else { 0.0 }),
                Justify::SpaceAround => (free / n / 2.0, free / n),
                Justify::SpaceEvenly => (free / (n + 1.0), free / (n + 1.0)),
            }
        } else {
            match s.justify_content {
                Justify::End => (free, 0.0),
                Justify::Center => (free / 2.0, 0.0),
                _ => (0.0, 0.0),
            }
        };
        let max_base: f32 = line.iter().filter(|&&k| items[k].align == Align::Baseline).map(|&k| items[k].baseline + items[k].c_start).fold(0.0, f32::max);
        let seq: Vec<usize> = if reverse { line.iter().rev().copied().collect() } else { line.clone() };
        for (j, &k) in seq.iter().enumerate() {
            let it = &items[k];
            if j > 0 {
                pos += gap_main + between;
            }
            pos += it.m_start + if it.auto_start { auto_share } else { 0.0 };
            let main_pos = if reverse { ms - pos - it.main } else { pos };
            pos += it.main + it.m_end + if it.auto_end { auto_share } else { 0.0 };
            let lc = line_cross[li];
            let outer_cross = it.cross + it.c_start + it.c_end;
            let cross_off = if it.c_auto_start && it.c_auto_end {
                (lc - it.cross) / 2.0
            } else if it.c_auto_start {
                lc - it.cross - it.c_end
            } else if it.c_auto_end {
                it.c_start
            } else {
                match it.align {
                    Align::End => lc - outer_cross + it.c_start,
                    Align::Center => (lc - outer_cross) / 2.0 + it.c_start,
                    Align::Baseline if row => max_base - it.baseline,
                    _ => it.c_start,
                }
            };
            let c = &mut b.children[it.idx];
            let bb = c.border_box();
            let (tx, ty) = if row { (cx + main_pos, cy + cross_cursor + cross_off) } else { (cx + cross_cursor + cross_off, cy + main_pos) };
            c.translate(tx - bb.x, ty - bb.y);
        }
        cross_cursor += line_cross[li] + gap_cross;
    }
    if row {
        def_h.unwrap_or(total_cross)
    } else {
        def_h.unwrap_or(max_main_used.max(main_size.unwrap_or(0.0)))
    }
}

fn item_align(self_align: &Align, items: Align) -> Align {
    match self_align {
        Align::Auto => items,
        a => *a,
    }
}
