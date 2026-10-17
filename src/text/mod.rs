//! Font discovery, glyph caching and text measurement.

pub mod ttf;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::raster::Accum;
use crate::style::FontFamily;
use ttf::{Font, Seg};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FontKey {
    pub family: FontFamily,
    pub bold: bool,
    pub italic: bool,
}

impl FontKey {
    pub fn new(family: FontFamily, weight: u16, italic: bool) -> FontKey {
        FontKey { family, bold: weight >= 600, italic }
    }
}

pub struct GlyphBitmap {
    pub w: usize,
    pub h: usize,
    /// offset from pen position to top-left of bitmap, y down
    pub left: i32,
    pub top: i32,
    pub mask: Vec<u8>,
}

#[derive(Clone, Copy)]
struct Resolved {
    face: usize,
    synth_bold: bool,
    synth_italic: bool,
}

pub struct FontSystem {
    faces: Vec<Font>,
    by_key: HashMap<FontKey, Option<Resolved>>,
    fallbacks: Vec<usize>,
    fallbacks_loaded: bool,
    cmap: HashMap<(usize, char), u16>,
    glyphs: HashMap<(usize, u16, u32, bool, bool), Arc<GlyphBitmap>>,
}

#[derive(Debug, Clone, Copy)]
pub struct LineMetrics {
    pub ascent: f32,
    pub descent: f32,
}

fn candidates(key: FontKey) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    let suffix_mac = match (key.bold, key.italic) {
        (false, false) => "",
        (true, false) => " Bold",
        (false, true) => " Italic",
        (true, true) => " Bold Italic",
    };
    if let Ok(dir) = std::env::var("TIP_FONT_DIR") {
        let base = match key.family {
            FontFamily::Sans => "sans",
            FontFamily::Serif => "serif",
            FontFamily::Mono => "mono",
        };
        let style = match (key.bold, key.italic) {
            (false, false) => "regular",
            (true, false) => "bold",
            (false, true) => "italic",
            (true, true) => "bolditalic",
        };
        out.push((format!("{dir}/{base}-{style}.ttf"), 0));
        out.push((format!("{dir}/{base}-regular.ttf"), 0));
    }
    // macOS
    let mac_names: &[&str] = match key.family {
        FontFamily::Sans => &["Arial", "Verdana"],
        FontFamily::Serif => &["Times New Roman", "Georgia"],
        FontFamily::Mono => &["Courier New"],
    };
    for n in mac_names {
        out.push((format!("/System/Library/Fonts/Supplemental/{n}{suffix_mac}.ttf"), 0));
        out.push((format!("/Library/Fonts/{n}{suffix_mac}.ttf"), 0));
    }
    // Windows
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    let win: &[&str] = match (key.family, key.bold, key.italic) {
        (FontFamily::Sans, false, false) => &["segoeui", "arial"],
        (FontFamily::Sans, true, false) => &["segoeuib", "arialbd"],
        (FontFamily::Sans, false, true) => &["segoeuii", "ariali"],
        (FontFamily::Sans, true, true) => &["segoeuiz", "arialbi"],
        (FontFamily::Serif, false, false) => &["times"],
        (FontFamily::Serif, true, false) => &["timesbd"],
        (FontFamily::Serif, false, true) => &["timesi"],
        (FontFamily::Serif, true, true) => &["timesbi"],
        (FontFamily::Mono, false, false) => &["consola", "cour"],
        (FontFamily::Mono, true, false) => &["consolab", "courbd"],
        (FontFamily::Mono, false, true) => &["consolai", "couri"],
        (FontFamily::Mono, true, true) => &["consolaz", "courbi"],
    };
    for n in win {
        out.push((format!("{windir}\\Fonts\\{n}.ttf"), 0));
    }
    // Linux
    let lin_style = match (key.bold, key.italic) {
        (false, false) => ("", "Regular"),
        (true, false) => ("-Bold", "Bold"),
        (false, true) => ("-Oblique", "Italic"),
        (true, true) => ("-BoldOblique", "BoldItalic"),
    };
    let (dj, lib) = match key.family {
        FontFamily::Sans => ("DejaVuSans", "LiberationSans"),
        FontFamily::Serif => ("DejaVuSerif", "LiberationSerif"),
        FontFamily::Mono => ("DejaVuSansMono", "LiberationMono"),
    };
    let dj_style = if key.family == FontFamily::Serif { lin_style.0.replace("Oblique", "Italic") } else { lin_style.0.to_string() };
    out.push((format!("/usr/share/fonts/truetype/liberation/{lib}-{}.ttf", lin_style.1), 0));
    out.push((format!("/usr/share/fonts/truetype/liberation2/{lib}-{}.ttf", lin_style.1), 0));
    out.push((format!("/usr/share/fonts/liberation-sans/{lib}-{}.ttf", lin_style.1), 0));
    out.push((format!("/usr/share/fonts/truetype/dejavu/{dj}{dj_style}.ttf"), 0));
    out.push((format!("/usr/share/fonts/TTF/{dj}{dj_style}.ttf"), 0));
    out.push((format!("/usr/share/fonts/dejavu/{dj}{dj_style}.ttf"), 0));
    out
}

fn fallback_candidates() -> Vec<(String, u32)> {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    vec![
        ("/System/Library/Fonts/Supplemental/Arial Unicode.ttf".into(), 0),
        ("/Library/Fonts/Arial Unicode.ttf".into(), 0),
        ("/System/Library/Fonts/Apple Symbols.ttf".into(), 0),
        (format!("{windir}\\Fonts\\seguisym.ttf"), 0),
        (format!("{windir}\\Fonts\\arialuni.ttf"), 0),
        (format!("{windir}\\Fonts\\msyh.ttc"), 0),
        ("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf".into(), 0),
        ("/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf".into(), 0),
    ]
}

fn load(path: &str, index: u32) -> Option<Font> {
    let data = std::fs::read(path).ok()?;
    Font::parse(data, index).ok()
}

static FONTS: OnceLock<Mutex<FontSystem>> = OnceLock::new();

/// Run `f` with exclusive access to the process-wide font system.
pub fn with_fonts<R>(f: impl FnOnce(&mut FontSystem) -> R) -> R {
    let m = FONTS.get_or_init(|| Mutex::new(FontSystem::new()));
    let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut g)
}

impl FontSystem {
    fn new() -> FontSystem {
        FontSystem { faces: vec![], by_key: HashMap::new(), fallbacks: vec![], fallbacks_loaded: false, cmap: HashMap::new(), glyphs: HashMap::new() }
    }

    /// Fallback fonts are large (Arial Unicode is ~23MB) so they load on first use.
    fn load_fallbacks(&mut self) {
        if self.fallbacks_loaded {
            return;
        }
        self.fallbacks_loaded = true;
        for (p, i) in fallback_candidates() {
            if let Some(f) = load(&p, i) {
                self.faces.push(f);
                self.fallbacks.push(self.faces.len() - 1);
            }
        }
    }

    fn resolve(&mut self, key: FontKey) -> Option<Resolved> {
        if let Some(r) = self.by_key.get(&key) {
            return *r;
        }
        let mut found = None;
        for (p, i) in candidates(key) {
            if let Some(f) = load(&p, i) {
                self.faces.push(f);
                found = Some(Resolved { face: self.faces.len() - 1, synth_bold: false, synth_italic: false });
                break;
            }
        }
        if found.is_none() {
            let regular = FontKey { bold: false, italic: false, ..key };
            let base = if regular != key { self.resolve(regular) } else { None }
                .or_else(|| {
                    if key.family != FontFamily::Sans {
                        self.resolve(FontKey { family: FontFamily::Sans, bold: false, italic: false })
                    } else {
                        None
                    }
                })
                .or_else(|| {
                    self.load_fallbacks();
                    self.fallbacks.first().map(|&f| Resolved { face: f, synth_bold: false, synth_italic: false })
                });
            found = base.map(|b| Resolved { face: b.face, synth_bold: key.bold, synth_italic: key.italic });
        }
        self.by_key.insert(key, found);
        found
    }

    pub fn has_fonts(&mut self) -> bool {
        self.resolve(FontKey { family: FontFamily::Sans, bold: false, italic: false }).is_some()
    }

    /// Find the face and glyph id to use for `c`, consulting fallbacks.
    fn gid(&mut self, face: usize, c: char) -> u16 {
        if let Some(&g) = self.cmap.get(&(face, c)) {
            return g;
        }
        let g = self.faces[face].glyph_index(c);
        self.cmap.insert((face, c), g);
        g
    }

    fn lookup(&mut self, key: FontKey, c: char) -> Option<(Resolved, u16)> {
        let r = self.resolve(key)?;
        let g = self.gid(r.face, c);
        if g != 0 || c.is_whitespace() || (c as u32) < 0x20 {
            return Some((r, g));
        }
        self.load_fallbacks();
        for i in 0..self.fallbacks.len() {
            let fb = self.fallbacks[i];
            let g = self.gid(fb, c);
            if g != 0 {
                return Some((Resolved { face: fb, ..r }, g));
            }
        }
        Some((r, 0))
    }

    pub fn metrics(&mut self, key: FontKey, size: f32) -> LineMetrics {
        match self.resolve(key) {
            Some(r) => {
                let f = &self.faces[r.face];
                let s = size / f.units_per_em;
                LineMetrics { ascent: f.ascender * s, descent: -f.descender * s }
            }
            None => LineMetrics { ascent: size * 0.8, descent: size * 0.2 },
        }
    }

    pub fn advance(&mut self, key: FontKey, size: f32, c: char) -> f32 {
        if c == '\u{a0}' {
            return self.advance(key, size, ' ');
        }
        if c == '\t' {
            return self.advance(key, size, ' ') * 8.0;
        }
        match self.lookup(key, c) {
            Some((r, g)) => {
                let f = &self.faces[r.face];
                let mut a = f.advance(g) * size / f.units_per_em;
                if r.synth_bold {
                    a += (size / 24.0).max(0.5);
                }
                a
            }
            None => size * 0.5,
        }
    }

    pub fn measure(&mut self, key: FontKey, size: f32, text: &str, letter_spacing: f32) -> f32 {
        let mut w = 0.0;
        let mut prev: Option<(usize, u16)> = None;
        for c in text.chars() {
            let adv = self.advance(key, size, c);
            if let Some((r, g)) = self.lookup(key, c) {
                if let Some((pf, pg)) = prev {
                    if pf == r.face {
                        let f = &self.faces[r.face];
                        w += f.kerning(pg, g) * size / f.units_per_em;
                    }
                }
                prev = Some((r.face, g));
            }
            w += adv + letter_spacing;
        }
        w
    }

    /// Per-character pen offsets for `text`, matching `measure`.
    pub fn positions(&mut self, key: FontKey, size: f32, text: &str, letter_spacing: f32) -> Vec<(char, f32)> {
        let mut out = Vec::with_capacity(text.len());
        let mut x = 0.0;
        let mut prev: Option<(usize, u16)> = None;
        for c in text.chars() {
            if let Some((r, g)) = self.lookup(key, c) {
                if let Some((pf, pg)) = prev {
                    if pf == r.face {
                        let f = &self.faces[r.face];
                        x += f.kerning(pg, g) * size / f.units_per_em;
                    }
                }
                prev = Some((r.face, g));
            }
            out.push((c, x));
            x += self.advance(key, size, c) + letter_spacing;
        }
        out
    }

    /// Rasterize a glyph at a device pixel size.
    pub fn glyph(&mut self, key: FontKey, size: f32, c: char) -> Option<Arc<GlyphBitmap>> {
        if c.is_whitespace() {
            return None;
        }
        let (r, g) = self.lookup(key, c)?;
        let size_q = (size * 4.0).round() as u32;
        let ck = (r.face, g, size_q, r.synth_bold, r.synth_italic);
        if let Some(b) = self.glyphs.get(&ck) {
            return Some(b.clone());
        }
        let f = &self.faces[r.face];
        let scale = size_q as f32 / 4.0 / f.units_per_em;
        let skew = if r.synth_italic { 0.2 } else { 0.0 };
        // transform to pixel space, y down
        let tf = |x: f32, y: f32| (x * scale + y * scale * skew, -y * scale);
        let segs: Vec<Seg> = f
            .outline(g)
            .into_iter()
            .map(|s| match s {
                Seg::Move(x, y) => {
                    let (a, b) = tf(x, y);
                    Seg::Move(a, b)
                }
                Seg::Line(x, y) => {
                    let (a, b) = tf(x, y);
                    Seg::Line(a, b)
                }
                Seg::Quad(cx, cy, x, y) => {
                    let (a, b) = tf(cx, cy);
                    let (c, d) = tf(x, y);
                    Seg::Quad(a, b, c, d)
                }
                Seg::Close => Seg::Close,
            })
            .collect();
        let bmp = if segs.is_empty() {
            GlyphBitmap { w: 0, h: 0, left: 0, top: 0, mask: vec![] }
        } else {
            let b = crate::raster::Canvas::path_bounds(&segs);
            let left = b.x.floor() as i32;
            let top = b.y.floor() as i32;
            let w = (b.right().ceil() as i32 - left + 1).max(1) as usize;
            let h = (b.bottom().ceil() as i32 - top + 1).max(1) as usize;
            let mut acc = Accum::new(w, h);
            acc.path(&segs, left as f32, top as f32);
            let mut mask = acc.finish();
            let mut w = w;
            if r.synth_bold {
                let extra = ((size / 24.0).round() as usize).max(1);
                let nw = w + extra;
                let mut m2 = vec![0u8; nw * h];
                for y in 0..h {
                    for x in 0..nw {
                        let mut v = 0u8;
                        for k in 0..=extra {
                            if x >= k && x - k < w {
                                v = v.max(mask[y * w + x - k]);
                            }
                        }
                        m2[y * nw + x] = v;
                    }
                }
                mask = m2;
                w = nw;
            }
            GlyphBitmap { w, h, left, top, mask }
        };
        let rc = Arc::new(bmp);
        self.glyphs.insert(ck, rc.clone());
        if self.glyphs.len() > 20000 {
            self.glyphs.clear();
        }
        Some(rc)
    }
}

/// Convenience wrappers.
pub fn measure(key: FontKey, size: f32, text: &str, letter_spacing: f32) -> f32 {
    with_fonts(|f| f.measure(key, size, text, letter_spacing))
}

pub fn metrics(key: FontKey, size: f32) -> LineMetrics {
    with_fonts(|f| f.metrics(key, size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_system_font() {
        with_fonts(|fs| {
            if !fs.has_fonts() {
                eprintln!("no system fonts; skipping");
                return;
            }
            let key = FontKey::new(FontFamily::Sans, 400, false);
            let w = fs.measure(key, 16.0, "Hello", 0.0);
            assert!(w > 20.0 && w < 60.0, "width {w}");
            let g = fs.glyph(key, 32.0, 'A').unwrap();
            assert!(g.w > 10 && g.h > 15);
            assert!(g.mask.iter().any(|&v| v == 255));
        });
    }
}
