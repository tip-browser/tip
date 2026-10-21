//! CSS color functions (CSS Color 4/5): rgb(), hsl(), hwb(), lab(), lch(), oklab(), oklch(),
//! color(), color-mix(), light-dark() and relative colors (`rgb(from <color> r g b / alpha)`).

use crate::css::{named_color, parse_hex_color, Color, Tok};

/// A color as gamma-encoded sRGB channels and alpha (0..1; channels may be out of gamut).
type Rgba = [f32; 4];

pub fn parse(t: &[&Tok], current: Option<Color>) -> Option<Color> {
    let mut p = 0;
    let c = color(t, &mut p, current)?;
    let ch = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Some(Color::rgba(ch(c[0]), ch(c[1]), ch(c[2]), ch(c[3])))
}

fn from_u8(c: Color) -> Rgba {
    [c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0, c.a as f32 / 255.0]
}

/// A function's argument tokens, from `p` (just after the `Function` token) to the matching
/// `RParen`; `p` moves past it.
fn args<'b, 'a>(t: &'b [&'a Tok], p: &mut usize) -> &'b [&'a Tok] {
    let start = *p;
    let mut depth = 1;
    while *p < t.len() {
        match t[*p] {
            Tok::Function(_) | Tok::LParen => depth += 1,
            Tok::RParen => {
                depth -= 1;
                if depth == 0 {
                    *p += 1;
                    return &t[start..*p - 1];
                }
            }
            _ => {}
        }
        *p += 1;
    }
    &t[start..]
}

fn color(t: &[&Tok], p: &mut usize, cur: Option<Color>) -> Option<Rgba> {
    let tok = *t.get(*p)?;
    *p += 1;
    match tok {
        Tok::Hash(h) => parse_hex_color(h).map(from_u8),
        Tok::Ident(n) => {
            let n = n.to_ascii_lowercase();
            if n == "currentcolor" {
                return cur.map(from_u8);
            }
            named_color(&n).map(from_u8)
        }
        Tok::Function(f) => {
            let f = f.to_ascii_lowercase();
            let a = args(t, p);
            match f.as_str() {
                "rgb" | "rgba" => channels(Space::Rgb255, a, cur),
                "hsl" | "hsla" => channels(Space::Hsl, a, cur),
                "hwb" => channels(Space::Hwb, a, cur),
                "lab" => channels(Space::Lab, a, cur),
                "lch" => channels(Space::Lch, a, cur),
                "oklab" => channels(Space::Oklab, a, cur),
                "oklch" => channels(Space::Oklch, a, cur),
                "color" => color_fn(a, cur),
                "color-mix" => mix(a, cur),
                "light-dark" => color(a, &mut 0, cur),
                _ => None,
            }
        }
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Space {
    /// rgb(): channels 0..255
    Rgb255,
    /// color(srgb): channels 0..1
    Srgb,
    SrgbLinear,
    Hsl,
    Hwb,
    Lab,
    Lch,
    Oklab,
    Oklch,
}

impl Space {
    fn named(s: &str) -> Option<Space> {
        Some(match s {
            "srgb" | "display-p3" | "a98-rgb" | "prophoto-rgb" | "rec2020" => Space::Srgb,
            // XYZ is linear in linear-light sRGB, so interpolating in either gives the same result
            "srgb-linear" | "xyz" | "xyz-d65" | "xyz-d50" => Space::SrgbLinear,
            "hsl" => Space::Hsl,
            "hwb" => Space::Hwb,
            "lab" => Space::Lab,
            "lch" => Space::Lch,
            "oklab" => Space::Oklab,
            "oklch" => Space::Oklch,
            _ => return None,
        })
    }

    fn channel_names(self) -> [&'static str; 3] {
        match self {
            Space::Rgb255 | Space::Srgb | Space::SrgbLinear => ["r", "g", "b"],
            Space::Hsl => ["h", "s", "l"],
            Space::Hwb => ["h", "w", "b"],
            Space::Lab | Space::Oklab => ["l", "a", "b"],
            Space::Lch | Space::Oklch => ["l", "c", "h"],
        }
    }

    fn hue_index(self) -> Option<usize> {
        match self {
            Space::Hsl | Space::Hwb => Some(0),
            Space::Lch | Space::Oklch => Some(2),
            _ => None,
        }
    }

    /// What 100% means for channel `i`.
    fn percent_ref(self, i: usize) -> f32 {
        match (self, i) {
            (Space::Rgb255, _) => 255.0,
            (Space::Hsl | Space::Hwb, _) => 100.0,
            (Space::Lab | Space::Lch, 0) => 100.0,
            (Space::Lab, _) => 125.0,
            (Space::Lch, _) => 150.0,
            (Space::Oklab, 0) | (Space::Oklch, 0) => 1.0,
            (Space::Oklab | Space::Oklch, _) => 0.4,
            _ => 1.0,
        }
    }

    fn from_srgb(self, c: [f32; 3]) -> [f32; 3] {
        match self {
            Space::Rgb255 => c.map(|v| v * 255.0),
            Space::Srgb => c,
            Space::SrgbLinear => c.map(linear),
            Space::Hsl => srgb_to_hsl(c),
            Space::Hwb => {
                let max = c[0].max(c[1]).max(c[2]);
                let min = c[0].min(c[1]).min(c[2]);
                [srgb_to_hsl(c)[0], min * 100.0, (1.0 - max) * 100.0]
            }
            Space::Lab => srgb_to_lab(c),
            Space::Lch => polar(srgb_to_lab(c)),
            Space::Oklab => srgb_to_oklab(c),
            Space::Oklch => polar(srgb_to_oklab(c)),
        }
    }

    fn to_srgb(self, c: [f32; 3]) -> [f32; 3] {
        match self {
            Space::Rgb255 => c.map(|v| v / 255.0),
            Space::Srgb => c,
            Space::SrgbLinear => c.map(gamma),
            Space::Hsl => hsl_to_srgb(c),
            Space::Hwb => {
                let (w, b) = (c[1] / 100.0, c[2] / 100.0);
                if w + b >= 1.0 {
                    let g = w / (w + b);
                    return [g, g, g];
                }
                hsl_to_srgb([c[0], 100.0, 50.0]).map(|v| v * (1.0 - w - b) + w)
            }
            Space::Lab => lab_to_srgb(c),
            Space::Lch => lab_to_srgb(cartesian(c)),
            Space::Oklab => oklab_to_srgb(c),
            Space::Oklch => oklab_to_srgb(cartesian(c)),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Unit {
    Num,
    Pct,
    Deg,
}

/// Channel keywords of a relative color: the origin's channels in the target space, and alpha.
type Refs = Option<([f32; 3], f32)>;

/// `<space-fn>( [from <color>] c1 c2 c3 [/ alpha] )`, legacy comma syntax included.
fn channels(space: Space, a: &[&Tok], cur: Option<Color>) -> Option<Rgba> {
    let mut q = 0;
    let mut refs: Refs = None;
    if matches!(a.first(), Some(Tok::Ident(s)) if s.eq_ignore_ascii_case("from")) {
        q = 1;
        let origin = color(a, &mut q, cur)?;
        refs = Some((space.from_srgb([origin[0], origin[1], origin[2]]), origin[3]));
    }
    finish_channels(space, a, q, refs)
}

fn finish_channels(space: Space, a: &[&Tok], mut q: usize, refs: Refs) -> Option<Rgba> {
    let names = space.channel_names();
    let mut vals = [0f32; 3];
    let mut alpha = refs.map(|r| r.1).unwrap_or(1.0);
    let mut n = 0;
    let mut in_alpha = false;
    while q < a.len() {
        match a[q] {
            Tok::Comma => {
                q += 1;
                continue;
            }
            Tok::Delim('/') => {
                q += 1;
                in_alpha = true;
                continue;
            }
            _ => {}
        }
        let (v, unit) = value(a, &mut q, &names, refs)?;
        if in_alpha || n == 3 {
            alpha = if unit == Unit::Pct { v / 100.0 } else { v };
            break;
        }
        vals[n] = if space.hue_index() == Some(n) {
            v
        } else if unit == Unit::Pct {
            v / 100.0 * space.percent_ref(n)
        } else {
            v
        };
        n += 1;
    }
    if n < 3 {
        return None;
    }
    let c = space.to_srgb(vals);
    Some([c[0], c[1], c[2], alpha.clamp(0.0, 1.0)])
}

/// `color(<space> c1 c2 c3 [/ alpha])`, optionally relative (`color(from <color> srgb r g b)`).
fn color_fn(a: &[&Tok], cur: Option<Color>) -> Option<Rgba> {
    let mut q = 0;
    let mut origin = None;
    if matches!(a.first(), Some(Tok::Ident(s)) if s.eq_ignore_ascii_case("from")) {
        q = 1;
        origin = Some(color(a, &mut q, cur)?);
    }
    let space = match a.get(q) {
        Some(Tok::Ident(s)) => Space::named(&s.to_ascii_lowercase()).filter(|s| matches!(s, Space::Srgb | Space::SrgbLinear))?,
        _ => return None,
    };
    q += 1;
    let refs = origin.map(|o| (space.from_srgb([o[0], o[1], o[2]]), o[3]));
    // color() names its channels r g b (or x y z); either works as a keyword
    finish_channels(space, a, q, refs)
}

fn value(a: &[&Tok], q: &mut usize, names: &[&str; 3], refs: Refs) -> Option<(f32, Unit)> {
    let t = *a.get(*q)?;
    *q += 1;
    Some(match t {
        Tok::Number(n) => (*n, Unit::Num),
        Tok::Percentage(n) => (*n, Unit::Pct),
        Tok::Dimension(n, u) => (
            match u.to_ascii_lowercase().as_str() {
                "deg" => *n,
                "rad" => n.to_degrees(),
                "grad" => n * 0.9,
                "turn" => n * 360.0,
                _ => return None,
            },
            Unit::Deg,
        ),
        Tok::Ident(s) => {
            let s = s.to_ascii_lowercase();
            if s == "none" {
                (0.0, Unit::Num)
            } else {
                let (ch, al) = refs?;
                if s == "alpha" {
                    (al, Unit::Num)
                } else {
                    let i = names.iter().position(|n| *n == s).or_else(|| ["x", "y", "z"].iter().position(|n| *n == s))?;
                    (ch[i], Unit::Num)
                }
            }
        }
        Tok::Function(f) if f.eq_ignore_ascii_case("calc") => sum(args(a, q), &mut 0, names, refs)?,
        Tok::LParen => sum(args(a, q), &mut 0, names, refs)?,
        _ => return None,
    })
}

/// The inside of calc(): a sum of products of values.
fn sum(a: &[&Tok], q: &mut usize, names: &[&str; 3], refs: Refs) -> Option<(f32, Unit)> {
    let mut acc = product(a, q, names, refs)?;
    loop {
        let sign = match a.get(*q) {
            Some(Tok::Delim('+')) => 1.0,
            Some(Tok::Delim('-')) => -1.0,
            _ => return Some(acc),
        };
        *q += 1;
        let (v, u) = product(a, q, names, refs)?;
        let unit = if acc.1 == u { u } else { Unit::Num };
        acc = (acc.0 + sign * v, unit);
    }
}

fn product(a: &[&Tok], q: &mut usize, names: &[&str; 3], refs: Refs) -> Option<(f32, Unit)> {
    let mut acc = value(a, q, names, refs)?;
    loop {
        match a.get(*q) {
            Some(Tok::Delim('*')) => {
                *q += 1;
                let (v, u) = value(a, q, names, refs)?;
                acc = (acc.0 * v, if acc.1 == Unit::Num { u } else { acc.1 });
            }
            Some(Tok::Delim('/')) => {
                *q += 1;
                let (v, _) = value(a, q, names, refs)?;
                if v == 0.0 {
                    return None;
                }
                acc.0 /= v;
            }
            _ => return Some(acc),
        }
    }
}

/// `color-mix(in <space> [<hue> hue], <color> [<pct>], <color> [<pct>])`
fn mix(a: &[&Tok], cur: Option<Color>) -> Option<Rgba> {
    let mut parts: Vec<&[&Tok]> = Vec::new();
    let (mut depth, mut start) = (0, 0);
    for (i, t) in a.iter().enumerate() {
        match t {
            Tok::Function(_) | Tok::LParen => depth += 1,
            Tok::RParen => depth -= 1,
            Tok::Comma if depth == 0 => {
                parts.push(&a[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&a[start..]);
    let [method, x, y] = parts.as_slice() else { return None };
    if !matches!(method.first(), Some(Tok::Ident(s)) if s.eq_ignore_ascii_case("in")) {
        return None;
    }
    let space = match method.get(1) {
        Some(Tok::Ident(s)) => Space::named(&s.to_ascii_lowercase())?,
        _ => return None,
    };
    let longer = matches!(method.get(2), Some(Tok::Ident(s)) if s.eq_ignore_ascii_case("longer"));
    let item = |t: &[&Tok]| -> Option<(Rgba, Option<f32>)> {
        match (t.first(), t.last()) {
            (Some(Tok::Percentage(p)), _) => Some((color(&t[1..], &mut 0, cur)?, Some(*p))),
            (_, Some(Tok::Percentage(p))) if t.len() > 1 => Some((color(&t[..t.len() - 1], &mut 0, cur)?, Some(*p))),
            _ => Some((color(t, &mut 0, cur)?, None)),
        }
    };
    let (c1, p1) = item(x)?;
    let (c2, p2) = item(y)?;
    let (p1, p2) = match (p1, p2) {
        (None, None) => (50.0, 50.0),
        (Some(a), None) => (a, 100.0 - a),
        (None, Some(b)) => (100.0 - b, b),
        (Some(a), Some(b)) => (a, b),
    };
    let total = p1 + p2;
    if total <= 0.0 {
        return None;
    }
    let (w1, w2) = (p1 / total, p2 / total);
    let mut v1 = space.from_srgb([c1[0], c1[1], c1[2]]);
    let mut v2 = space.from_srgb([c2[0], c2[1], c2[2]]);
    let hue = space.hue_index();
    if let Some(h) = hue {
        // an achromatic color's hue is powerless: take the other's
        // (saturation for hsl, chroma for lch/oklch, whiteness + blackness for hwb)
        let gray = |v: &[f32; 3], c: &Rgba| c[3] == 0.0 || if space == Space::Hwb { v[1] + v[2] >= 100.0 } else { v[1].abs() < 1e-4 };
        if gray(&v1, &c1) {
            v1[h] = v2[h];
        } else if gray(&v2, &c2) {
            v2[h] = v1[h];
        }
        let d = v2[h] - v1[h];
        if !longer {
            if d > 180.0 {
                v1[h] += 360.0;
            } else if d < -180.0 {
                v2[h] += 360.0;
            }
        } else if d > 0.0 && d < 180.0 {
            v1[h] += 360.0;
        } else if d > -180.0 && d <= 0.0 {
            v2[h] += 360.0;
        }
    }
    let alpha = c1[3] * w1 + c2[3] * w2;
    let mut out = [0f32; 3];
    for i in 0..3 {
        out[i] = if Some(i) == hue {
            (v1[i] * w1 + v2[i] * w2).rem_euclid(360.0)
        } else {
            // premultiplied interpolation
            let m = v1[i] * c1[3] * w1 + v2[i] * c2[3] * w2;
            if alpha > 0.0 {
                m / alpha
            } else {
                0.0
            }
        };
    }
    let c = space.to_srgb(out);
    let alpha = alpha * if total < 100.0 { total / 100.0 } else { 1.0 };
    Some([c[0], c[1], c[2], alpha])
}

fn linear(c: f32) -> f32 {
    let a = c.abs();
    if a <= 0.04045 {
        c / 12.92
    } else {
        c.signum() * ((a + 0.055) / 1.055).powf(2.4)
    }
}

fn gamma(c: f32) -> f32 {
    let a = c.abs();
    if a <= 0.0031308 {
        c * 12.92
    } else {
        c.signum() * (1.055 * a.powf(1.0 / 2.4) - 0.055)
    }
}

fn polar([l, a, b]: [f32; 3]) -> [f32; 3] {
    let h = b.atan2(a).to_degrees().rem_euclid(360.0);
    [l, a.hypot(b), h]
}

fn cartesian([l, c, h]: [f32; 3]) -> [f32; 3] {
    let r = h.to_radians();
    [l, c.max(0.0) * r.cos(), c.max(0.0) * r.sin()]
}

fn hsl_to_srgb([h, s, l]: [f32; 3]) -> [f32; 3] {
    let (s, l) = (s / 100.0, l / 100.0);
    let f = |n: f32| {
        let k = (n + h / 30.0).rem_euclid(12.0);
        let a = s * l.min(1.0 - l);
        l - a * (k - 3.0).min(9.0 - k).clamp(-1.0, 1.0)
    };
    [f(0.0), f(8.0), f(4.0)]
}

fn srgb_to_hsl([r, g, b]: [f32; 3]) -> [f32; 3] {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    let (mut h, mut s) = (0.0, 0.0);
    if d > 0.0 {
        s = if l <= 0.0 || l >= 1.0 { 0.0 } else { (max - l) / l.min(1.0 - l) };
        h = if max == r {
            (g - b) / d + if g < b { 6.0 } else { 0.0 }
        } else if max == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        } * 60.0;
    }
    [h, s * 100.0, l * 100.0]
}

fn srgb_to_oklab(c: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = c.map(linear);
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

fn oklab_to_srgb([l, a, b]: [f32; 3]) -> [f32; 3] {
    let l_ = (l + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let m_ = (l - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let s_ = (l - 0.0894841775 * a - 1.2914855480 * b).powi(3);
    [
        4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_,
        -1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_,
        -0.0041960863 * l_ - 0.7034186147 * m_ + 1.7076147010 * s_,
    ]
    .map(gamma)
}

const D50: [f32; 3] = [0.96422, 1.0, 0.82521];
const LAB_E: f32 = 216.0 / 24389.0;
const LAB_K: f32 = 24389.0 / 27.0;

fn srgb_to_lab(c: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = c.map(linear);
    let x = 0.4360747 * r + 0.3850649 * g + 0.1430804 * b;
    let y = 0.2225045 * r + 0.7168786 * g + 0.0606169 * b;
    let z = 0.0139322 * r + 0.0971045 * g + 0.7141733 * b;
    let f = |t: f32| if t > LAB_E { t.cbrt() } else { (LAB_K * t + 16.0) / 116.0 };
    let (fx, fy, fz) = (f(x / D50[0]), f(y / D50[1]), f(z / D50[2]));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn lab_to_srgb([l, a, b]: [f32; 3]) -> [f32; 3] {
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let x = if fx.powi(3) > LAB_E { fx.powi(3) } else { (116.0 * fx - 16.0) / LAB_K };
    let y = if l > LAB_K * LAB_E { fy.powi(3) } else { l / LAB_K };
    let z = if fz.powi(3) > LAB_E { fz.powi(3) } else { (116.0 * fz - 16.0) / LAB_K };
    let (x, y, z) = (x * D50[0], y * D50[1], z * D50[2]);
    [
        3.1338561 * x - 1.6168667 * y - 0.4906146 * z,
        -0.9787684 * x + 1.9161415 * y + 0.0334540 * z,
        0.0719453 * x - 0.2289914 * y + 1.4052427 * z,
    ]
    .map(gamma)
}
