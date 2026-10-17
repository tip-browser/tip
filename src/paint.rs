//! Display list construction and rendering.

use std::rc::Rc;

use crate::css::Color;
use crate::dom::NodeId;
use crate::layout::{Inner, LayoutBox, LayoutResult, Replaced};
use crate::raster::{self, Canvas, Image, Rect};
use crate::style::{Background, BgPos, BgSize, BgSizeSpec, BorderStyle, ComputedStyle, Position, Shadow};
use crate::text::{self, FontKey};
use crate::text::ttf::Seg;

#[derive(Clone)]
pub enum Item {
    SetFixed(bool),
    PushClip(Rect),
    PopClip,
    Rect { rect: Rect, color: Color, radii: [f32; 4] },
    Ring { outer: Rect, outer_radii: [f32; 4], inner: Rect, inner_radii: [f32; 4], color: Color },
    Line { x0: f32, y0: f32, x1: f32, y1: f32, width: f32, color: Color, dash: Option<f32> },
    Text { x: f32, baseline: f32, text: String, font: FontKey, size: f32, color: Color, letter_spacing: f32 },
    Image { rect: Rect, image: Rc<Image>, radii: [f32; 4], opacity: f32 },
    Gradient { rect: Rect, bg: Background, radii: [f32; 4], opacity: f32 },
    Shadow { rect: Rect, radii: [f32; 4], color: Color, blur: f32 },
    Path { segs: Vec<Seg>, color: Color },
    /// Start a group composited with `opacity` (CSS opacity); `bounds` limits the work.
    PushLayer { bounds: Rect, opacity: f32 },
    PopLayer,
}

#[derive(Debug, Clone, Copy)]
pub struct Hit {
    pub rect: Rect,
    pub node: NodeId,
    pub fixed: bool,
}

#[derive(Default)]
pub struct DisplayList {
    pub items: Vec<Item>,
    pub hits: Vec<Hit>,
    pub background: Color,
}

impl DisplayList {
    /// Topmost node at a document point (with viewport scroll applied for fixed items).
    pub fn hit_test(&self, x: f32, y: f32, scroll: (f32, f32)) -> Option<NodeId> {
        for h in self.hits.iter().rev() {
            let (px, py) = if h.fixed { (x - scroll.0, y - scroll.1) } else { (x, y) };
            if h.rect.contains(px, py) {
                return Some(h.node);
            }
        }
        None
    }
}

/// State a page provides while painting form controls and images.
pub struct ControlState {
    pub text: String,
    pub placeholder: bool,
    pub checked: bool,
    pub focused: bool,
    pub caret: Option<usize>,
    /// Selected character range (start, end) when focused.
    pub sel: Option<(usize, usize)>,
    /// Range inputs: position of the value within [min, max], 0..=1.
    pub fraction: Option<f32>,
}

pub trait PaintEnv {
    fn node_image(&self, node: NodeId) -> Option<Rc<Image>>;
    fn url_image(&self, url: &str) -> Option<Rc<Image>>;
    fn control(&self, node: NodeId) -> ControlState;
    fn document(&self) -> Option<&crate::dom::Document> {
        None
    }
}

pub fn key_of(s: &ComputedStyle) -> FontKey {
    FontKey::new(s.font_family, s.font_weight, s.italic)
}

struct Builder<'a> {
    list: DisplayList,
    env: &'a dyn PaintEnv,
    clip: Vec<Rect>,
    fixed: bool,
    opacity: f32,
}

fn radii_of(s: &ComputedStyle, r: Rect) -> [f32; 4] {
    use crate::style::Display;
    // border-radius doesn't apply to tables in the collapsing border model
    if s.border_collapse && matches!(s.display, Display::Table | Display::InlineTable | Display::TableRowGroup | Display::TableRow | Display::TableCell) {
        return [0.0; 4];
    }
    let base = r.w.min(r.h);
    [
        s.border_radius[0].or_zero(base),
        s.border_radius[1].or_zero(base),
        s.border_radius[2].or_zero(base),
        s.border_radius[3].or_zero(base),
    ]
}

impl Builder<'_> {
    fn push(&mut self, it: Item) {
        self.list.items.push(it);
    }

    fn current_clip(&self) -> Option<Rect> {
        self.clip.last().copied()
    }

    fn hit(&mut self, rect: Rect, node: Option<NodeId>, style: &ComputedStyle) {
        let Some(node) = node else { return };
        if !style.pointer_events || !style.visible {
            return;
        }
        let r = match self.current_clip() {
            Some(c) => rect.intersect(&c),
            None => rect,
        };
        if !r.is_empty() {
            self.list.hits.push(Hit { rect: r, node, fixed: self.fixed });
        }
    }

    fn push_clip(&mut self, r: Rect) {
        let r = match self.current_clip() {
            Some(c) => r.intersect(&c),
            None => r,
        };
        self.clip.push(r);
        self.push(Item::PushClip(r));
    }

    fn pop_clip(&mut self) {
        self.clip.pop();
        self.push(Item::PopClip);
    }

    fn col(&self, c: Color) -> Color {
        if self.opacity < 1.0 { c.with_alpha_mul(self.opacity) } else { c }
    }

    /// Paint `background-image` layers (bottom-most first). `rect` is the clip (border box),
    /// `origin` the positioning area (padding box).
    fn background_layers(&mut self, s: &ComputedStyle, rect: Rect, origin: Rect, radii: [f32; 4]) {
        let n = s.background_images.len();
        if n == 0 {
            return;
        }
        let rounded = radii.iter().any(|&r| r > 0.0);
        for i in (0..n).rev() {
            let layer = &s.background_images[i];
            let size = if s.background_sizes.is_empty() { BgSizeSpec::Auto } else { s.background_sizes[i % s.background_sizes.len()] };
            let pos = if s.background_positions.is_empty() { BgPos::default() } else { s.background_positions[i % s.background_positions.len()] };
            let (rx, ry) = if s.background_repeats.is_empty() { (true, true) } else { s.background_repeats[i % s.background_repeats.len()] };
            let image = match layer {
                Background::Url(u) => match self.env.url_image(u) {
                    Some(img) => Some(img),
                    None => continue,
                },
                _ => None,
            };
            let intrinsic = image.as_ref().map(|im| (im.w as f32, im.h as f32));
            // tile size
            let (aw, ah) = (origin.w.max(0.0), origin.h.max(0.0));
            let (tw, th) = match (size, intrinsic) {
                (BgSizeSpec::Cover | BgSizeSpec::Contain, Some((iw, ih))) if iw > 0.0 && ih > 0.0 => {
                    let sc = if size == BgSizeSpec::Cover { (aw / iw).max(ah / ih) } else { (aw / iw).min(ah / ih) };
                    (iw * sc, ih * sc)
                }
                (BgSizeSpec::Explicit(w, h), _) => {
                    let w = w.resolve(aw);
                    let h = h.resolve(ah);
                    match (w, h, intrinsic) {
                        (Some(w), Some(h), _) => (w, h),
                        (Some(w), None, Some((iw, ih))) if iw > 0.0 => (w, w * ih / iw),
                        (None, Some(h), Some((iw, ih))) if ih > 0.0 => (h * iw / ih, h),
                        (Some(w), None, None) => (w, ah),
                        (None, Some(h), None) => (aw, h),
                        _ => intrinsic.unwrap_or((aw, ah)),
                    }
                }
                (_, Some(i)) => i,
                _ => (aw, ah),
            };
            if tw <= 0.0 || th <= 0.0 {
                continue;
            }
            let fx = pos.x.resolve(aw - tw).unwrap_or(0.0);
            let fy = pos.y.resolve(ah - th).unwrap_or(0.0);
            let x0 = origin.x + if pos.from_right { aw - tw - fx } else { fx };
            let y0 = origin.y + if pos.from_bottom { ah - th - fy } else { fy };
            // tile positions covering the clip rect
            let xs: Vec<f32> = if rx {
                let start = x0 - ((x0 - rect.x) / tw).ceil().max(0.0) * tw;
                (0..400).map(|k| start + k as f32 * tw).take_while(|x| *x < rect.right()).collect()
            } else {
                vec![x0]
            };
            let ys: Vec<f32> = if ry {
                let start = y0 - ((y0 - rect.y) / th).ceil().max(0.0) * th;
                (0..400).map(|k| start + k as f32 * th).take_while(|y| *y < rect.bottom()).collect()
            } else {
                vec![y0]
            };
            self.push_clip(rect);
            let single = xs.len() == 1 && ys.len() == 1;
            for &ty in ys.iter().take(200) {
                for &tx in xs.iter().take(200) {
                    let tile = Rect::new(tx, ty, tw, th);
                    // rounded corners only matter when one tile covers the whole box
                    let tr = if single && rounded && tile.x <= rect.x + 0.5 && tile.right() >= rect.right() - 0.5 { radii } else { [0.0; 4] };
                    match (&image, layer) {
                        (Some(img), _) => self.push(Item::Image { rect: tile, image: img.clone(), radii: tr, opacity: self.opacity }),
                        (None, g) => self.push(Item::Gradient { rect: tile, bg: g.clone(), radii: tr, opacity: self.opacity }),
                    }
                }
            }
            self.pop_clip();
        }
    }

    fn background(&mut self, b: &LayoutBox, rect: Rect) {
        let origin = b.padding_box();
        let s = &b.style;
        let radii = radii_of(s, rect);
        for sh in s.box_shadow.iter().filter(|sh| !sh.inset) {
            self.shadow(rect, radii, sh);
        }
        if s.background_color.a > 0 {
            let c = self.col(s.background_color);
            self.push(Item::Rect { rect, color: c, radii });
        }
        self.background_layers(s, rect, origin, radii);
        for sh in s.box_shadow.iter().filter(|sh| sh.inset) {
            // approximate inset shadow with a ring
            let inner = Rect::new(rect.x + sh.x + sh.spread, rect.y + sh.y + sh.spread, rect.w - 2.0 * sh.spread, rect.h - 2.0 * sh.spread);
            let c = self.col(sh.color.with_alpha_mul(0.6));
            self.push_clip(rect);
            self.push(Item::Ring { outer: rect.inflate(sh.blur + 50.0), outer_radii: [0.0; 4], inner, inner_radii: radii, color: c });
            self.pop_clip();
        }
    }

    fn shadow(&mut self, rect: Rect, radii: [f32; 4], sh: &Shadow) {
        let r = Rect::new(rect.x + sh.x - sh.spread, rect.y + sh.y - sh.spread, rect.w + 2.0 * sh.spread, rect.h + 2.0 * sh.spread);
        let c = self.col(sh.color);
        self.push(Item::Shadow { rect: r, radii: radii.map(|v| v + sh.spread.max(0.0)), color: c, blur: sh.blur });
    }

    fn borders(&mut self, s: &ComputedStyle, rect: Rect, bw: [f32; 4], first: bool, last: bool) {
        let bw = [bw[0], if last { bw[1] } else { 0.0 }, bw[2], if first { bw[3] } else { 0.0 }];
        if bw.iter().all(|&w| w <= 0.0) {
            return;
        }
        let radii = radii_of(s, rect);
        let colors = [0, 1, 2, 3].map(|i| self.col(s.border_color_at(i)));
        let styles = s.border_style;
        let uniform = colors.iter().all(|c| *c == colors[0]) && styles.iter().all(|st| *st == styles[0] || bw[styles.iter().position(|x| x == st).unwrap()] == 0.0);
        let rounded = radii.iter().any(|&r| r > 0.0);
        if rounded || (uniform && styles[0] == BorderStyle::Solid && bw.iter().all(|&w| w == bw[0])) {
            let inner = Rect::new(rect.x + bw[3], rect.y + bw[0], rect.w - bw[1] - bw[3], rect.h - bw[0] - bw[2]);
            let ir = [
                (radii[0] - bw[3].max(bw[0])).max(0.0),
                (radii[1] - bw[1].max(bw[0])).max(0.0),
                (radii[2] - bw[1].max(bw[2])).max(0.0),
                (radii[3] - bw[3].max(bw[2])).max(0.0),
            ];
            let c = colors.iter().zip(bw.iter()).find(|(_, w)| **w > 0.0).map(|(c, _)| *c).unwrap_or(colors[0]);
            self.push(Item::Ring { outer: rect, outer_radii: radii, inner, inner_radii: ir, color: c });
            return;
        }
        let sides = [
            (Rect::new(rect.x, rect.y, rect.w, bw[0]), (rect.x, rect.y + bw[0] / 2.0, rect.right(), rect.y + bw[0] / 2.0)),
            (Rect::new(rect.right() - bw[1], rect.y, bw[1], rect.h), (rect.right() - bw[1] / 2.0, rect.y, rect.right() - bw[1] / 2.0, rect.bottom())),
            (Rect::new(rect.x, rect.bottom() - bw[2], rect.w, bw[2]), (rect.x, rect.bottom() - bw[2] / 2.0, rect.right(), rect.bottom() - bw[2] / 2.0)),
            (Rect::new(rect.x, rect.y, bw[3], rect.h), (rect.x + bw[3] / 2.0, rect.y, rect.x + bw[3] / 2.0, rect.bottom())),
        ];
        for i in 0..4 {
            if bw[i] <= 0.0 || colors[i].a == 0 {
                continue;
            }
            match styles[i] {
                BorderStyle::Dashed | BorderStyle::Dotted => {
                    let (x0, y0, x1, y1) = sides[i].1;
                    let dash = if styles[i] == BorderStyle::Dashed { bw[i] * 3.0 } else { bw[i] };
                    self.push(Item::Line { x0, y0, x1, y1, width: bw[i], color: colors[i], dash: Some(dash) });
                }
                BorderStyle::Double if bw[i] >= 3.0 => {
                    let t = bw[i] / 3.0;
                    let r = sides[i].0;
                    let (a, b) = if i % 2 == 0 {
                        (Rect::new(r.x, r.y, r.w, t), Rect::new(r.x, r.bottom() - t, r.w, t))
                    } else {
                        (Rect::new(r.x, r.y, t, r.h), Rect::new(r.right() - t, r.y, t, r.h))
                    };
                    self.push(Item::Rect { rect: a, color: colors[i], radii: [0.0; 4] });
                    self.push(Item::Rect { rect: b, color: colors[i], radii: [0.0; 4] });
                }
                _ => self.push(Item::Rect { rect: sides[i].0, color: colors[i], radii: [0.0; 4] }),
            }
        }
    }

    fn outline(&mut self, s: &ComputedStyle, rect: Rect) {
        if s.outline_style == BorderStyle::None || s.outline_width <= 0.0 {
            return;
        }
        let w = s.outline_width;
        let outer = rect.inflate(s.outline_offset + w);
        let inner = rect.inflate(s.outline_offset);
        let c = self.col(s.outline_color.unwrap_or(s.color));
        let r = radii_of(s, rect).map(|v| if v > 0.0 { v + s.outline_offset + w } else { 0.0 });
        self.push(Item::Ring { outer, outer_radii: r, inner, inner_radii: r.map(|v| (v - w).max(0.0)), color: c });
    }

    fn text(&mut self, x: f32, baseline: f32, w: f32, t: &str, s: &ComputedStyle) {
        if !s.visible {
            return;
        }
        let color = self.col(s.color);
        if !t.trim().is_empty() {
            self.push(Item::Text { x, baseline, text: t.to_string(), font: key_of(s), size: s.font_size, color, letter_spacing: s.letter_spacing });
        }
        let dc = self.col(s.decoration_color.unwrap_or(s.color));
        let th = (s.font_size / 14.0).max(1.0);
        if s.underline {
            self.push(Item::Rect { rect: Rect::new(x, baseline + s.font_size * 0.12, w, th), color: dc, radii: [0.0; 4] });
        }
        if s.line_through {
            self.push(Item::Rect { rect: Rect::new(x, baseline - s.font_size * 0.3, w, th), color: dc, radii: [0.0; 4] });
        }
        if s.overline {
            self.push(Item::Rect { rect: Rect::new(x, baseline - s.font_size * 0.9, w, th), color: dc, radii: [0.0; 4] });
        }
    }

    fn replaced(&mut self, b: &LayoutBox, kind: &Replaced) {
        let s = &b.style;
        let c = b.content;
        let node = b.node.unwrap_or(0);
        let m = text::metrics(key_of(s), s.font_size);
        let baseline_mid = c.y + (c.h - (m.ascent + m.descent)) / 2.0 + m.ascent;
        match kind {
            Replaced::Image => match self.env.node_image(node) {
                Some(img) => {
                    let dst = fit_rect(c, img.w as f32, img.h as f32, s.object_fit, false);
                    let radii = radii_of(s, b.border_box());
                    self.push_clip(c);
                    self.push(Item::Image { rect: dst, image: img, radii, opacity: self.opacity });
                    self.pop_clip();
                }
                None => {
                    let st = self.env.control(node);
                    if !st.text.is_empty() {
                        self.push_clip(c);
                        let w = text::measure(key_of(s), s.font_size, &st.text, 0.0);
                        self.text(c.x + 2.0, baseline_mid, w, &st.text, s);
                        self.pop_clip();
                    }
                }
            },
            Replaced::TextInput | Replaced::TextArea => {
                let st = self.env.control(node);
                self.push_clip(b.padding_box());
                let mut ts = (**s).clone();
                if st.placeholder {
                    ts.color = Color::rgb(0x75, 0x75, 0x75);
                }
                let lh = s.line_height_px();
                let lines: Vec<&str> = if *kind == Replaced::TextArea { st.text.split('\n').collect() } else { vec![st.text.as_str()] };
                let mut caret_xy = None;
                let mut consumed = 0usize;
                // horizontal scroll so the caret stays visible in single-line inputs
                let mut shift = 0.0;
                if *kind == Replaced::TextInput && st.focused && !st.placeholder {
                    let upto: String = st.text.chars().take(st.caret.unwrap_or(usize::MAX)).collect();
                    let cw = text::measure(key_of(s), s.font_size, &upto, s.letter_spacing);
                    if cw > c.w - 2.0 {
                        shift = cw - c.w + 2.0;
                    }
                }
                for (i, line) in lines.iter().enumerate() {
                    let base = if *kind == Replaced::TextArea { c.y + i as f32 * lh + (lh - (m.ascent + m.descent)) / 2.0 + m.ascent } else { baseline_mid };
                    let w = text::measure(key_of(s), s.font_size, line, s.letter_spacing);
                    // selection highlight for the part of the range on this line
                    if let (Some((a, b)), false) = (st.sel, st.placeholder) {
                        let n = line.chars().count();
                        let (la, lb) = (a.max(consumed), b.min(consumed + n));
                        if la < lb || (a <= consumed + n && b > consumed + n && lines.len() > 1) {
                            let pre: String = line.chars().take(la.saturating_sub(consumed)).collect();
                            let upto: String = line.chars().take(lb.saturating_sub(consumed)).collect();
                            let x0 = c.x - shift + text::measure(key_of(s), s.font_size, &pre, s.letter_spacing);
                            let mut x1 = c.x - shift + text::measure(key_of(s), s.font_size, &upto, s.letter_spacing);
                            if b > consumed + n {
                                x1 += s.font_size * 0.3;
                            }
                            let hl = if st.focused { Color::rgb(0xb4, 0xd5, 0xfe) } else { Color::rgb(0xdc, 0xdf, 0xe4) };
                            self.push(Item::Rect { rect: Rect::new(x0, base - m.ascent, (x1 - x0).max(1.0), m.ascent + m.descent), color: self.col(hl), radii: [0.0; 4] });
                        }
                    }
                    self.text(c.x - shift, base, w, line, &ts);
                    let n = line.chars().count();
                    if let Some(caret) = st.caret {
                        if !st.placeholder && caret >= consumed && caret <= consumed + n {
                            let pre: String = line.chars().take(caret - consumed).collect();
                            let cx = c.x - shift + text::measure(key_of(s), s.font_size, &pre, s.letter_spacing);
                            caret_xy = Some((cx, base));
                        }
                    }
                    consumed += n + 1;
                }
                if st.focused && st.sel.is_none() {
                    let (cx, base) = caret_xy.unwrap_or((c.x, baseline_mid));
                    self.push(Item::Rect { rect: Rect::new(cx.round(), base - m.ascent, 1.0, m.ascent + m.descent), color: self.col(s.color), radii: [0.0; 4] });
                }
                self.pop_clip();
            }
            Replaced::Checkbox | Replaced::Radio if s.appearance_none => {}
            Replaced::Checkbox | Replaced::Radio => {
                let st = self.env.control(node);
                let r = Rect::new(c.x, c.y, c.w.min(c.h), c.w.min(c.h));
                let round = if *kind == Replaced::Radio { r.w / 2.0 } else { 2.5 };
                let blue = self.col(Color::rgb(0x0b, 0x57, 0xd0));
                if st.checked {
                    self.push(Item::Rect { rect: r, color: blue, radii: [round; 4] });
                    if *kind == Replaced::Radio {
                        let d = r.w * 0.38;
                        self.push(Item::Rect { rect: Rect::new(r.x + (r.w - d) / 2.0, r.y + (r.h - d) / 2.0, d, d), color: self.col(Color::WHITE), radii: [d / 2.0; 4] });
                    } else {
                        let p = |fx: f32, fy: f32| (r.x + r.w * fx, r.y + r.h * fy);
                        let (ax, ay) = p(0.2, 0.52);
                        let (bx, by) = p(0.42, 0.74);
                        let (cx2, cy2) = p(0.8, 0.3);
                        let t = r.w * 0.13;
                        self.push(Item::Line { x0: ax, y0: ay, x1: bx, y1: by, width: t, color: self.col(Color::WHITE), dash: None });
                        self.push(Item::Line { x0: bx, y0: by, x1: cx2, y1: cy2, width: t, color: self.col(Color::WHITE), dash: None });
                    }
                } else {
                    let inner = Rect::new(r.x + 1.0, r.y + 1.0, r.w - 2.0, r.h - 2.0);
                    self.push(Item::Rect { rect: r, color: self.col(Color::WHITE), radii: [round; 4] });
                    self.push(Item::Ring { outer: r, outer_radii: [round; 4], inner, inner_radii: [(round - 1.0).max(0.0); 4], color: self.col(Color::rgb(0x76, 0x76, 0x76)) });
                }
                if st.focused {
                    self.push(Item::Ring { outer: r.inflate(2.0), outer_radii: [round + 2.0; 4], inner: r.inflate(0.5), inner_radii: [round; 4], color: blue });
                }
            }
            Replaced::Button(t) => {
                let w = text::measure(key_of(s), s.font_size, t, 0.0);
                let x = c.x + (c.w - w) / 2.0;
                self.text(x, baseline_mid, w, t, s);
            }
            Replaced::Select => {
                let st = self.env.control(node);
                self.push_clip(b.padding_box());
                let w = text::measure(key_of(s), s.font_size, &st.text, 0.0);
                self.text(c.x, baseline_mid, w, &st.text, s);
                self.pop_clip();
                if s.appearance_none {
                    return;
                }
                // chevron
                let pb = b.padding_box();
                let ax = pb.right() - 12.0;
                let ay = pb.y + pb.h / 2.0;
                let col = self.col(s.color);
                self.push(Item::Line { x0: ax - 3.5, y0: ay - 2.0, x1: ax, y1: ay + 1.5, width: 1.3, color: col, dash: None });
                self.push(Item::Line { x0: ax, y0: ay + 1.5, x1: ax + 3.5, y1: ay - 2.0, width: 1.3, color: col, dash: None });
            }
            Replaced::Range => {
                let st = self.env.control(node);
                let f = st.fraction.unwrap_or(0.5).clamp(0.0, 1.0);
                let accent = self.col(Color::rgb(0x0b, 0x57, 0xd0));
                let thumb = c.h.clamp(10.0, 18.0);
                let track_h = 4.0;
                let x0 = c.x + thumb / 2.0;
                let x1 = c.right() - thumb / 2.0;
                let cy = c.y + c.h / 2.0;
                let tx = x0 + (x1 - x0) * f;
                let track = Rect::new(c.x + 1.0, cy - track_h / 2.0, c.w - 2.0, track_h);
                self.push(Item::Rect { rect: track, color: self.col(Color::rgb(0xd1, 0xd5, 0xdb)), radii: [track_h / 2.0; 4] });
                self.push(Item::Rect { rect: Rect::new(track.x, track.y, (tx - track.x).max(0.0), track_h), color: accent, radii: [track_h / 2.0; 4] });
                let th = Rect::new(tx - thumb / 2.0, cy - thumb / 2.0, thumb, thumb);
                self.push(Item::Shadow { rect: th, radii: [thumb / 2.0; 4], color: Color::rgba(0, 0, 0, 50), blur: 2.0 });
                self.push(Item::Rect { rect: th, color: accent, radii: [thumb / 2.0; 4] });
                let inner = th.inflate(-thumb * 0.28);
                self.push(Item::Rect { rect: inner, color: self.col(Color::WHITE), radii: [inner.w / 2.0; 4] });
                if st.focused {
                    self.push(Item::Ring { outer: th.inflate(3.0), outer_radii: [thumb / 2.0 + 3.0; 4], inner: th.inflate(1.0), inner_radii: [thumb / 2.0 + 1.0; 4], color: accent.with_alpha_mul(0.5) });
                }
            }
            Replaced::Placeholder => {
                let Some(doc) = self.env.document() else { return };
                if doc.tag(node).is_some_and(|t| t.eq_ignore_ascii_case("svg")) {
                    let shapes = crate::svg::render(doc, node, c, s.color, s.font_size, &s.svg);
                    if shapes.is_empty() {
                        return;
                    }
                    self.push_clip(c);
                    for sh in shapes {
                        let color = self.col(sh.color);
                        self.push(Item::Path { segs: sh.segs, color });
                    }
                    self.pop_clip();
                }
            }
        }
    }

    /// Paint a box's own decorations and in-flow content; positioned descendants are deferred
    /// into `layer`.
    fn paint_box(&mut self, b: &LayoutBox, layer: &mut Vec<Deferred>) {
        let s = &b.style;
        let bb = b.border_box();
        if b.inner == Inner::InlineFlow || b.is_text() || b.inner == Inner::Break {
            // inline boxes are painted via their containing block's line boxes; descend for atomics
            for c in &b.children {
                self.child(c, layer);
            }
            return;
        }
        if s.visible {
            let bw = [b.border.top, b.border.right, b.border.bottom, b.border.left];
            if let Some((x0, x1, off)) = b.legend_gap {
                // fieldset: border drawn through the legend's middle, interrupted behind it
                let r = Rect::new(bb.x, bb.y + off, bb.w, (bb.h - off).max(0.0));
                self.background(b, r);
                let (gx0, gx1) = (bb.x + x0, bb.x + x1);
                let pad = 2.0;
                for clip in [
                    Rect::new(bb.x - pad, r.y - pad, gx0 - bb.x + pad, r.h + 2.0 * pad),
                    Rect::new(gx1, r.y - pad, bb.right() - gx1 + pad, r.h + 2.0 * pad),
                    Rect::new(gx0, r.y + bw[0], gx1 - gx0, r.h),
                ] {
                    if clip.w > 0.0 && clip.h > 0.0 {
                        self.push_clip(clip);
                        self.borders(s, r, bw, true, true);
                        self.pop_clip();
                    }
                }
            } else {
                if !matches!(b.inner, Inner::TableRow | Inner::TableRowGroup) || s.background_color.a > 0 {
                    self.background(b, bb);
                }
                self.borders(s, bb, bw, true, true);
            }
        }
        self.hit(bb, if b.is_pseudo { b.node } else { b.node }, s);
        if let Inner::Replaced(k) = &b.inner {
            if s.visible {
                self.replaced(b, k);
            }
        }
        if let Some(m) = &b.marker {
            if s.visible {
                let baseline = b.first_baseline().unwrap_or(b.content.y + s.font_size);
                let key = key_of(s);
                let is_bullet = m.chars().count() == 1;
                let size = if is_bullet { s.font_size * 0.9 } else { s.font_size };
                let w = text::measure(key, size, m, 0.0);
                let x = b.content.x - w - s.font_size * 0.5;
                let mut ms = (**s).clone();
                ms.underline = false;
                ms.line_through = false;
                ms.font_size = size;
                self.text(x, baseline, w, m, &ms);
            }
        }
        let clips = b.is_scroll_container() && b.node.is_some();
        if clips {
            self.push_clip(b.padding_box());
        }
        // block-level, non-floating children
        for c in b.children.iter().filter(|c| !c.is_float() && !c.inline_level) {
            self.child(c, layer);
        }
        for c in b.children.iter().filter(|c| c.is_float()) {
            self.child(c, layer);
        }
        // line boxes: inline decorations then text
        for l in &b.lines {
            for d in &l.decos {
                if d.style.visible {
                    self.background_inline(d);
                }
                self.hit(d.rect, d.node, &d.style);
            }
            for t in &l.texts {
                self.text(t.x, t.baseline, t.w, &t.text, &t.style);
                self.hit(t.rect(), t.node, &t.style);
            }
        }
        for c in b.children.iter().filter(|c| !c.is_float() && c.inline_level) {
            self.child(c, layer);
        }
        if clips {
            self.pop_clip();
            if b.scroll_max.1 > 0.0 && b.style.overflow_y.scrolls() {
                // scrollbar
                let pb = b.padding_box();
                let total = pb.h + b.scroll_max.1;
                let th = (pb.h * pb.h / total).max(20.0);
                let ty = pb.y + (pb.h - th) * (b.scroll.1 / b.scroll_max.1);
                self.push(Item::Rect { rect: Rect::new(pb.right() - 7.0, ty, 5.0, th), color: Color::rgba(0, 0, 0, 90), radii: [2.5; 4] });
            }
        }
        if s.visible {
            self.outline(s, bb);
        }
    }

    fn background_inline(&mut self, d: &crate::layout::InlineDeco) {
        let s = &d.style;
        if s.background_color.a > 0 {
            let c = self.col(s.background_color);
            let radii = radii_of(s, d.rect);
            self.push(Item::Rect { rect: d.rect, color: c, radii });
        }
        if !s.background_images.is_empty() {
            let radii = radii_of(s, d.rect);
            self.background_layers(s, d.rect, d.rect, radii);
        }
        if s.has_border() {
            let bw = [s.effective_border(0), s.effective_border(1), s.effective_border(2), s.effective_border(3)];
            self.borders(s, d.rect, bw, d.first, d.last);
        }
        self.outline(s, d.rect);
    }

    fn child(&mut self, c: &LayoutBox, layer: &mut Vec<Deferred>) {
        if creates_layer(c) {
            layer.push(Deferred { b: c as *const LayoutBox, z: c.style.z_index.unwrap_or(0), clip: if c.fixed { None } else { self.current_clip() }, opacity: self.opacity, order: layer.len() });
        } else {
            self.paint_box(c, layer);
        }
    }

    /// Paint a stacking context rooted at `b`.
    fn paint_context(&mut self, b: &LayoutBox) {
        let saved_op = self.opacity;
        let saved_fixed = self.fixed;
        if b.fixed {
            self.fixed = true;
            self.push(Item::SetFixed(true));
        }
        // opacity applies to the element and its descendants as one group
        let layered = b.style.opacity < 1.0;
        if layered {
            let mut bounds = visual_bounds(b);
            if let Some(c) = self.current_clip() {
                bounds = bounds.intersect(&c);
            }
            self.push(Item::PushLayer { bounds, opacity: b.style.opacity.max(0.0) });
        }
        let mut layer: Vec<Deferred> = Vec::new();
        self.paint_box(b, &mut layer);
        layer.sort_by_key(|d| (d.z, d.order));
        for d in layer {
            // SAFETY: deferred boxes borrow from the layout tree which outlives painting.
            let child = unsafe { &*d.b };
            let pushed = if let Some(c) = d.clip {
                self.clip.push(c);
                self.push(Item::PushClip(c));
                true
            } else if child.fixed && !self.clip.is_empty() {
                false
            } else {
                false
            };
            let saved = self.opacity;
            self.opacity = d.opacity;
            let saved_stack = if child.fixed { Some(std::mem::take(&mut self.clip)) } else { None };
            self.paint_context(child);
            if let Some(s) = saved_stack {
                self.clip = s;
            }
            self.opacity = saved;
            if pushed {
                self.clip.pop();
                self.push(Item::PopClip);
            }
        }
        if layered {
            self.push(Item::PopLayer);
        }
        if b.fixed && !saved_fixed {
            self.push(Item::SetFixed(false));
        }
        self.fixed = saved_fixed;
        self.opacity = saved_op;
    }
}

/// Everything a box may paint: its border box, descendants, lines, shadows and outline.
fn visual_bounds(b: &LayoutBox) -> Rect {
    let s = &b.style;
    let mut r = if matches!(b.inner, Inner::Text(_) | Inner::InlineFlow | Inner::Break) { Rect::default() } else { b.border_box() };
    let mut grow = 0f32;
    for sh in s.box_shadow.iter().filter(|sh| !sh.inset) {
        grow = grow.max(sh.blur * 2.0 + sh.spread.max(0.0) + sh.x.abs().max(sh.y.abs()));
    }
    if s.outline_style != crate::style::BorderStyle::None {
        grow = grow.max(s.outline_width + s.outline_offset.max(0.0));
    }
    if grow > 0.0 && !r.is_empty() {
        r = r.inflate(grow + 1.0);
    }
    let union = |a: Rect, b: Rect| if a.is_empty() { b } else if b.is_empty() { a } else { a.union(&b) };
    for l in &b.lines {
        r = union(r, l.rect.inflate(2.0));
    }
    if !b.is_scroll_container() {
        for c in &b.children {
            if !c.fixed {
                r = union(r, visual_bounds(c));
            }
        }
    }
    r
}

struct Deferred {
    b: *const LayoutBox,
    z: i32,
    clip: Option<Rect>,
    opacity: f32,
    order: usize,
}

fn creates_layer(b: &LayoutBox) -> bool {
    if b.is_text() || b.inner == Inner::Break || b.inner == Inner::InlineFlow && b.style.position == Position::Static {
        return false;
    }
    b.is_positioned() || b.style.opacity < 1.0
}

/// Compute a destination rect for an image of size (iw, ih) within `r`.
fn fit_rect(r: Rect, iw: f32, ih: f32, fit: BgSize, _repeat: bool) -> Rect {
    if iw <= 0.0 || ih <= 0.0 {
        return r;
    }
    match fit {
        BgSize::Auto => r,
        BgSize::Cover | BgSize::Contain => {
            let sx = r.w / iw;
            let sy = r.h / ih;
            let s = if fit == BgSize::Cover { sx.max(sy) } else { sx.min(sy) };
            let w = iw * s;
            let h = ih * s;
            Rect::new(r.x + (r.w - w) / 2.0, r.y + (r.h - h) / 2.0, w, h)
        }
    }
}

pub fn build_display_list(layout: &LayoutResult, env: &dyn PaintEnv) -> DisplayList {
    let mut b = Builder { list: DisplayList::default(), env, clip: vec![], fixed: false, opacity: 1.0 };
    b.list.background = layout.canvas_background;
    if let Some(root) = &layout.root {
        b.paint_context(root);
    }
    b.list
}

// ---------------------------------------------------------------- rendering

pub struct Viewport {
    /// Device-pixel origin of the page area on the canvas.
    pub ox: f32,
    pub oy: f32,
    /// Page area size in device pixels.
    pub w: f32,
    pub h: f32,
    pub scale: f32,
    pub scroll: (f32, f32),
}

fn sample_gradient(stops: &[(Color, Option<f32>)], t: f32) -> Color {
    let n = stops.len();
    if n == 1 {
        return stops[0].0;
    }
    let pos: Vec<f32> = stops.iter().enumerate().map(|(i, (_, p))| p.unwrap_or(i as f32 / (n - 1) as f32)).collect();
    if t <= pos[0] {
        return stops[0].0;
    }
    for i in 1..n {
        if t <= pos[i] {
            let span = (pos[i] - pos[i - 1]).max(1e-6);
            let f = (t - pos[i - 1]) / span;
            let (a, b) = (stops[i - 1].0, stops[i].0);
            let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * f).round() as u8;
            return Color::rgba(l(a.r, b.r), l(a.g, b.g), l(a.b, b.b), l(a.a, b.a));
        }
    }
    stops[n - 1].0
}

pub fn render(list: &DisplayList, canvas: &mut Canvas, vp: &Viewport) {
    let area = Rect::new(vp.ox, vp.oy, vp.w, vp.h);
    canvas.push_clip(area);
    canvas.fill_rect(area, list.background);
    let mut fixed = false;
    let s = vp.scale;
    let mut clip_depth = 0;
    let tf = |r: Rect, fixed: bool| -> Rect {
        let (sx, sy) = if fixed { (0.0, 0.0) } else { vp.scroll };
        Rect::new(vp.ox + (r.x - sx) * s, vp.oy + (r.y - sy) * s, r.w * s, r.h * s)
    };
    let pt = |x: f32, y: f32, fixed: bool| -> (f32, f32) {
        let (sx, sy) = if fixed { (0.0, 0.0) } else { vp.scroll };
        (vp.ox + (x - sx) * s, vp.oy + (y - sy) * s)
    };
    // cull items far outside the viewport
    let visible = Rect::new(vp.ox - 64.0, vp.oy - 64.0, vp.w + 128.0, vp.h + 128.0);
    let mut layers: Vec<(Vec<u32>, (usize, usize, usize, usize), f32)> = Vec::new();
    for it in &list.items {
        match it {
            Item::PushLayer { bounds, opacity } => {
                let r = tf(*bounds, fixed).inflate(1.0);
                let region = canvas.region(r);
                layers.push((canvas.save_region(region), region, *opacity));
            }
            Item::PopLayer => {
                if let Some((saved, region, op)) = layers.pop() {
                    canvas.blend_region(&saved, region, op);
                }
            }
            Item::SetFixed(f) => fixed = *f,
            Item::PushClip(r) => {
                canvas.push_clip(tf(*r, fixed));
                clip_depth += 1;
            }
            Item::PopClip => {
                if clip_depth > 0 {
                    canvas.pop_clip();
                    clip_depth -= 1;
                }
            }
            Item::Rect { rect, color, radii } => {
                let r = tf(*rect, fixed);
                if r.intersect(&visible).is_empty() {
                    continue;
                }
                if radii.iter().all(|&v| v <= 0.0) {
                    canvas.fill_rect(snap(r), *color);
                } else {
                    canvas.fill_path(&raster::rounded_rect_path(r, radii.map(|v| v * s)), *color);
                }
            }
            Item::Ring { outer, outer_radii, inner, inner_radii, color } => {
                let o = tf(*outer, fixed);
                if o.intersect(&visible).is_empty() {
                    continue;
                }
                let i = tf(*inner, fixed);
                let mut p = raster::rounded_rect_path(snap(o), outer_radii.map(|v| v * s));
                if i.w > 0.0 && i.h > 0.0 {
                    p.extend(raster::reverse_path(&raster::rounded_rect_path(snap(i), inner_radii.map(|v| v * s))));
                }
                canvas.fill_path(&p, *color);
            }
            Item::Line { x0, y0, x1, y1, width, color, dash } => {
                let (ax, ay) = pt(*x0, *y0, fixed);
                let (bx, by) = pt(*x1, *y1, fixed);
                let w = (width * s).max(1.0);
                match dash {
                    None => canvas.fill_path(&line_path(ax, ay, bx, by, w), *color),
                    Some(d) => {
                        let d = d * s;
                        let len = ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt();
                        if len <= 0.0 {
                            continue;
                        }
                        let (ux, uy) = ((bx - ax) / len, (by - ay) / len);
                        let mut t = 0.0;
                        while t < len {
                            let e = (t + d).min(len);
                            canvas.fill_path(&line_path(ax + ux * t, ay + uy * t, ax + ux * e, ay + uy * e, w), *color);
                            t += d * 2.0;
                        }
                    }
                }
            }
            Item::Text { x, baseline, text, font, size, color, letter_spacing } => {
                let (px, py) = pt(*x, *baseline, fixed);
                if py < visible.y - size * s * 2.0 || py > visible.bottom() + size * s {
                    continue;
                }
                draw_text(canvas, px, py, text, *font, *size, s, *color, *letter_spacing);
            }
            Item::Image { rect, image, radii, opacity } => {
                let r = tf(*rect, fixed);
                if r.intersect(&visible).is_empty() {
                    continue;
                }
                if radii.iter().any(|&v| v > 0.0) {
                    let p = raster::rounded_rect_path(r, radii.map(|v| v * s));
                    canvas.draw_image(image, r, *opacity, Some(&p));
                } else {
                    canvas.draw_image(image, r, *opacity, None);
                }
            }
            Item::Gradient { rect, bg, radii, opacity } => {
                let r = tf(*rect, fixed);
                if r.intersect(&visible).is_empty() {
                    continue;
                }
                let p = raster::rounded_rect_path(r, radii.map(|v| v * s));
                match bg {
                    Background::LinearGradient { angle, stops } => {
                        let a = angle.to_radians();
                        let (dx, dy) = (a.sin(), -a.cos());
                        let half = (r.w * dx.abs() + r.h * dy.abs()) / 2.0;
                        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
                        let op = *opacity;
                        canvas.fill_path_shaded(&p, &mut |x, y| {
                            let t = (((x - cx) * dx + (y - cy) * dy) / half.max(1e-3) + 1.0) / 2.0;
                            sample_gradient(stops, t.clamp(0.0, 1.0)).with_alpha_mul(op)
                        });
                    }
                    Background::RadialGradient { stops } => {
                        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
                        let (rx, ry) = (r.w / 2.0 * 1.4142, r.h / 2.0 * 1.4142);
                        let op = *opacity;
                        canvas.fill_path_shaded(&p, &mut |x, y| {
                            let t = (((x - cx) / rx).powi(2) + ((y - cy) / ry).powi(2)).sqrt();
                            sample_gradient(stops, t.clamp(0.0, 1.0)).with_alpha_mul(op)
                        });
                    }
                    Background::Url(_) => {}
                }
            }
            Item::Shadow { rect, radii, color, blur } => {
                let r = tf(*rect, fixed);
                if r.inflate(blur * s * 2.0).intersect(&visible).is_empty() {
                    continue;
                }
                let p = raster::rounded_rect_path(r, radii.map(|v| v * s));
                canvas.fill_path_blurred(&p, *color, blur * s);
            }
            Item::Path { segs, color } => {
                let mapped: Vec<Seg> = segs
                    .iter()
                    .map(|sg| match *sg {
                        Seg::Move(x, y) => {
                            let (a, b) = pt(x, y, fixed);
                            Seg::Move(a, b)
                        }
                        Seg::Line(x, y) => {
                            let (a, b) = pt(x, y, fixed);
                            Seg::Line(a, b)
                        }
                        Seg::Quad(cx, cy, x, y) => {
                            let (a, b) = pt(cx, cy, fixed);
                            let (c, d) = pt(x, y, fixed);
                            Seg::Quad(a, b, c, d)
                        }
                        Seg::Close => Seg::Close,
                    })
                    .collect();
                canvas.fill_path(&mapped, *color);
            }
        }
    }
    while clip_depth > 0 {
        canvas.pop_clip();
        clip_depth -= 1;
    }
    canvas.pop_clip();
}

/// Snap rect edges to whole device pixels for crisp boxes.
fn snap(r: Rect) -> Rect {
    let x0 = r.x.round();
    let y0 = r.y.round();
    let x1 = r.right().round();
    let y1 = r.bottom().round();
    Rect::new(x0, y0, (x1 - x0).max(if r.w > 0.0 { 1.0 } else { 0.0 }), (y1 - y0).max(if r.h > 0.0 { 1.0 } else { 0.0 }))
}

fn line_path(x0: f32, y0: f32, x1: f32, y1: f32, w: f32) -> Vec<Seg> {
    let len = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt().max(1e-3);
    let (nx, ny) = (-(y1 - y0) / len * w / 2.0, (x1 - x0) / len * w / 2.0);
    vec![Seg::Move(x0 + nx, y0 + ny), Seg::Line(x1 + nx, y1 + ny), Seg::Line(x1 - nx, y1 - ny), Seg::Line(x0 - nx, y0 - ny), Seg::Close]
}

#[allow(clippy::too_many_arguments)]
pub fn draw_text(canvas: &mut Canvas, px: f32, py: f32, t: &str, font: FontKey, size: f32, scale: f32, color: Color, letter_spacing: f32) {
    text::with_fonts(|fs| {
        let dsize = size * scale;
        let positions = fs.positions(font, size, t, letter_spacing);
        let by = py.round() as i32;
        for (ch, off) in positions {
            if ch.is_whitespace() {
                continue;
            }
            if let Some(g) = fs.glyph(font, dsize, ch) {
                if g.w == 0 {
                    continue;
                }
                let gx = (px + off * scale).round() as i32 + g.left;
                canvas.draw_mask(gx, by + g.top, g.w, g.h, &g.mask, color);
            }
        }
    });
}
