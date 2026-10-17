//! Inline SVG: shapes, paths, transforms and viewBox, filled and stroked into
//! rasterizer paths.

use crate::dom::{Document, NodeId};
use crate::raster::Rect;
use crate::css::Color;
use crate::text::ttf::Seg;

type Pt = (f32, f32);

#[derive(Clone, Copy)]
struct Mat([f32; 6]);

impl Mat {
    const ID: Mat = Mat([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    fn mul(self, o: Mat) -> Mat {
        let [a, b, c, d, e, f] = self.0;
        let [a2, b2, c2, d2, e2, f2] = o.0;
        Mat([a * a2 + c * b2, b * a2 + d * b2, a * c2 + c * d2, b * c2 + d * d2, a * e2 + c * f2 + e, b * e2 + d * f2 + f])
    }
    fn apply(&self, p: Pt) -> Pt {
        let [a, b, c, d, e, f] = self.0;
        (a * p.0 + c * p.1 + e, b * p.0 + d * p.1 + f)
    }
    /// Average scale factor (for stroke widths).
    fn scale(&self) -> f32 {
        let [a, b, c, d, ..] = self.0;
        ((a * d - b * c).abs()).sqrt()
    }
}

#[derive(Clone)]
enum Paint {
    None,
    Color(Color),
    Current,
}

#[derive(Clone)]
struct Ctx {
    m: Mat,
    fill: Paint,
    stroke: Paint,
    stroke_width: f32,
    cap: u8,  // 0 butt, 1 round, 2 square
    join: u8, // 0 miter, 1 round, 2 bevel
    miter: f32,
    opacity: f32,
    fill_opacity: f32,
    stroke_opacity: f32,
    color: Color,
    font_size: f32,
}

/// A filled region ready to paint.
pub struct Shape {
    pub segs: Vec<Seg>,
    pub color: Color,
}

fn color(v: &str) -> Option<Color> {
    crate::css::parse_color(&crate::css::tokenize(v))
}

fn num(s: &str) -> Option<f32> {
    s.trim().trim_end_matches("px").trim().parse().ok()
}

/// Parse an SVG length attribute (`24`, `24px`, `1.5em`); percentages resolve against `pct_base`.
pub fn length(v: &str, font_size: f32, pct_base: f32) -> Option<f32> {
    let v = v.trim();
    if let Some(n) = v.strip_suffix("em") {
        if !n.ends_with('r') {
            return n.trim().parse::<f32>().ok().map(|x| x * font_size);
        }
    }
    if let Some(n) = v.strip_suffix("rem") {
        return n.trim().parse::<f32>().ok().map(|x| x * 16.0);
    }
    if let Some(n) = v.strip_suffix('%') {
        return n.trim().parse::<f32>().ok().map(|x| x * pct_base / 100.0);
    }
    if let Some(n) = v.strip_suffix("pt") {
        return n.trim().parse::<f32>().ok().map(|x| x * 4.0 / 3.0);
    }
    num(v)
}

fn numbers(s: &str) -> Vec<f32> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_digit() || c == b'-' || c == b'+' || c == b'.' {
            let start = i;
            i += 1;
            let mut seen_dot = c == b'.';
            let mut seen_e = false;
            while i < b.len() {
                let d = b[i];
                if d.is_ascii_digit() {
                    i += 1;
                } else if d == b'.' && !seen_dot && !seen_e {
                    seen_dot = true;
                    i += 1;
                } else if (d == b'e' || d == b'E') && !seen_e {
                    seen_e = true;
                    i += 1;
                    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
                        i += 1;
                    }
                } else {
                    break;
                }
            }
            if let Ok(v) = s[start..i].parse::<f32>() {
                out.push(v);
            }
        } else {
            i += 1;
        }
    }
    out
}

pub fn view_box(doc: &Document, n: NodeId) -> Option<[f32; 4]> {
    let v = numbers(doc.attr(n, "viewBox").or_else(|| doc.attr(n, "viewbox"))?);
    (v.len() == 4 && v[2] > 0.0 && v[3] > 0.0).then(|| [v[0], v[1], v[2], v[3]])
}

/// Intrinsic size of an `<svg>` element from its width/height attributes and viewBox.
pub fn intrinsic_size(doc: &Document, n: NodeId, font_size: f32) -> (f32, f32) {
    let w = doc.attr(n, "width").and_then(|v| if v.trim().ends_with('%') { None } else { length(v, font_size, 0.0) });
    let h = doc.attr(n, "height").and_then(|v| if v.trim().ends_with('%') { None } else { length(v, font_size, 0.0) });
    let ratio = view_box(doc, n).map(|v| v[2] / v[3]);
    match (w, h, ratio) {
        (Some(w), Some(h), _) => (w, h),
        (Some(w), None, Some(r)) => (w, w / r),
        (None, Some(h), Some(r)) => (h * r, h),
        (Some(w), None, None) => (w, 150.0),
        (None, Some(h), None) => (300.0, h),
        (None, None, Some(r)) if r > 0.0 => (300.0, 300.0 / r),
        _ => (300.0, 150.0),
    }
}

fn transform(s: &str) -> Mat {
    let mut m = Mat::ID;
    let mut rest = s;
    while let Some(open) = rest.find('(') {
        let name = rest[..open].trim().trim_start_matches(',').trim();
        let Some(close) = rest[open..].find(')') else { break };
        let args = numbers(&rest[open + 1..open + close]);
        rest = &rest[open + close + 1..];
        let a = |i: usize| args.get(i).copied().unwrap_or(0.0);
        let t = match name {
            "matrix" if args.len() == 6 => Mat([a(0), a(1), a(2), a(3), a(4), a(5)]),
            "translate" => Mat([1.0, 0.0, 0.0, 1.0, a(0), a(1)]),
            "scale" => {
                let sx = args.first().copied().unwrap_or(1.0);
                let sy = args.get(1).copied().unwrap_or(sx);
                Mat([sx, 0.0, 0.0, sy, 0.0, 0.0])
            }
            "rotate" => {
                let (s, c) = a(0).to_radians().sin_cos();
                let r = Mat([c, s, -s, c, 0.0, 0.0]);
                if args.len() >= 3 {
                    Mat([1.0, 0.0, 0.0, 1.0, a(1), a(2)]).mul(r).mul(Mat([1.0, 0.0, 0.0, 1.0, -a(1), -a(2)]))
                } else {
                    r
                }
            }
            "skewX" => Mat([1.0, 0.0, a(0).to_radians().tan(), 1.0, 0.0, 0.0]),
            "skewY" => Mat([1.0, a(0).to_radians().tan(), 0.0, 1.0, 0.0, 0.0]),
            _ => Mat::ID,
        };
        m = m.mul(t);
    }
    m
}

fn parse_paint(v: &str) -> Option<Paint> {
    let v = v.trim();
    if v == "none" || v == "transparent" {
        return Some(Paint::None);
    }
    if v.eq_ignore_ascii_case("currentcolor") {
        return Some(Paint::Current);
    }
    if v.starts_with("url(") {
        // gradients/patterns: fall back to the declared fallback color or currentColor
        return v.split(')').nth(1).and_then(|f| parse_paint(f)).or(Some(Paint::Current));
    }
    color(v).map(Paint::Color)
}

/// Presentation attribute or `style` declaration.
fn prop<'a>(doc: &'a Document, n: NodeId, name: &str, style: &'a [(String, String)]) -> Option<String> {
    if let Some((_, v)) = style.iter().rev().find(|(k, _)| k == name) {
        return Some(v.clone());
    }
    doc.attr(n, name).map(|s| s.to_string())
}

fn inline_style(doc: &Document, n: NodeId) -> Vec<(String, String)> {
    doc.attr(n, "style")
        .map(|s| s.split(';').filter_map(|d| d.split_once(':').map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().trim_end_matches("!important").trim().to_string()))).collect())
        .unwrap_or_default()
}

fn apply_props(doc: &Document, n: NodeId, ctx: &mut Ctx) {
    let st = inline_style(doc, n);
    let p = |k: &str| prop(doc, n, k, &st);
    if let Some(c) = p("color").and_then(|v| color(&v)) {
        ctx.color = c;
    }
    if let Some(v) = p("fill").and_then(|v| parse_paint(&v)) {
        ctx.fill = v;
    }
    if let Some(v) = p("stroke").and_then(|v| parse_paint(&v)) {
        ctx.stroke = v;
    }
    if let Some(v) = p("stroke-width").and_then(|v| length(&v, ctx.font_size, 24.0)) {
        ctx.stroke_width = v.max(0.0);
    }
    if let Some(v) = p("stroke-linecap") {
        ctx.cap = match v.trim() {
            "round" => 1,
            "square" => 2,
            _ => 0,
        };
    }
    if let Some(v) = p("stroke-linejoin") {
        ctx.join = match v.trim() {
            "round" => 1,
            "bevel" => 2,
            _ => 0,
        };
    }
    if let Some(v) = p("stroke-miterlimit").and_then(|v| num(&v)) {
        ctx.miter = v.max(1.0);
    }
    for (k, slot) in [("opacity", 0usize), ("fill-opacity", 1), ("stroke-opacity", 2)] {
        if let Some(v) = p(k).and_then(|v| num(&v)) {
            let v = v.clamp(0.0, 1.0);
            match slot {
                0 => ctx.opacity *= v,
                1 => ctx.fill_opacity = v,
                _ => ctx.stroke_opacity = v,
            }
        }
    }
    if let Some(t) = doc.attr(n, "transform") {
        ctx.m = ctx.m.mul(transform(t));
    }
}

// ---------------------------------------------------------------- geometry

/// Flattened subpaths: points plus closed flag.
type Poly = (Vec<Pt>, bool);

fn cubic(out: &mut Vec<Pt>, p0: Pt, c1: Pt, c2: Pt, p1: Pt, tol: f32) {
    let d = ((c1.0 - p0.0).abs() + (c1.1 - p0.1).abs() + (c2.0 - c1.0).abs() + (c2.1 - c1.1).abs() + (p1.0 - c2.0).abs() + (p1.1 - c2.1).abs()) / tol;
    let n = (d.sqrt().ceil() as usize).clamp(2, 64);
    for i in 1..=n {
        let t = i as f32 / n as f32;
        let mt = 1.0 - t;
        let a = mt * mt * mt;
        let b = 3.0 * mt * mt * t;
        let c = 3.0 * mt * t * t;
        let e = t * t * t;
        out.push((a * p0.0 + b * c1.0 + c * c2.0 + e * p1.0, a * p0.1 + b * c1.1 + c * c2.1 + e * p1.1));
    }
}

fn quad(out: &mut Vec<Pt>, p0: Pt, c: Pt, p1: Pt, tol: f32) {
    cubic(out, p0, (p0.0 + 2.0 / 3.0 * (c.0 - p0.0), p0.1 + 2.0 / 3.0 * (c.1 - p0.1)), (p1.0 + 2.0 / 3.0 * (c.0 - p1.0), p1.1 + 2.0 / 3.0 * (c.1 - p1.1)), p1, tol);
}

#[allow(clippy::too_many_arguments)]
fn arc(out: &mut Vec<Pt>, p0: Pt, rx: f32, ry: f32, phi: f32, large: bool, sweep: bool, p1: Pt) {
    if p0 == p1 {
        return;
    }
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if rx == 0.0 || ry == 0.0 {
        out.push(p1);
        return;
    }
    let (sp, cp) = phi.to_radians().sin_cos();
    let dx = (p0.0 - p1.0) / 2.0;
    let dy = (p0.1 - p1.1) / 2.0;
    let x1 = cp * dx + sp * dy;
    let y1 = -sp * dx + cp * dy;
    let lam = (x1 * x1) / (rx * rx) + (y1 * y1) / (ry * ry);
    if lam > 1.0 {
        rx *= lam.sqrt();
        ry *= lam.sqrt();
    }
    let num = (rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1).max(0.0);
    let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
    let mut co = if den > 0.0 { (num / den).sqrt() } else { 0.0 };
    if large == sweep {
        co = -co;
    }
    let cxp = co * rx * y1 / ry;
    let cyp = -co * ry * x1 / rx;
    let cx = cp * cxp - sp * cyp + (p0.0 + p1.0) / 2.0;
    let cy = sp * cxp + cp * cyp + (p0.1 + p1.1) / 2.0;
    let ang = |ux: f32, uy: f32, vx: f32, vy: f32| {
        let a = (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
        a
    };
    let t1 = ang(1.0, 0.0, (x1 - cxp) / rx, (y1 - cyp) / ry);
    let mut dt = ang((x1 - cxp) / rx, (y1 - cyp) / ry, (-x1 - cxp) / rx, (-y1 - cyp) / ry);
    if !sweep && dt > 0.0 {
        dt -= std::f32::consts::TAU;
    } else if sweep && dt < 0.0 {
        dt += std::f32::consts::TAU;
    }
    let n = ((dt.abs() / (std::f32::consts::PI / 16.0)).ceil() as usize).clamp(2, 128);
    for i in 1..=n {
        let t = t1 + dt * i as f32 / n as f32;
        let (s, c) = t.sin_cos();
        out.push((cx + cp * rx * c - sp * ry * s, cy + sp * rx * c + cp * ry * s));
    }
}

fn path_data(d: &str) -> Vec<Poly> {
    let mut polys: Vec<Poly> = Vec::new();
    let mut cur: Vec<Pt> = Vec::new();
    let b = d.as_bytes();
    let mut i = 0;
    let mut cmd = b'M';
    let (mut x, mut y) = (0f32, 0f32);
    let (mut sx, mut sy) = (0f32, 0f32);
    let mut last_ctrl: Option<(u8, Pt)> = None;
    let tol = 0.25;
    // read the next number (or flag for arcs)
    let next_num = |i: &mut usize, flag: bool| -> Option<f32> {
        while *i < b.len() && (b[*i].is_ascii_whitespace() || b[*i] == b',') {
            *i += 1;
        }
        if *i >= b.len() {
            return None;
        }
        if flag && (b[*i] == b'0' || b[*i] == b'1') {
            *i += 1;
            return Some((b[*i - 1] - b'0') as f32);
        }
        let start = *i;
        if b[*i] == b'-' || b[*i] == b'+' {
            *i += 1;
        }
        let mut dot = false;
        let mut digits = false;
        while *i < b.len() {
            let c = b[*i];
            if c.is_ascii_digit() {
                digits = true;
                *i += 1;
            } else if c == b'.' && !dot {
                dot = true;
                *i += 1;
            } else if (c == b'e' || c == b'E') && digits {
                *i += 1;
                if *i < b.len() && (b[*i] == b'-' || b[*i] == b'+') {
                    *i += 1;
                }
                while *i < b.len() && b[*i].is_ascii_digit() {
                    *i += 1;
                }
                break;
            } else {
                break;
            }
        }
        if !digits {
            *i = start;
            return None;
        }
        d[start..*i].parse().ok()
    };
    let flush = |cur: &mut Vec<Pt>, polys: &mut Vec<Poly>, closed: bool| {
        if cur.len() > 1 || (cur.len() == 1 && !closed) {
            polys.push((std::mem::take(cur), closed));
        } else {
            cur.clear();
        }
    };
    loop {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b',') {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        if b[i].is_ascii_alphabetic() {
            cmd = b[i];
            i += 1;
            if cmd == b'Z' || cmd == b'z' {
                if !cur.is_empty() {
                    flush(&mut cur, &mut polys, true);
                }
                x = sx;
                y = sy;
                last_ctrl = None;
                continue;
            }
        }
        let rel = cmd.is_ascii_lowercase();
        let (ox, oy) = if rel { (x, y) } else { (0.0, 0.0) };
        macro_rules! n {
            () => {
                match next_num(&mut i, false) {
                    Some(v) => v,
                    None => break,
                }
            };
        }
        match cmd.to_ascii_uppercase() {
            b'M' => {
                let (nx, ny) = (n!() + ox, n!() + oy);
                if !cur.is_empty() {
                    flush(&mut cur, &mut polys, false);
                }
                x = nx;
                y = ny;
                sx = x;
                sy = y;
                cur.push((x, y));
                // subsequent pairs are implicit lineto
                cmd = if rel { b'l' } else { b'L' };
                last_ctrl = None;
            }
            b'L' => {
                x = n!() + ox;
                y = n!() + oy;
                if cur.is_empty() {
                    cur.push((sx, sy));
                }
                cur.push((x, y));
                last_ctrl = None;
            }
            b'H' => {
                x = n!() + ox;
                if cur.is_empty() {
                    cur.push((sx, sy));
                }
                cur.push((x, y));
                last_ctrl = None;
            }
            b'V' => {
                y = n!() + oy;
                if cur.is_empty() {
                    cur.push((sx, sy));
                }
                cur.push((x, y));
                last_ctrl = None;
            }
            b'C' | b'S' => {
                let upper = cmd.to_ascii_uppercase();
                let c1 = if upper == b'C' {
                    (n!() + ox, n!() + oy)
                } else {
                    match last_ctrl {
                        Some((b'C', c)) => (2.0 * x - c.0, 2.0 * y - c.1),
                        _ => (x, y),
                    }
                };
                let c2 = (n!() + ox, n!() + oy);
                let p = (n!() + ox, n!() + oy);
                if cur.is_empty() {
                    cur.push((x, y));
                }
                cubic(&mut cur, (x, y), c1, c2, p, tol);
                last_ctrl = Some((b'C', c2));
                x = p.0;
                y = p.1;
            }
            b'Q' | b'T' => {
                let upper = cmd.to_ascii_uppercase();
                let c = if upper == b'Q' {
                    (n!() + ox, n!() + oy)
                } else {
                    match last_ctrl {
                        Some((b'Q', c)) => (2.0 * x - c.0, 2.0 * y - c.1),
                        _ => (x, y),
                    }
                };
                let p = (n!() + ox, n!() + oy);
                if cur.is_empty() {
                    cur.push((x, y));
                }
                quad(&mut cur, (x, y), c, p, tol);
                last_ctrl = Some((b'Q', c));
                x = p.0;
                y = p.1;
            }
            b'A' => {
                let rx = n!();
                let ry = n!();
                let rot = n!();
                let Some(large) = next_num(&mut i, true) else { break };
                let Some(sweep) = next_num(&mut i, true) else { break };
                let p = (n!() + ox, n!() + oy);
                if cur.is_empty() {
                    cur.push((x, y));
                }
                arc(&mut cur, (x, y), rx, ry, rot, large != 0.0, sweep != 0.0, p);
                x = p.0;
                y = p.1;
                last_ctrl = None;
            }
            _ => {
                i += 1;
            }
        }
    }
    if !cur.is_empty() {
        flush(&mut cur, &mut polys, false);
    }
    polys
}

fn ellipse(cx: f32, cy: f32, rx: f32, ry: f32) -> Poly {
    let n = 72;
    let _ = rx.max(ry);
    let pts = (0..n).map(|i| {
        let t = i as f32 / n as f32 * std::f32::consts::TAU;
        (cx + rx * t.cos(), cy + ry * t.sin())
    });
    (pts.collect(), true)
}

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, rx: f32, ry: f32) -> Poly {
    if rx <= 0.0 || ry <= 0.0 {
        return (vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)], true);
    }
    let mut pts = Vec::new();
    let corners = [(x + w - rx, y + ry, -90.0f32), (x + w - rx, y + h - ry, 0.0), (x + rx, y + h - ry, 90.0), (x + rx, y + ry, 180.0)];
    for (cx, cy, a0) in corners {
        for k in 0..=12 {
            let t = (a0 + 90.0 * k as f32 / 12.0).to_radians();
            pts.push((cx + rx * t.cos(), cy + ry * t.sin()));
        }
    }
    (pts, true)
}

fn geometry(doc: &Document, n: NodeId, tag: &str, fs: f32, vb: (f32, f32)) -> Vec<Poly> {
    let a = |k: &str, base: f32| doc.attr(n, k).and_then(|v| length(v, fs, base)).unwrap_or(0.0);
    let diag = ((vb.0 * vb.0 + vb.1 * vb.1) / 2.0).sqrt();
    match tag {
        "path" => doc.attr(n, "d").map(path_data).unwrap_or_default(),
        "rect" => {
            let (w, h) = (a("width", vb.0), a("height", vb.1));
            if w <= 0.0 || h <= 0.0 {
                return vec![];
            }
            let rx_attr = doc.attr(n, "rx").and_then(|v| length(v, fs, vb.0));
            let ry_attr = doc.attr(n, "ry").and_then(|v| length(v, fs, vb.1));
            let rx = rx_attr.or(ry_attr).unwrap_or(0.0).min(w / 2.0);
            let ry = ry_attr.or(rx_attr).unwrap_or(0.0).min(h / 2.0);
            vec![rounded_rect(a("x", vb.0), a("y", vb.1), w, h, rx, ry)]
        }
        "circle" => {
            let r = a("r", diag);
            if r <= 0.0 { vec![] } else { vec![ellipse(a("cx", vb.0), a("cy", vb.1), r, r)] }
        }
        "ellipse" => {
            let (rx, ry) = (a("rx", vb.0), a("ry", vb.1));
            if rx <= 0.0 || ry <= 0.0 { vec![] } else { vec![ellipse(a("cx", vb.0), a("cy", vb.1), rx, ry)] }
        }
        "line" => vec![(vec![(a("x1", vb.0), a("y1", vb.1)), (a("x2", vb.0), a("y2", vb.1))], false)],
        "polyline" | "polygon" => {
            let v = numbers(doc.attr(n, "points").unwrap_or(""));
            let pts: Vec<Pt> = v.chunks_exact(2).map(|c| (c[0], c[1])).collect();
            if pts.len() < 2 { vec![] } else { vec![(pts, tag == "polygon")] }
        }
        _ => vec![],
    }
}

// ---------------------------------------------------------------- stroking

fn signed_area(p: &[Pt]) -> f32 {
    let mut a = 0.0;
    for i in 0..p.len() {
        let (x0, y0) = p[i];
        let (x1, y1) = p[(i + 1) % p.len()];
        a += x0 * y1 - x1 * y0;
    }
    a / 2.0
}

/// Append a polygon with consistent (positive) orientation so overlapping pieces union.
fn push_poly(out: &mut Vec<Seg>, mut p: Vec<Pt>) {
    if p.len() < 3 {
        return;
    }
    if signed_area(&p) < 0.0 {
        p.reverse();
    }
    out.push(Seg::Move(p[0].0, p[0].1));
    for q in &p[1..] {
        out.push(Seg::Line(q.0, q.1));
    }
    out.push(Seg::Close);
}

fn disc(c: Pt, r: f32) -> Vec<Pt> {
    let n = ((r * 4.0).ceil() as usize).clamp(12, 64);
    (0..n).map(|i| {
        let t = i as f32 / n as f32 * std::f32::consts::TAU;
        (c.0 + r * t.cos(), c.1 + r * t.sin())
    }).collect()
}

fn stroke(poly: &Poly, hw: f32, cap: u8, join: u8, miter: f32, out: &mut Vec<Seg>) {
    let (pts, closed) = poly;
    let mut p: Vec<Pt> = Vec::with_capacity(pts.len());
    for &q in pts {
        if p.last().is_none_or(|l: &Pt| (l.0 - q.0).abs() > 1e-4 || (l.1 - q.1).abs() > 1e-4) {
            p.push(q);
        }
    }
    if *closed && p.len() > 2 && p.first() == p.last() {
        p.pop();
    }
    if p.len() == 1 {
        // zero-length subpath: round/square caps still draw a dot
        match cap {
            1 => push_poly(out, disc(p[0], hw)),
            2 => push_poly(out, vec![(p[0].0 - hw, p[0].1 - hw), (p[0].0 + hw, p[0].1 - hw), (p[0].0 + hw, p[0].1 + hw), (p[0].0 - hw, p[0].1 + hw)]),
            _ => {}
        }
        return;
    }
    let n = p.len();
    let segs = if *closed { n } else { n - 1 };
    let dir = |a: Pt, b: Pt| {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let l = (dx * dx + dy * dy).sqrt().max(1e-6);
        (dx / l, dy / l)
    };
    for i in 0..segs {
        let a = p[i];
        let b = p[(i + 1) % n];
        let (dx, dy) = dir(a, b);
        let (nx, ny) = (-dy * hw, dx * hw);
        let (mut a2, mut b2) = (a, b);
        if !closed && cap == 2 {
            if i == 0 {
                a2 = (a.0 - dx * hw, a.1 - dy * hw);
            }
            if i == segs - 1 {
                b2 = (b.0 + dx * hw, b.1 + dy * hw);
            }
        }
        push_poly(out, vec![(a2.0 + nx, a2.1 + ny), (b2.0 + nx, b2.1 + ny), (b2.0 - nx, b2.1 - ny), (a2.0 - nx, a2.1 - ny)]);
    }
    // joins
    let joins: Vec<usize> = if *closed { (0..n).collect() } else { (1..n - 1).collect() };
    for i in joins {
        let v = p[i];
        let prev = p[(i + n - 1) % n];
        let next = p[(i + 1) % n];
        let d0 = dir(prev, v);
        let d1 = dir(v, next);
        let cross = d0.0 * d1.1 - d0.1 * d1.0;
        if cross.abs() < 1e-6 && d0.0 * d1.0 + d0.1 * d1.1 > 0.0 {
            continue;
        }
        if join == 1 {
            push_poly(out, disc(v, hw));
            continue;
        }
        // outer side of the turn
        let s = if cross > 0.0 { -1.0 } else { 1.0 };
        let n0 = (-d0.1 * hw * s, d0.0 * hw * s);
        let n1 = (-d1.1 * hw * s, d1.0 * hw * s);
        let o0 = (v.0 + n0.0, v.1 + n0.1);
        let o1 = (v.0 + n1.0, v.1 + n1.1);
        let mut poly = vec![v, o0];
        if join == 0 {
            let cos = (d0.0 * d1.0 + d0.1 * d1.1).clamp(-1.0, 1.0);
            let half = ((1.0 - cos) / 2.0).sqrt(); // sin(theta/2) with theta the turn angle
            let ratio = if half > 1e-6 { 1.0 / ((1.0 + cos) / 2.0).sqrt().max(1e-6) } else { f32::INFINITY };
            if ratio <= miter {
                let bis = (n0.0 + n1.0, n0.1 + n1.1);
                let bl = (bis.0 * bis.0 + bis.1 * bis.1).sqrt().max(1e-6);
                let len = hw * ratio;
                poly.push((v.0 + bis.0 / bl * len, v.1 + bis.1 / bl * len));
            }
        }
        poly.push(o1);
        push_poly(out, poly);
    }
    if !closed && cap == 1 {
        push_poly(out, disc(p[0], hw));
        push_poly(out, disc(p[n - 1], hw));
    }
}

// ---------------------------------------------------------------- rendering

fn resolve(paint: &Paint, ctx: &Ctx, alpha: f32) -> Option<Color> {
    let c = match paint {
        Paint::None => return None,
        Paint::Color(c) => *c,
        Paint::Current => ctx.color,
    };
    let a = (c.a as f32 * alpha * ctx.opacity).round().clamp(0.0, 255.0) as u8;
    (a > 0).then_some(Color { a, ..c })
}

fn walk(doc: &Document, n: NodeId, ctx: &Ctx, vb: (f32, f32), depth: usize, out: &mut Vec<Shape>) {
    if depth > 32 {
        return;
    }
    for &c in doc.children(n) {
        let Some(tag) = doc.tag(c) else { continue };
        let tag = tag.to_ascii_lowercase();
        if matches!(tag.as_str(), "defs" | "symbol" | "title" | "desc" | "metadata" | "style" | "clippath" | "mask" | "lineargradient" | "radialgradient" | "pattern" | "filter" | "marker" | "script") {
            continue;
        }
        if doc.attr(c, "display").is_some_and(|d| d.trim() == "none") || doc.attr(c, "visibility").is_some_and(|d| d.trim() == "hidden") {
            continue;
        }
        let mut cx = ctx.clone();
        apply_props(doc, c, &mut cx);
        match tag.as_str() {
            "g" | "a" | "switch" => walk(doc, c, &cx, vb, depth + 1, out),
            "use" => {
                let href = doc.attr(c, "href").or_else(|| doc.attr(c, "xlink:href")).unwrap_or("");
                if let Some(id) = href.strip_prefix('#') {
                    if let Some(target) = doc.element_descendants(0).into_iter().find(|&e| doc.attr(e, "id") == Some(id)) {
                        let x = doc.attr(c, "x").and_then(|v| length(v, cx.font_size, vb.0)).unwrap_or(0.0);
                        let y = doc.attr(c, "y").and_then(|v| length(v, cx.font_size, vb.1)).unwrap_or(0.0);
                        cx.m = cx.m.mul(Mat([1.0, 0.0, 0.0, 1.0, x, y]));
                        let mut tctx = cx.clone();
                        apply_props(doc, target, &mut tctx);
                        let ttag = doc.tag(target).unwrap_or("").to_ascii_lowercase();
                        if ttag == "symbol" || ttag == "svg" {
                            if let Some(v) = view_box(doc, target) {
                                let w = doc.attr(c, "width").and_then(|s| length(s, cx.font_size, vb.0)).unwrap_or(vb.0);
                                let h = doc.attr(c, "height").and_then(|s| length(s, cx.font_size, vb.1)).unwrap_or(vb.1);
                                tctx.m = tctx.m.mul(fit(v, Rect::new(0.0, 0.0, w, h)));
                            }
                            walk(doc, target, &tctx, vb, depth + 1, out);
                        } else {
                            shape(doc, target, &ttag, &tctx, vb, out);
                        }
                    }
                }
            }
            "svg" => {
                let x = doc.attr(c, "x").and_then(|v| length(v, cx.font_size, vb.0)).unwrap_or(0.0);
                let y = doc.attr(c, "y").and_then(|v| length(v, cx.font_size, vb.1)).unwrap_or(0.0);
                let (w, h) = intrinsic_size(doc, c, cx.font_size);
                if let Some(v) = view_box(doc, c) {
                    cx.m = cx.m.mul(fit(v, Rect::new(x, y, w, h)));
                    walk(doc, c, &cx, (v[2], v[3]), depth + 1, out);
                } else {
                    cx.m = cx.m.mul(Mat([1.0, 0.0, 0.0, 1.0, x, y]));
                    walk(doc, c, &cx, (w, h), depth + 1, out);
                }
            }
            "text" => {}
            t => shape(doc, c, t, &cx, vb, out),
        }
    }
}

fn shape(doc: &Document, n: NodeId, tag: &str, ctx: &Ctx, vb: (f32, f32), out: &mut Vec<Shape>) {
    let polys = geometry(doc, n, tag, ctx.font_size, vb);
    if polys.is_empty() {
        return;
    }
    let tx: Vec<Poly> = polys.iter().map(|(p, c)| (p.iter().map(|&q| ctx.m.apply(q)).collect(), *c)).collect();
    if !matches!(tag, "line" | "polyline") {
        if let Some(color) = resolve(&ctx.fill, ctx, ctx.fill_opacity) {
            let mut segs = Vec::new();
            for (p, _) in &tx {
                if p.len() < 3 {
                    continue;
                }
                segs.push(Seg::Move(p[0].0, p[0].1));
                for q in &p[1..] {
                    segs.push(Seg::Line(q.0, q.1));
                }
                segs.push(Seg::Close);
            }
            if !segs.is_empty() {
                out.push(Shape { segs, color });
            }
        }
    } else if let (Some(color), "polyline") = (resolve(&ctx.fill, ctx, ctx.fill_opacity), tag) {
        // polylines fill like polygons when they have a fill
        let mut segs = Vec::new();
        for (p, _) in &tx {
            if p.len() >= 3 {
                segs.push(Seg::Move(p[0].0, p[0].1));
                for q in &p[1..] {
                    segs.push(Seg::Line(q.0, q.1));
                }
                segs.push(Seg::Close);
            }
        }
        if !segs.is_empty() {
            out.push(Shape { segs, color });
        }
    }
    if let Some(color) = resolve(&ctx.stroke, ctx, ctx.stroke_opacity) {
        let hw = ctx.stroke_width * ctx.m.scale() / 2.0;
        if hw > 0.0 {
            let mut segs = Vec::new();
            for p in &tx {
                stroke(p, hw, ctx.cap, ctx.join, ctx.miter, &mut segs);
            }
            if !segs.is_empty() {
                out.push(Shape { segs, color });
            }
        }
    }
}

/// viewBox → viewport transform (preserveAspectRatio xMidYMid meet).
fn fit(v: [f32; 4], r: Rect) -> Mat {
    let s = (r.w / v[2]).min(r.h / v[3]);
    let tx = r.x + (r.w - v[2] * s) / 2.0 - v[0] * s;
    let ty = r.y + (r.h - v[3] * s) / 2.0 - v[1] * s;
    Mat([s, 0.0, 0.0, s, tx, ty])
}

fn fit_par(doc: &Document, n: NodeId, v: [f32; 4], r: Rect) -> Mat {
    let par = doc.attr(n, "preserveAspectRatio").unwrap_or("xMidYMid meet").trim().to_string();
    if par.starts_with("none") {
        let (sx, sy) = (r.w / v[2], r.h / v[3]);
        return Mat([sx, 0.0, 0.0, sy, r.x - v[0] * sx, r.y - v[1] * sy]);
    }
    let slice = par.contains("slice");
    let s = if slice { (r.w / v[2]).max(r.h / v[3]) } else { (r.w / v[2]).min(r.h / v[3]) };
    let fx = if par.contains("xMin") { 0.0 } else if par.contains("xMax") { 1.0 } else { 0.5 };
    let fy = if par.contains("YMin") { 0.0 } else if par.contains("YMax") { 1.0 } else { 0.5 };
    let tx = r.x + (r.w - v[2] * s) * fx - v[0] * s;
    let ty = r.y + (r.h - v[3] * s) * fy - v[1] * s;
    Mat([s, 0.0, 0.0, s, tx, ty])
}

/// Render the `<svg>` element `n` into `rect` (its content box). `color` is `currentColor`,
/// `fill`/`stroke` are the element's computed values when set by CSS.
pub fn render(doc: &Document, n: NodeId, rect: Rect, color: Color, font_size: f32, css: &SvgCss) -> Vec<Shape> {
    let mut ctx = Ctx {
        m: Mat::ID,
        fill: Paint::Color(Color::BLACK),
        stroke: Paint::None,
        stroke_width: 1.0,
        cap: 0,
        join: 0,
        miter: 4.0,
        opacity: 1.0,
        fill_opacity: 1.0,
        stroke_opacity: 1.0,
        color,
        font_size,
    };
    apply_props(doc, n, &mut ctx);
    css.apply(&mut ctx);
    ctx.m = match view_box(doc, n) {
        Some(v) => fit_par(doc, n, v, rect),
        None => Mat([1.0, 0.0, 0.0, 1.0, rect.x, rect.y]),
    };
    let vb = view_box(doc, n).map(|v| (v[2], v[3])).unwrap_or((rect.w, rect.h));
    let mut out = Vec::new();
    walk(doc, n, &ctx, vb, 0, &mut out);
    out
}

/// SVG paint properties set on the root `<svg>` by CSS (class rules like `.icon { stroke: … }`).
#[derive(Default, Clone, Debug, PartialEq)]
pub struct SvgCss {
    pub fill: Option<String>,
    pub stroke: Option<String>,
    pub stroke_width: Option<String>,
}

impl SvgCss {
    fn apply(&self, ctx: &mut Ctx) {
        if let Some(p) = self.fill.as_deref().and_then(parse_paint) {
            ctx.fill = p;
        }
        if let Some(p) = self.stroke.as_deref().and_then(parse_paint) {
            ctx.stroke = p;
        }
        if let Some(w) = self.stroke_width.as_deref().and_then(|v| length(v, ctx.font_size, 24.0)) {
            ctx.stroke_width = w;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_parsing() {
        let p = path_data("M18 6 6 18M6 6l12 12");
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].0, vec![(18.0, 6.0), (6.0, 18.0)]);
        assert_eq!(p[1].0, vec![(6.0, 6.0), (18.0, 18.0)]);
        let q = path_data("M0 0h10v10H0z");
        assert_eq!(q.len(), 1);
        assert!(q[0].1);
        assert_eq!(numbers("1.5.5-2e1"), vec![1.5, 0.5, -20.0]);
        let a = path_data("M10 10a5 5 0 1 0 10 0");
        assert!(a[0].0.len() > 4);
    }
}
