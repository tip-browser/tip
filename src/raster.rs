//! Software rasterizer: anti-aliased path filling and a pixel canvas.
//!
//! Path coverage uses the signed-area accumulation technique: each line
//! segment deposits area/coverage deltas into a buffer which is then
//! prefix-summed per row.

use crate::css::Color;
use crate::text::ttf::Seg;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
    pub fn intersect(&self, o: &Rect) -> Rect {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        Rect::new(x, y, (r - x).max(0.0), (b - y).max(0.0))
    }
    pub fn union(&self, o: &Rect) -> Rect {
        if self.w <= 0.0 && self.h <= 0.0 {
            return *o;
        }
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        let r = self.right().max(o.right());
        let b = self.bottom().max(o.bottom());
        Rect::new(x, y, r - x, b - y)
    }
    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
    pub fn translate(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }
    pub fn inflate(&self, d: f32) -> Rect {
        Rect::new(self.x - d, self.y - d, self.w + 2.0 * d, self.h + 2.0 * d)
    }
    pub fn scale(&self, s: f32) -> Rect {
        Rect::new(self.x * s, self.y * s, self.w * s, self.h * s)
    }
}

/// Coverage accumulator for a w×h region.
pub struct Accum {
    pub w: usize,
    pub h: usize,
    stride: usize,
    a: Vec<f32>,
}

impl Accum {
    pub fn new(w: usize, h: usize) -> Accum {
        let stride = w + 3;
        Accum { w, h, stride, a: vec![0.0; stride * h + 4] }
    }

    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) {
        if (y0 - y1).abs() <= f32::EPSILON {
            return;
        }
        let wmax = self.w as f32;
        let x0 = x0.clamp(0.0, wmax);
        let x1 = x1.clamp(0.0, wmax);
        let (dir, ax, ay, bx, by) = if y0 < y1 { (1.0, x0, y0, x1, y1) } else { (-1.0, x1, y1, x0, y0) };
        let dxdy = (bx - ax) / (by - ay);
        let mut x = ax;
        let ystart = if ay < 0.0 {
            x -= ay * dxdy;
            0
        } else {
            ay as usize
        };
        let yend = (self.h).min(by.ceil().max(0.0) as usize);
        for y in ystart..yend {
            let linestart = y * self.stride;
            let dy = ((y + 1) as f32).min(by) - (y as f32).max(ay);
            let xnext = x + dxdy * dy;
            let d = dy * dir;
            let (xa, xb) = if x < xnext { (x, xnext) } else { (xnext, x) };
            let xaf = xa.floor();
            let xai = xaf as usize;
            let xbc = xb.ceil();
            let xbi = xbc as usize;
            if xbi <= xai + 1 {
                let xmf = 0.5 * (x + xnext) - xaf;
                self.a[linestart + xai] += d - d * xmf;
                self.a[linestart + xai + 1] += d * xmf;
            } else {
                let s = (xb - xa).recip();
                let x0f = xa - xaf;
                let a0 = 0.5 * s * (1.0 - x0f) * (1.0 - x0f);
                let x1f = xb - xbc + 1.0;
                let am = 0.5 * s * x1f * x1f;
                self.a[linestart + xai] += d * a0;
                if xbi == xai + 2 {
                    self.a[linestart + xai + 1] += d * (1.0 - a0 - am);
                } else {
                    let a1 = s * (1.5 - x0f);
                    self.a[linestart + xai + 1] += d * (a1 - a0);
                    for xi in xai + 2..xbi - 1 {
                        self.a[linestart + xi] += d * s;
                    }
                    let a2 = a1 + (xbi - xai - 3) as f32 * s;
                    self.a[linestart + xbi - 1] += d * (1.0 - a2 - am);
                }
                self.a[linestart + xbi] += d * am;
            }
            x = xnext;
        }
    }

    pub fn quad(&mut self, x0: f32, y0: f32, cx: f32, cy: f32, x1: f32, y1: f32) {
        let devx = x0 - 2.0 * cx + x1;
        let devy = y0 - 2.0 * cy + y1;
        let dd = devx * devx + devy * devy;
        let n = (1.0 + (3.0 * dd.sqrt()).sqrt().floor()).min(32.0) as usize;
        let mut px = x0;
        let mut py = y0;
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let mt = 1.0 - t;
            let nx = mt * mt * x0 + 2.0 * mt * t * cx + t * t * x1;
            let ny = mt * mt * y0 + 2.0 * mt * t * cy + t * t * y1;
            self.line(px, py, nx, ny);
            px = nx;
            py = ny;
        }
    }

    pub fn path(&mut self, segs: &[Seg], ox: f32, oy: f32) {
        let (mut sx, mut sy, mut px, mut py) = (0.0, 0.0, 0.0, 0.0);
        for s in segs {
            match *s {
                Seg::Move(x, y) => {
                    if (px, py) != (sx, sy) {
                        self.line(px - ox, py - oy, sx - ox, sy - oy);
                    }
                    sx = x;
                    sy = y;
                    px = x;
                    py = y;
                }
                Seg::Line(x, y) => {
                    self.line(px - ox, py - oy, x - ox, y - oy);
                    px = x;
                    py = y;
                }
                Seg::Quad(cx, cy, x, y) => {
                    self.quad(px - ox, py - oy, cx - ox, cy - oy, x - ox, y - oy);
                    px = x;
                    py = y;
                }
                Seg::Close => {
                    self.line(px - ox, py - oy, sx - ox, sy - oy);
                    px = sx;
                    py = sy;
                }
            }
        }
        if (px, py) != (sx, sy) {
            self.line(px - ox, py - oy, sx - ox, sy - oy);
        }
    }

    /// Produce an 8-bit coverage mask.
    pub fn finish(self) -> Vec<u8> {
        let mut out = vec![0u8; self.w * self.h];
        for y in 0..self.h {
            let mut acc = 0.0f32;
            let src = &self.a[y * self.stride..y * self.stride + self.w];
            let dst = &mut out[y * self.w..y * self.w + self.w];
            for (o, v) in dst.iter_mut().zip(src) {
                acc += v;
                *o = (acc.abs().min(1.0) * 255.0 + 0.5) as u8;
            }
        }
        out
    }
}

/// Build a rounded-rectangle path. Radii: [tl, tr, br, bl].
pub fn rounded_rect_path(r: Rect, radii: [f32; 4]) -> Vec<Seg> {
    let max = (r.w / 2.0).min(r.h / 2.0).max(0.0);
    let [tl, tr, br, bl] = radii.map(|v| v.clamp(0.0, max));
    // Approximate quarter circles with two quadratic segments each.
    let k = 0.4142; // tan(pi/8)
    let mut p = Vec::with_capacity(20);
    let (x0, y0, x1, y1) = (r.x, r.y, r.right(), r.bottom());
    p.push(Seg::Move(x0 + tl, y0));
    p.push(Seg::Line(x1 - tr, y0));
    if tr > 0.0 {
        let (cx, cy) = (x1 - tr, y0 + tr);
        let m = std::f32::consts::FRAC_1_SQRT_2;
        p.push(Seg::Quad(cx + tr * k, y0, cx + tr * m, cy - tr * m));
        p.push(Seg::Quad(x1, cy - tr * k, x1, cy));
    }
    p.push(Seg::Line(x1, y1 - br));
    if br > 0.0 {
        let (cx, cy) = (x1 - br, y1 - br);
        let m = std::f32::consts::FRAC_1_SQRT_2;
        p.push(Seg::Quad(x1, cy + br * k, cx + br * m, cy + br * m));
        p.push(Seg::Quad(cx + br * k, y1, cx, y1));
    }
    p.push(Seg::Line(x0 + bl, y1));
    if bl > 0.0 {
        let (cx, cy) = (x0 + bl, y1 - bl);
        let m = std::f32::consts::FRAC_1_SQRT_2;
        p.push(Seg::Quad(cx - bl * k, y1, cx - bl * m, cy + bl * m));
        p.push(Seg::Quad(x0, cy + bl * k, x0, cy));
    }
    p.push(Seg::Line(x0, y0 + tl));
    if tl > 0.0 {
        let (cx, cy) = (x0 + tl, y0 + tl);
        let m = std::f32::consts::FRAC_1_SQRT_2;
        p.push(Seg::Quad(x0, cy - tl * k, cx - tl * m, cy - tl * m));
        p.push(Seg::Quad(cx - tl * k, y0, cx, y0));
    }
    p.push(Seg::Close);
    p
}

pub fn ellipse_path(cx: f32, cy: f32, rx: f32, ry: f32) -> Vec<Seg> {
    rounded_rect_path(Rect::new(cx - rx, cy - ry, rx * 2.0, ry * 2.0), [rx.min(ry); 4])
}

pub fn reverse_path(segs: &[Seg]) -> Vec<Seg> {
    // Only used for even-odd style "ring" shapes built from two closed contours where
    // winding direction matters; our accumulator uses abs(nonzero) so we reverse the inner one.
    let mut pts: Vec<(f32, f32)> = Vec::new();
    let mut out = Vec::new();
    let mut ops: Vec<Seg> = Vec::new();
    let mut cur = (0.0, 0.0);
    for s in segs {
        match *s {
            Seg::Move(x, y) => {
                cur = (x, y);
                pts.push(cur);
            }
            Seg::Line(x, y) => {
                ops.push(Seg::Line(cur.0, cur.1));
                cur = (x, y);
            }
            Seg::Quad(cx, cy, x, y) => {
                ops.push(Seg::Quad(cx, cy, cur.0, cur.1));
                cur = (x, y);
            }
            Seg::Close => {}
        }
    }
    out.push(Seg::Move(cur.0, cur.1));
    for op in ops.into_iter().rev() {
        out.push(op);
    }
    out.push(Seg::Close);
    out
}

#[derive(Clone)]
pub struct Image {
    pub w: u32,
    pub h: u32,
    /// RGBA8, non-premultiplied
    pub data: Vec<u8>,
}

pub struct Canvas {
    pub w: usize,
    pub h: usize,
    /// 0x00RRGGBB
    pub px: Vec<u32>,
    clip_stack: Vec<Rect>,
    pub clip: Rect,
}

#[inline]
fn blend_px(dst: u32, c: Color, cov: u32) -> u32 {
    let a = (c.a as u32 * cov + 127) / 255;
    if a == 0 {
        return dst;
    }
    if a >= 255 {
        return ((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32;
    }
    let ia = 255 - a;
    let dr = (dst >> 16) & 0xff;
    let dg = (dst >> 8) & 0xff;
    let db = dst & 0xff;
    let r = (c.r as u32 * a + dr * ia + 127) / 255;
    let g = (c.g as u32 * a + dg * ia + 127) / 255;
    let b = (c.b as u32 * a + db * ia + 127) / 255;
    (r << 16) | (g << 8) | b
}

impl Canvas {
    pub fn new(w: usize, h: usize) -> Canvas {
        Canvas { w, h, px: vec![0xffffff; w * h], clip_stack: vec![], clip: Rect::new(0.0, 0.0, w as f32, h as f32) }
    }

    pub fn resize(&mut self, w: usize, h: usize) {
        self.w = w;
        self.h = h;
        self.px = vec![0xffffff; w * h];
        self.clip_stack.clear();
        self.clip = Rect::new(0.0, 0.0, w as f32, h as f32);
    }

    pub fn clear(&mut self, c: Color) {
        let v = ((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32;
        self.px.fill(v);
    }

    pub fn push_clip(&mut self, r: Rect) {
        self.clip_stack.push(self.clip);
        self.clip = self.clip.intersect(&r);
    }

    pub fn pop_clip(&mut self) {
        if let Some(c) = self.clip_stack.pop() {
            self.clip = c;
        }
    }

    fn clip_bounds(&self) -> (i32, i32, i32, i32) {
        (
            self.clip.x.floor().max(0.0) as i32,
            self.clip.y.floor().max(0.0) as i32,
            (self.clip.right().ceil() as i32).min(self.w as i32),
            (self.clip.bottom().ceil() as i32).min(self.h as i32),
        )
    }

    /// Pixel bounds (x0, y0, x1, y1) of a rect within the canvas and current clip.
    pub fn region(&self, r: Rect) -> (usize, usize, usize, usize) {
        let r = r.intersect(&self.clip);
        if r.is_empty() {
            return (0, 0, 0, 0);
        }
        let x0 = r.x.floor().max(0.0) as usize;
        let y0 = r.y.floor().max(0.0) as usize;
        let x1 = (r.right().ceil().max(0.0) as usize).min(self.w);
        let y1 = (r.bottom().ceil().max(0.0) as usize).min(self.h);
        (x0, y0, x1.max(x0), y1.max(y0))
    }

    /// Copy the pixels of a region (the backdrop of a group).
    pub fn save_region(&self, (x0, y0, x1, y1): (usize, usize, usize, usize)) -> Vec<u32> {
        let mut out = Vec::with_capacity((x1 - x0) * (y1 - y0));
        for y in y0..y1 {
            out.extend_from_slice(&self.px[y * self.w + x0..y * self.w + x1]);
        }
        out
    }

    /// Composite a group drawn over `saved` (its backdrop) with group opacity `op`:
    /// result = backdrop + op × (drawn − backdrop).
    pub fn blend_region(&mut self, saved: &[u32], (x0, y0, x1, y1): (usize, usize, usize, usize), op: f32) {
        let a = (op.clamp(0.0, 1.0) * 256.0) as u32;
        let w = x1 - x0;
        for y in y0..y1 {
            let row = &mut self.px[y * self.w + x0..y * self.w + x1];
            let back = &saved[(y - y0) * w..(y - y0 + 1) * w];
            for (d, &b) in row.iter_mut().zip(back) {
                if *d == b {
                    continue;
                }
                let mix = |sh: u32| {
                    let dv = (*d >> sh) & 0xff;
                    let bv = (b >> sh) & 0xff;
                    ((bv * (256 - a) + dv * a) >> 8) << sh
                };
                *d = mix(16) | mix(8) | mix(0);
            }
        }
    }

    /// Axis-aligned rect with fractional-edge anti-aliasing.
    pub fn fill_rect(&mut self, r: Rect, c: Color) {
        if c.a == 0 || r.is_empty() {
            return;
        }
        let r = r.intersect(&self.clip);
        if r.is_empty() {
            return;
        }
        let x0 = r.x.floor() as i32;
        let y0 = r.y.floor() as i32;
        let x1 = (r.right().ceil() as i32).min(self.w as i32);
        let y1 = (r.bottom().ceil() as i32).min(self.h as i32);
        for y in y0.max(0)..y1 {
            let cy = ((y + 1) as f32).min(r.bottom()) - (y as f32).max(r.y);
            if cy <= 0.0 {
                continue;
            }
            let row = y as usize * self.w;
            for x in x0.max(0)..x1 {
                let cx = ((x + 1) as f32).min(r.right()) - (x as f32).max(r.x);
                if cx <= 0.0 {
                    continue;
                }
                let cov = (cx * cy * 255.0 + 0.5) as u32;
                let i = row + x as usize;
                self.px[i] = blend_px(self.px[i], c, cov.min(255));
            }
        }
    }

    /// Fill a path given in canvas coordinates.
    pub fn fill_path(&mut self, segs: &[Seg], c: Color) {
        self.fill_path_shaded(segs, &mut |_, _| c);
    }

    pub fn path_bounds(segs: &[Seg]) -> Rect {
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        let mut add = |x: f32, y: f32| {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        };
        for s in segs {
            match *s {
                Seg::Move(x, y) | Seg::Line(x, y) => add(x, y),
                Seg::Quad(cx, cy, x, y) => {
                    add(cx, cy);
                    add(x, y)
                }
                Seg::Close => {}
            }
        }
        if x0 > x1 {
            return Rect::default();
        }
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }

    /// Compute the coverage mask of a path clipped to the current clip.
    /// Returns (origin x, origin y, width, height, mask).
    fn path_mask(&self, segs: &[Seg]) -> Option<(i32, i32, usize, usize, Vec<u8>)> {
        let b = Self::path_bounds(segs);
        let (cx0, cy0, cx1, cy1) = self.clip_bounds();
        let x0 = (b.x.floor() as i32).max(cx0);
        let y0 = (b.y.floor() as i32).max(cy0);
        let x1 = (b.right().ceil() as i32 + 1).min(cx1);
        let y1 = (b.bottom().ceil() as i32 + 1).min(cy1);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        // Rasterize the full path bbox horizontally (needed for correct winding) but only clip rows.
        let bx0 = b.x.floor() as i32 - 1;
        let bw = (b.right().ceil() as i32 + 2 - bx0).max(1) as usize;
        let h = (y1 - y0) as usize;
        if bw > 16384 || h > 16384 {
            return None;
        }
        let mut acc = Accum::new(bw, h);
        acc.path(segs, bx0 as f32, y0 as f32);
        let full = acc.finish();
        let w = (x1 - x0) as usize;
        let mut mask = vec![0u8; w * h];
        let off = (x0 - bx0) as usize;
        for row in 0..h {
            let src = row * bw + off;
            mask[row * w..row * w + w].copy_from_slice(&full[src..src + w]);
        }
        Some((x0, y0, w, h, mask))
    }

    pub fn fill_path_shaded(&mut self, segs: &[Seg], shade: &mut dyn FnMut(f32, f32) -> Color) {
        let Some((x0, y0, w, h, mask)) = self.path_mask(segs) else { return };
        for row in 0..h {
            let y = y0 as usize + row;
            for col in 0..w {
                let cov = mask[row * w + col];
                if cov == 0 {
                    continue;
                }
                let x = x0 as usize + col;
                let c = shade(x as f32 + 0.5, y as f32 + 0.5);
                let i = y * self.w + x;
                self.px[i] = blend_px(self.px[i], c, cov as u32);
            }
        }
    }

    /// Fill a path with a blurred edge (used for box-shadow).
    pub fn fill_path_blurred(&mut self, segs: &[Seg], c: Color, blur: f32) {
        if blur < 0.5 {
            return self.fill_path(segs, c);
        }
        let b = Self::path_bounds(segs);
        let pad = (blur * 1.5).ceil() as i32 + 2;
        let x0 = b.x.floor() as i32 - pad;
        let y0 = b.y.floor() as i32 - pad;
        let w = (b.w.ceil() as i32 + 2 * pad) as usize;
        let h = (b.h.ceil() as i32 + 2 * pad) as usize;
        if w * h > 4_000_000 {
            return self.fill_path(segs, c);
        }
        let mut acc = Accum::new(w, h);
        acc.path(segs, x0 as f32, y0 as f32);
        let mut m: Vec<f32> = acc.finish().into_iter().map(|v| v as f32).collect();
        // three box blur passes approximate a gaussian with sigma = blur/2
        let r = ((blur / 2.0) * 1.0).max(1.0) as usize;
        for _ in 0..3 {
            box_blur_h(&mut m, w, h, r);
            box_blur_v(&mut m, w, h, r);
        }
        let (cx0, cy0, cx1, cy1) = self.clip_bounds();
        for row in 0..h {
            let y = y0 + row as i32;
            if y < cy0 || y >= cy1 {
                continue;
            }
            for col in 0..w {
                let x = x0 + col as i32;
                if x < cx0 || x >= cx1 {
                    continue;
                }
                let cov = m[row * w + col].clamp(0.0, 255.0) as u32;
                if cov == 0 {
                    continue;
                }
                let i = y as usize * self.w + x as usize;
                self.px[i] = blend_px(self.px[i], c, cov);
            }
        }
    }

    /// Blit a glyph coverage mask with its top-left at (x, y).
    pub fn draw_mask(&mut self, x: i32, y: i32, w: usize, h: usize, mask: &[u8], c: Color) {
        let (cx0, cy0, cx1, cy1) = self.clip_bounds();
        for row in 0..h {
            let py = y + row as i32;
            if py < cy0 || py >= cy1 {
                continue;
            }
            let base = py as usize * self.w;
            for col in 0..w {
                let px = x + col as i32;
                if px < cx0 || px >= cx1 {
                    continue;
                }
                let cov = mask[row * w + col];
                if cov == 0 {
                    continue;
                }
                let i = base + px as usize;
                self.px[i] = blend_px(self.px[i], c, cov as u32);
            }
        }
    }

    /// Draw an image scaled into `dst` with bilinear filtering, optionally masked by a path.
    pub fn draw_image(&mut self, img: &Image, dst: Rect, opacity: f32, clip_path: Option<&[Seg]>) {
        if img.w == 0 || img.h == 0 || dst.is_empty() {
            return;
        }
        let sx = img.w as f32 / dst.w;
        let sy = img.h as f32 / dst.h;
        let sample = |x: f32, y: f32| -> Color {
            let fx = ((x - dst.x) * sx - 0.5).clamp(0.0, img.w as f32 - 1.0);
            let fy = ((y - dst.y) * sy - 0.5).clamp(0.0, img.h as f32 - 1.0);
            let ix = fx as u32;
            let iy = fy as u32;
            let tx = fx - ix as f32;
            let ty = fy - iy as f32;
            let ix1 = (ix + 1).min(img.w - 1);
            let iy1 = (iy + 1).min(img.h - 1);
            let p = |x: u32, y: u32| {
                let o = ((y * img.w + x) * 4) as usize;
                [img.data[o] as f32, img.data[o + 1] as f32, img.data[o + 2] as f32, img.data[o + 3] as f32]
            };
            let (a, b, c, d) = (p(ix, iy), p(ix1, iy), p(ix, iy1), p(ix1, iy1));
            let mut out = [0f32; 4];
            for k in 0..4 {
                let top = a[k] + (b[k] - a[k]) * tx;
                let bot = c[k] + (d[k] - c[k]) * tx;
                out[k] = top + (bot - top) * ty;
            }
            Color::rgba(out[0] as u8, out[1] as u8, out[2] as u8, (out[3] * opacity) as u8)
        };
        if let Some(path) = clip_path {
            let mut shade = |x: f32, y: f32| sample(x, y);
            self.fill_path_shaded(path, &mut shade);
            return;
        }
        let r = dst.intersect(&self.clip);
        if r.is_empty() {
            return;
        }
        let x0 = r.x.floor().max(0.0) as usize;
        let y0 = r.y.floor().max(0.0) as usize;
        let x1 = (r.right().ceil() as usize).min(self.w);
        let y1 = (r.bottom().ceil() as usize).min(self.h);
        for y in y0..y1 {
            for x in x0..x1 {
                let c = sample(x as f32 + 0.5, y as f32 + 0.5);
                let i = y * self.w + x;
                self.px[i] = blend_px(self.px[i], c, 255);
            }
        }
    }

    pub fn to_rgb(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.w * self.h * 3);
        for &p in &self.px {
            out.push((p >> 16) as u8);
            out.push((p >> 8) as u8);
            out.push(p as u8);
        }
        out
    }
}

fn box_blur_h(m: &mut [f32], w: usize, h: usize, r: usize) {
    let mut tmp = vec![0f32; w];
    let norm = 1.0 / (2 * r + 1) as f32;
    for y in 0..h {
        let row = &mut m[y * w..y * w + w];
        let mut sum = 0.0;
        for i in 0..=r.min(w - 1) {
            sum += row[i];
        }
        for x in 0..w {
            tmp[x] = sum * norm;
            let add = x + r + 1;
            if add < w {
                sum += row[add];
            }
            if x >= r {
                sum -= row[x - r];
            }
        }
        row.copy_from_slice(&tmp);
    }
}

fn box_blur_v(m: &mut [f32], w: usize, h: usize, r: usize) {
    let mut tmp = vec![0f32; h];
    let norm = 1.0 / (2 * r + 1) as f32;
    for x in 0..w {
        let mut sum = 0.0;
        for i in 0..=r.min(h - 1) {
            sum += m[i * w + x];
        }
        for y in 0..h {
            tmp[y] = sum * norm;
            let add = y + r + 1;
            if add < h {
                sum += m[add * w + x];
            }
            if y >= r {
                sum -= m[(y - r) * w + x];
            }
        }
        for y in 0..h {
            m[y * w + x] = tmp[y];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn square_coverage() {
        let mut a = Accum::new(4, 4);
        a.path(&[Seg::Move(1.0, 1.0), Seg::Line(3.0, 1.0), Seg::Line(3.0, 3.0), Seg::Line(1.0, 3.0), Seg::Close], 0.0, 0.0);
        let m = a.finish();
        assert_eq!(m[5], 255);
        assert_eq!(m[0], 0);
        assert_eq!(m[3], 0);
    }

    #[test]
    fn rect_fill() {
        let mut c = Canvas::new(4, 4);
        c.fill_rect(Rect::new(0.0, 0.0, 2.0, 2.0), Color::BLACK);
        assert_eq!(c.px[0], 0);
        assert_eq!(c.px[3], 0xffffff);
    }
}

#[cfg(test)]
mod ring_tests {
    use super::*;

    #[test]
    fn rounded_ring_interior_is_empty() {
        let mut c = Canvas::new(400, 80);
        let outer = Rect::new(10.0, 10.0, 360.0, 40.0);
        let inner = Rect::new(12.0, 12.0, 356.0, 36.0);
        let mut p = rounded_rect_path(outer, [10.0; 4]);
        p.extend(reverse_path(&rounded_rect_path(inner, [8.0; 4])));
        c.fill_path(&p, Color::BLACK);
        for x in (20..360).step_by(10) {
            assert_eq!(c.px[30 * 400 + x], 0xffffff, "interior pixel at x={x} was painted");
        }
        assert_eq!(c.px[30 * 400 + 10], 0, "left edge should be painted");
    }
}
