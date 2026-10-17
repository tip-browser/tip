//! Minimal TrueType (glyf-flavoured OpenType) font parser.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy)]
pub enum Seg {
    Move(f32, f32),
    Line(f32, f32),
    Quad(f32, f32, f32, f32),
    Close,
}

pub struct Font {
    data: Vec<u8>,
    glyf: usize,
    loca: usize,
    hmtx: usize,
    cmap_sub: usize,
    cmap_format: u16,
    pub units_per_em: f32,
    num_hmetrics: u16,
    num_glyphs: u16,
    loca_long: bool,
    pub ascender: f32,
    pub descender: f32,
    pub line_gap: f32,
    kern: HashMap<u32, i16>,
}

fn rd16(d: &[u8], o: usize) -> u16 {
    if o + 2 > d.len() {
        return 0;
    }
    u16::from_be_bytes([d[o], d[o + 1]])
}
fn rdi16(d: &[u8], o: usize) -> i16 {
    rd16(d, o) as i16
}
fn rd32(d: &[u8], o: usize) -> u32 {
    if o + 4 > d.len() {
        return 0;
    }
    u32::from_be_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

impl Font {
    pub fn parse(data: Vec<u8>, index: u32) -> Result<Font, String> {
        let mut base = 0usize;
        if &data.get(0..4).ok_or("too short")? == b"ttcf" {
            let n = rd32(&data, 8);
            if index >= n {
                return Err("font index out of range".into());
            }
            base = rd32(&data, 12 + 4 * index as usize) as usize;
        }
        let version = rd32(&data, base);
        if version != 0x00010000 && version != 0x74727565 {
            return Err("not a TrueType outline font".into());
        }
        let num_tables = rd16(&data, base + 4) as usize;
        let mut tables: HashMap<[u8; 4], (usize, usize)> = HashMap::new();
        for i in 0..num_tables {
            let r = base + 12 + 16 * i;
            if r + 16 > data.len() {
                break;
            }
            let tag = [data[r], data[r + 1], data[r + 2], data[r + 3]];
            tables.insert(tag, (rd32(&data, r + 8) as usize, rd32(&data, r + 12) as usize));
        }
        let t = |tag: &[u8; 4]| tables.get(tag).copied().ok_or_else(|| format!("missing table {}", String::from_utf8_lossy(tag)));
        let (head, _) = t(b"head")?;
        let (hhea, _) = t(b"hhea")?;
        let (maxp, _) = t(b"maxp")?;
        let (hmtx, _) = t(b"hmtx")?;
        let (cmap, _) = t(b"cmap")?;
        let (loca, _) = t(b"loca")?;
        let (glyf, _) = t(b"glyf")?;
        let units_per_em = rd16(&data, head + 18) as f32;
        let loca_long = rdi16(&data, head + 50) != 0;
        let mut ascender = rdi16(&data, hhea + 4) as f32;
        let mut descender = rdi16(&data, hhea + 6) as f32;
        let mut line_gap = rdi16(&data, hhea + 8) as f32;
        if let Ok((os2, len)) = t(b"OS/2") {
            // Prefer typo metrics when USE_TYPO_METRICS is set, else win metrics (what browsers do on Windows/mac roughly).
            if len >= 78 {
                let fs_selection = rd16(&data, os2 + 62);
                if fs_selection & (1 << 7) != 0 {
                    ascender = rdi16(&data, os2 + 68) as f32;
                    descender = rdi16(&data, os2 + 70) as f32;
                    line_gap = rdi16(&data, os2 + 72) as f32;
                }
            }
        }
        let num_hmetrics = rd16(&data, hhea + 34);
        let num_glyphs = rd16(&data, maxp + 4);

        // cmap: prefer full unicode (format 12), then BMP (format 4).
        let n_sub = rd16(&data, cmap + 2) as usize;
        let mut best: Option<(usize, u16, u8)> = None;
        for i in 0..n_sub {
            let r = cmap + 4 + 8 * i;
            let platform = rd16(&data, r);
            let enc = rd16(&data, r + 2);
            let off = cmap + rd32(&data, r + 4) as usize;
            let format = rd16(&data, off);
            let score = match (platform, enc, format) {
                (3, 10, 12) | (0, 4, 12) | (0, 6, 12) => 4,
                (0, _, 12) => 3,
                (3, 1, 4) => 2,
                (0, _, 4) => 1,
                _ => 0,
            };
            if score > 0 && best.is_none_or(|b| score > b.2) {
                best = Some((off, format, score));
            }
        }
        let (cmap_sub, cmap_format, _) = best.ok_or("no usable cmap")?;

        let mut kern = HashMap::new();
        if let Ok((k, _)) = t(b"kern") {
            let n = rd16(&data, k + 2);
            let mut off = k + 4;
            for _ in 0..n {
                let len = rd16(&data, off + 2) as usize;
                let coverage = rd16(&data, off + 4);
                if coverage >> 8 == 0 && coverage & 1 != 0 {
                    let pairs = rd16(&data, off + 6) as usize;
                    for p in 0..pairs {
                        let r = off + 14 + p * 6;
                        let key = ((rd16(&data, r) as u32) << 16) | rd16(&data, r + 2) as u32;
                        kern.insert(key, rdi16(&data, r + 4));
                    }
                }
                off += len.max(6);
            }
        }

        Ok(Font {
            data,
            glyf,
            loca,
            hmtx,
            cmap_sub,
            cmap_format,
            units_per_em,
            num_hmetrics,
            num_glyphs,
            loca_long,
            ascender,
            descender,
            line_gap,
            kern,
        })
    }

    pub fn glyph_index(&self, c: char) -> u16 {
        self.lookup_cmap(c as u32)
    }

    fn lookup_cmap(&self, cp: u32) -> u16 {
        let d = &self.data;
        let s = self.cmap_sub;
        match self.cmap_format {
            4 => {
                if cp > 0xffff {
                    return 0;
                }
                let cp = cp as u16;
                let segx2 = rd16(d, s + 6) as usize;
                let ends = s + 14;
                let starts = ends + segx2 + 2;
                let deltas = starts + segx2;
                let range_offs = deltas + segx2;
                // binary search over end codes
                let (mut lo, mut hi) = (0usize, segx2 / 2);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    if rd16(d, ends + mid * 2) < cp {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                }
                let i = lo;
                if i >= segx2 / 2 {
                    return 0;
                }
                let start = rd16(d, starts + i * 2);
                if cp < start {
                    return 0;
                }
                let delta = rd16(d, deltas + i * 2);
                let ro = rd16(d, range_offs + i * 2);
                if ro == 0 {
                    cp.wrapping_add(delta)
                } else {
                    let addr = range_offs + i * 2 + ro as usize + (cp - start) as usize * 2;
                    let g = rd16(d, addr);
                    if g == 0 { 0 } else { g.wrapping_add(delta) }
                }
            }
            12 => {
                let n = rd32(d, s + 12) as usize;
                let (mut lo, mut hi) = (0usize, n);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    let r = s + 16 + mid * 12;
                    let start = rd32(d, r);
                    let end = rd32(d, r + 4);
                    if cp < start {
                        hi = mid;
                    } else if cp > end {
                        lo = mid + 1;
                    } else {
                        return (rd32(d, r + 8) + (cp - start)) as u16;
                    }
                }
                0
            }
            _ => 0,
        }
    }

    pub fn advance(&self, gid: u16) -> f32 {
        let i = gid.min(self.num_hmetrics.saturating_sub(1)) as usize;
        rd16(&self.data, self.hmtx + i * 4) as f32
    }

    pub fn kerning(&self, a: u16, b: u16) -> f32 {
        if self.kern.is_empty() {
            return 0.0;
        }
        self.kern.get(&(((a as u32) << 16) | b as u32)).copied().unwrap_or(0) as f32
    }

    fn glyph_range(&self, gid: u16) -> Option<(usize, usize)> {
        if gid >= self.num_glyphs {
            return None;
        }
        let (a, b) = if self.loca_long {
            (rd32(&self.data, self.loca + gid as usize * 4) as usize, rd32(&self.data, self.loca + gid as usize * 4 + 4) as usize)
        } else {
            (rd16(&self.data, self.loca + gid as usize * 2) as usize * 2, rd16(&self.data, self.loca + gid as usize * 2 + 2) as usize * 2)
        };
        if b <= a {
            return None;
        }
        Some((self.glyf + a, self.glyf + b))
    }

    /// Glyph outline in font units (y up).
    pub fn outline(&self, gid: u16) -> Vec<Seg> {
        let mut out = Vec::new();
        self.outline_into(gid, &mut out, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], 0);
        out
    }

    fn outline_into(&self, gid: u16, out: &mut Vec<Seg>, m: [f32; 6], depth: u32) {
        if depth > 8 {
            return;
        }
        let Some((start, end)) = self.glyph_range(gid) else { return };
        let d = &self.data;
        if end > d.len() {
            return;
        }
        let n_contours = rdi16(d, start);
        let tf = |x: f32, y: f32| (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]);
        if n_contours >= 0 {
            let n_contours = n_contours as usize;
            let mut ends = Vec::with_capacity(n_contours);
            for i in 0..n_contours {
                ends.push(rd16(d, start + 10 + i * 2) as usize);
            }
            let n_points = ends.last().map(|e| e + 1).unwrap_or(0);
            let ins_len = rd16(d, start + 10 + n_contours * 2) as usize;
            let mut p = start + 12 + n_contours * 2 + ins_len;
            let mut flags = Vec::with_capacity(n_points);
            while flags.len() < n_points && p < end {
                let f = d[p];
                p += 1;
                flags.push(f);
                if f & 8 != 0 && p < end {
                    let r = d[p];
                    p += 1;
                    for _ in 0..r {
                        flags.push(f);
                    }
                }
            }
            flags.truncate(n_points);
            let mut xs = Vec::with_capacity(n_points);
            let mut v: i32 = 0;
            for &f in &flags {
                if f & 2 != 0 {
                    let dx = d.get(p).copied().unwrap_or(0) as i32;
                    p += 1;
                    v += if f & 16 != 0 { dx } else { -dx };
                } else if f & 16 == 0 {
                    v += rdi16(d, p) as i32;
                    p += 2;
                }
                xs.push(v);
            }
            let mut ys = Vec::with_capacity(n_points);
            v = 0;
            for &f in &flags {
                if f & 4 != 0 {
                    let dy = d.get(p).copied().unwrap_or(0) as i32;
                    p += 1;
                    v += if f & 32 != 0 { dy } else { -dy };
                } else if f & 32 == 0 {
                    v += rdi16(d, p) as i32;
                    p += 2;
                }
                ys.push(v);
            }
            if xs.len() < n_points || ys.len() < n_points {
                return;
            }
            let mut s = 0;
            for &e in &ends {
                if e < s || e >= n_points {
                    break;
                }
                let pts: Vec<(f32, f32, bool)> = (s..=e).map(|i| {
                    let (x, y) = tf(xs[i] as f32, ys[i] as f32);
                    (x, y, flags[i] & 1 != 0)
                }).collect();
                emit_contour(&pts, out);
                s = e + 1;
            }
        } else {
            // composite
            let mut p = start + 10;
            loop {
                let flags = rd16(d, p);
                let gi = rd16(d, p + 2);
                p += 4;
                let (dx, dy) = if flags & 1 != 0 {
                    let r = (rdi16(d, p) as f32, rdi16(d, p + 2) as f32);
                    p += 4;
                    r
                } else {
                    let r = (d[p] as i8 as f32, d[p + 1] as i8 as f32);
                    p += 2;
                    r
                };
                let f2 = |v: i16| v as f32 / 16384.0;
                let (mut a, mut b, mut c, mut dd) = (1.0, 0.0, 0.0, 1.0);
                if flags & 8 != 0 {
                    a = f2(rdi16(d, p));
                    dd = a;
                    p += 2;
                } else if flags & 0x40 != 0 {
                    a = f2(rdi16(d, p));
                    dd = f2(rdi16(d, p + 2));
                    p += 4;
                } else if flags & 0x80 != 0 {
                    a = f2(rdi16(d, p));
                    b = f2(rdi16(d, p + 2));
                    c = f2(rdi16(d, p + 4));
                    dd = f2(rdi16(d, p + 6));
                    p += 8;
                }
                let (dx, dy) = if flags & 2 != 0 { (dx, dy) } else { (0.0, 0.0) };
                // compose: m * [a b c d dx dy]
                let cm = [
                    m[0] * a + m[2] * b,
                    m[1] * a + m[3] * b,
                    m[0] * c + m[2] * dd,
                    m[1] * c + m[3] * dd,
                    m[0] * dx + m[2] * dy + m[4],
                    m[1] * dx + m[3] * dy + m[5],
                ];
                self.outline_into(gi, out, cm, depth + 1);
                if flags & 0x20 == 0 || p >= end {
                    break;
                }
            }
        }
    }
}

fn emit_contour(pts: &[(f32, f32, bool)], out: &mut Vec<Seg>) {
    let n = pts.len();
    if n == 0 {
        return;
    }
    // Find a starting on-curve point (or synthesize one).
    let start_idx = pts.iter().position(|p| p.2);
    let (sx, sy, rot) = match start_idx {
        Some(i) => (pts[i].0, pts[i].1, i),
        None => {
            let a = pts[0];
            let b = pts[1 % n];
            ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0, 0)
        }
    };
    out.push(Seg::Move(sx, sy));
    let mut ctrl: Option<(f32, f32)> = None;
    let order: Vec<(f32, f32, bool)> = if start_idx.is_some() {
        (1..=n).map(|k| pts[(rot + k) % n]).collect()
    } else {
        (1..=n).map(|k| pts[k % n]).chain(std::iter::once((sx, sy, true))).collect()
    };
    for (x, y, on) in order {
        if on {
            match ctrl.take() {
                Some((cx, cy)) => out.push(Seg::Quad(cx, cy, x, y)),
                None => out.push(Seg::Line(x, y)),
            }
        } else {
            if let Some((cx, cy)) = ctrl {
                let mx = (cx + x) / 2.0;
                let my = (cy + y) / 2.0;
                out.push(Seg::Quad(cx, cy, mx, my));
            }
            ctrl = Some((x, y));
        }
    }
    if let Some((cx, cy)) = ctrl {
        out.push(Seg::Quad(cx, cy, sx, sy));
    }
    out.push(Seg::Close);
}
