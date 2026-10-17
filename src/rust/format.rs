//! `format!`-family formatting: compile-time parsing of format strings and runtime
//! Display/Debug formatting (including user `impl Display` / `#[derive(Debug)]`).

use std::rc::Rc;

use super::ast::Pos;
use super::runtime::{as_option, panic, type_name, Ctx, R};
use super::value::*;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Trait {
    Display,
    Debug,
    LowerHex,
    UpperHex,
    Binary,
    Octal,
    LowerExp,
}

#[derive(Debug, Clone)]
pub enum Count {
    Literal(usize),
    /// `{:width$}` / `{:.*}`: taken from an argument
    Arg(ArgRef),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ArgRef {
    Index(usize),
    Name(String),
}

#[derive(Debug, Clone)]
pub struct Spec {
    pub arg: ArgRef,
    pub fill: char,
    pub align: Option<char>,
    pub sign_plus: bool,
    pub alternate: bool,
    pub zero: bool,
    pub width: Option<Count>,
    pub precision: Option<Count>,
    pub trait_: Trait,
}

#[derive(Debug, Clone)]
pub enum Piece {
    Lit(String),
    Arg(Spec),
}

/// Parse a format string. Implicit positional arguments are numbered left to right.
pub fn parse(fmt: &str) -> Result<Vec<Piece>, String> {
    let chars: Vec<char> = fmt.chars().collect();
    let mut out = Vec::new();
    let mut lit = String::new();
    let mut next_pos = 0usize;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '{' {
            if chars.get(i + 1) == Some(&'{') {
                lit.push('{');
                i += 2;
                continue;
            }
            let start = i + 1;
            let mut j = start;
            while j < chars.len() && chars[j] != '}' {
                j += 1;
            }
            if j >= chars.len() {
                return Err("invalid format string: expected `}` but string was terminated".into());
            }
            let inner: String = chars[start..j].iter().collect();
            if !lit.is_empty() {
                out.push(Piece::Lit(std::mem::take(&mut lit)));
            }
            out.push(Piece::Arg(parse_spec(&inner, &mut next_pos)?));
            i = j + 1;
            continue;
        }
        if c == '}' {
            if chars.get(i + 1) == Some(&'}') {
                lit.push('}');
                i += 2;
                continue;
            }
            return Err("invalid format string: unmatched `}` found".into());
        }
        lit.push(c);
        i += 1;
    }
    if !lit.is_empty() {
        out.push(Piece::Lit(lit));
    }
    Ok(out)
}

fn parse_arg_ref(s: &str, next: &mut usize) -> ArgRef {
    if s.is_empty() {
        let r = ArgRef::Index(*next);
        *next += 1;
        r
    } else if let Ok(n) = s.parse::<usize>() {
        ArgRef::Index(n)
    } else {
        ArgRef::Name(s.to_string())
    }
}

fn parse_spec(inner: &str, next: &mut usize) -> Result<Spec, String> {
    let (arg, spec) = match inner.find(':') {
        Some(i) => (&inner[..i], &inner[i + 1..]),
        None => (inner, ""),
    };
    let arg = arg.trim();
    let mut s = Spec { arg: ArgRef::Index(0), fill: ' ', align: None, sign_plus: false, alternate: false, zero: false, width: None, precision: None, trait_: Trait::Display };
    let chars: Vec<char> = spec.chars().collect();
    let mut i = 0;
    // fill + align
    if chars.len() >= 2 && matches!(chars[1], '<' | '>' | '^') {
        s.fill = chars[0];
        s.align = Some(chars[1]);
        i = 2;
    } else if !chars.is_empty() && matches!(chars[0], '<' | '>' | '^') {
        s.align = Some(chars[0]);
        i = 1;
    }
    if chars.get(i) == Some(&'+') {
        s.sign_plus = true;
        i += 1;
    } else if chars.get(i) == Some(&'-') {
        i += 1;
    }
    if chars.get(i) == Some(&'#') {
        s.alternate = true;
        i += 1;
    }
    if chars.get(i) == Some(&'0') {
        s.zero = true;
        i += 1;
    }
    // width: digits, or name$ / n$
    let wstart = i;
    while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
        i += 1;
    }
    let wtxt: String = chars[wstart..i].iter().collect();
    if chars.get(i) == Some(&'$') {
        i += 1;
        s.width = Some(Count::Arg(if let Ok(n) = wtxt.parse() { ArgRef::Index(n) } else { ArgRef::Name(wtxt) }));
    } else if !wtxt.is_empty() {
        // digits only, else it's the type (e.g. `x`, `?`)
        let digits: String = wtxt.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            s.width = Some(Count::Literal(digits.parse().unwrap_or(0)));
        }
        i = wstart + digits.chars().count();
    }
    if chars.get(i) == Some(&'.') {
        i += 1;
        if chars.get(i) == Some(&'*') {
            i += 1;
            // `.*` takes the precision from the next positional argument (before the value)
            let p = ArgRef::Index(*next);
            *next += 1;
            s.precision = Some(Count::Arg(p));
        } else {
            let pstart = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let ptxt: String = chars[pstart..i].iter().collect();
            if chars.get(i) == Some(&'$') {
                i += 1;
                s.precision = Some(Count::Arg(if let Ok(n) = ptxt.parse() { ArgRef::Index(n) } else { ArgRef::Name(ptxt) }));
            } else {
                let digits: String = ptxt.chars().take_while(|c| c.is_ascii_digit()).collect();
                s.precision = Some(Count::Literal(digits.parse().map_err(|_| "invalid precision")?));
                i = pstart + digits.chars().count();
            }
        }
    }
    let ty: String = chars[i..].iter().collect();
    s.trait_ = match ty.as_str() {
        "" => Trait::Display,
        "?" | "#?" => Trait::Debug,
        "x" | "x?" => Trait::LowerHex,
        "X" | "X?" => Trait::UpperHex,
        "b" => Trait::Binary,
        "o" => Trait::Octal,
        "e" => Trait::LowerExp,
        other => return Err(format!("unknown format trait `{other}`")),
    };
    s.arg = parse_arg_ref(arg, next);
    Ok(s)
}

fn pad(s: String, spec: &Spec, width: Option<usize>, numeric: bool) -> String {
    let Some(w) = width else { return s };
    let n = s.chars().count();
    if n >= w {
        return s;
    }
    let extra = w - n;
    if spec.zero && numeric && spec.align.is_none() {
        // zeros go after the sign / prefix
        let (sign, rest) = if s.starts_with('-') || s.starts_with('+') { s.split_at(1) } else { ("", s.as_str()) };
        let (prefix, digits) = if rest.starts_with("0x") || rest.starts_with("0b") || rest.starts_with("0o") { rest.split_at(2) } else { ("", rest) };
        return format!("{sign}{prefix}{}{digits}", "0".repeat(extra));
    }
    let fill = spec.fill.to_string();
    let align = spec.align.unwrap_or(if numeric { '>' } else { '<' });
    match align {
        '>' => format!("{}{s}", fill.repeat(extra)),
        '^' => format!("{}{s}{}", fill.repeat(extra / 2), fill.repeat(extra - extra / 2)),
        _ => format!("{s}{}", fill.repeat(extra)),
    }
}

/// Format one argument according to `spec`.
pub fn format_arg(ctx: &mut Ctx, v: &Value, spec: &Spec, width: Option<usize>, precision: Option<usize>, pos: Pos) -> R<String> {
    let v = v.deref_all();
    let numeric = matches!(v, Value::Int(..) | Value::Float(..));
    let s = match spec.trait_ {
        Trait::Debug => debug(ctx, &v, spec.alternate, 0, pos)?,
        Trait::LowerHex | Trait::UpperHex | Trait::Binary | Trait::Octal => {
            let Value::Int(x, k) = v else { return panic(format!("`{}` doesn't implement a radix format", type_name(&v)), pos) };
            let bits = k.bits();
            let u = if x < 0 { (x as u128) & if bits == 128 { u128::MAX } else { (1u128 << bits) - 1 } } else { x as u128 };
            let (digits, prefix) = match spec.trait_ {
                Trait::LowerHex => (format!("{u:x}"), "0x"),
                Trait::UpperHex => (format!("{u:X}"), "0x"),
                Trait::Binary => (format!("{u:b}"), "0b"),
                _ => (format!("{u:o}"), "0o"),
            };
            if spec.alternate { format!("{prefix}{digits}") } else { digits }
        }
        Trait::LowerExp => match v {
            Value::Float(f, _) => match precision {
                Some(p) => format!("{f:.p$e}"),
                None => format!("{f:e}"),
            },
            Value::Int(x, _) => format!("{:e}", x as f64),
            other => return panic(format!("`{}` doesn't implement `LowerExp`", type_name(&other)), pos),
        },
        Trait::Display => match &v {
            Value::Float(f, f32) => match precision {
                Some(p) => format!("{f:.p$}"),
                None => float_display(*f, *f32),
            },
            Value::Str(s) => match precision {
                Some(p) => s.borrow().chars().take(p).collect(),
                None => s.borrow().clone(),
            },
            _ => display(ctx, &v, pos)?,
        },
    };
    let s = if spec.sign_plus && numeric && !s.starts_with('-') { format!("+{s}") } else { s };
    Ok(pad(s, spec, width, numeric))
}

/// Rust's float Display: shortest round-trip representation, no exponent.
pub fn float_display(f: f64, f32: bool) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let s = if f32 { format!("{}", f as f32) } else { format!("{f}") };
    s
}

pub fn display(ctx: &mut Ctx, v: &Value, pos: Pos) -> R<String> {
    let v = v.deref_all();
    Ok(match &v {
        Value::Unit => return panic("`()` doesn't implement `std::fmt::Display`", pos),
        Value::Bool(b) => b.to_string(),
        Value::Int(i, _) => i.to_string(),
        Value::Float(f, f32) => float_display(*f, *f32),
        Value::Char(c) => c.to_string(),
        Value::Str(s) => s.borrow().clone(),
        Value::Struct(_) | Value::Enum(_) => {
            let tn = type_name(&v);
            if let Some(code) = ctx.prog.method(&tn, "fmt#Display").or_else(|| ctx.prog.method(&tn, "to_string")) {
                return user_fmt(ctx, &code, &v, pos);
            }
            if tn == "Ordering" || tn == "Option" || tn == "Result" {
                return panic(format!("`{tn}` doesn't implement `std::fmt::Display`"), pos);
            }
            return panic(format!("`{tn}` doesn't implement `std::fmt::Display` (add `impl fmt::Display for {tn}` or use `{{:?}}`)"), pos);
        }
        Value::Cell(c) => display(ctx, &c.borrow(), pos)?,
        Value::Js(j) => match ctx.interp() {
            Some(it) => it.to_string_lossy(j),
            None => "[JsValue]".into(),
        },
        Value::Formatter(buf, _) => buf.borrow().clone(),
        other => return panic(format!("`{}` doesn't implement `std::fmt::Display`", type_name(other)), pos),
    })
}

fn user_fmt(ctx: &mut Ctx, code: &Rc<super::runtime::FnCode>, v: &Value, pos: Pos) -> R<String> {
    let buf = Rc::new(std::cell::RefCell::new(String::new()));
    let f = Value::Formatter(buf.clone(), Rc::new(FmtSpec::default()));
    let r = ctx.call_code(code, &[], vec![v.clone(), f], pos)?;
    // `fn to_string(&self) -> String` fallback
    if let Value::Str(s) = r.deref_all() {
        if buf.borrow().is_empty() {
            return Ok(s.borrow().clone());
        }
    }
    let out = buf.borrow().clone();
    Ok(out)
}

fn escape_debug_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn list(items: Vec<String>, open: &str, close: &str, pretty: bool, indent: usize, trailing_comma_single: bool) -> String {
    if items.is_empty() {
        return format!("{open}{close}");
    }
    if pretty {
        let pad = "    ".repeat(indent + 1);
        let mut s = format!("{open}\n");
        for it in &items {
            s.push_str(&format!("{pad}{it},\n"));
        }
        s.push_str(&"    ".repeat(indent));
        s.push_str(close);
        s
    } else {
        let mut s = format!("{open}{}", items.join(", "));
        if trailing_comma_single && items.len() == 1 {
            s.push(',');
        }
        s.push_str(close);
        s
    }
}

pub fn debug(ctx: &mut Ctx, v: &Value, pretty: bool, indent: usize, pos: Pos) -> R<String> {
    let v = v.deref_all();
    Ok(match &v {
        Value::Unit => "()".into(),
        Value::Str(s) => escape_debug_str(&s.borrow()),
        Value::Char(c) => match c {
            '\'' => "'\\''".into(),
            '\n' => "'\\n'".into(),
            '\t' => "'\\t'".into(),
            '\\' => "'\\\\'".into(),
            c => format!("'{c}'"),
        },
        Value::Float(f, f32) => {
            let s = float_display(*f, *f32);
            if f.is_finite() && !s.contains('.') && !s.contains('e') { format!("{s}.0") } else { s }
        }
        Value::Vec(items) | Value::Array(items) => {
            let items = items.borrow().clone();
            let mut parts = Vec::new();
            for it in &items {
                parts.push(debug(ctx, it, pretty, indent + 1, pos)?);
            }
            list(parts, "[", "]", pretty, indent, false)
        }
        Value::Tuple(items) => {
            let items = items.borrow().clone();
            let mut parts = Vec::new();
            for it in &items {
                parts.push(debug(ctx, it, pretty, indent + 1, pos)?);
            }
            list(parts, "(", ")", pretty, indent, true)
        }
        Value::Map(m) => {
            let (keys, vals, is_set) = {
                let m = m.borrow();
                (m.keys.clone(), m.vals.clone(), m.is_set)
            };
            let mut parts = Vec::new();
            for (k, val) in keys.iter().zip(vals.iter()) {
                let ks = debug(ctx, k, pretty, indent + 1, pos)?;
                if is_set {
                    parts.push(ks);
                } else {
                    parts.push(format!("{ks}: {}", debug(ctx, val, pretty, indent + 1, pos)?));
                }
            }
            list(parts, "{", "}", pretty, indent, false)
        }
        Value::Struct(s) => {
            let tn = s.ty.name.clone();
            if let Some(code) = ctx.prog.method(&tn, "fmt#Debug") {
                return user_fmt(ctx, &code, &v, pos);
            }
            let fields = s.fields.borrow().clone();
            if s.ty.is_tuple {
                let mut parts = Vec::new();
                for f in &fields {
                    parts.push(debug(ctx, f, pretty, indent + 1, pos)?);
                }
                format!("{tn}{}", list(parts, "(", ")", pretty, indent, false))
            } else if fields.is_empty() {
                tn
            } else {
                let mut parts = Vec::new();
                for (name, f) in s.ty.fields.iter().zip(fields.iter()) {
                    parts.push(format!("{name}: {}", debug(ctx, f, pretty, indent + 1, pos)?));
                }
                if pretty {
                    format!("{tn} {}", list(parts, "{", "}", true, indent, false))
                } else {
                    format!("{tn} {{ {} }}", parts.join(", "))
                }
            }
        }
        Value::Enum(e) => {
            let tn = e.ty.name.clone();
            if let Some(code) = ctx.prog.method(&tn, "fmt#Debug") {
                return user_fmt(ctx, &code, &v, pos);
            }
            let (vname, fnames, is_tuple, _) = e.ty.variants[e.variant].clone();
            let fields = e.fields.borrow().clone();
            if fields.is_empty() {
                vname
            } else if is_tuple {
                let mut parts = Vec::new();
                for f in &fields {
                    parts.push(debug(ctx, f, pretty, indent + 1, pos)?);
                }
                format!("{vname}{}", list(parts, "(", ")", pretty, indent, false))
            } else {
                let mut parts = Vec::new();
                for (name, f) in fnames.iter().zip(fields.iter()) {
                    parts.push(format!("{name}: {}", debug(ctx, f, pretty, indent + 1, pos)?));
                }
                if pretty {
                    format!("{vname} {}", list(parts, "{", "}", true, indent, false))
                } else {
                    format!("{vname} {{ {} }}", parts.join(", "))
                }
            }
        }
        Value::Cell(c) => {
            let inner = c.borrow().clone();
            format!("RefCell {{ value: {} }}", debug(ctx, &inner, pretty, indent, pos)?)
        }
        Value::Range(r) => {
            let a = if matches!(r.0, Value::Unit) { String::new() } else { debug(ctx, &r.0, false, 0, pos)? };
            let b = if matches!(r.1, Value::Unit) { String::new() } else { debug(ctx, &r.1, false, 0, pos)? };
            format!("{a}{}{b}", if r.2 { "..=" } else { ".." })
        }
        Value::Func(_) => "<closure>".into(),
        Value::Iter(_) => "Iter { .. }".into(),
        Value::Js(_) => format!("JsValue({})", display(ctx, &v, pos)?),
        other => display(ctx, other, pos)?,
    })
}

/// Run a parsed format string against evaluated arguments.
pub fn run(ctx: &mut Ctx, pieces: &[Piece], args: &[Value], named: &[(String, Value)], pos: Pos) -> R<String> {
    let lookup = |r: &ArgRef| -> Option<&Value> {
        match r {
            ArgRef::Index(i) => args.get(*i),
            ArgRef::Name(n) => named.iter().find(|(k, _)| k == n).map(|(_, v)| v),
        }
    };
    let count = |c: &Option<Count>| -> R<Option<usize>> {
        Ok(match c {
            None => None,
            Some(Count::Literal(n)) => Some(*n),
            Some(Count::Arg(r)) => match lookup(r).map(|v| v.deref_all()) {
                Some(Value::Int(n, _)) if n >= 0 => Some(n as usize),
                _ => return panic("width/precision argument must be a `usize`", pos),
            },
        })
    };
    let mut out = String::new();
    for p in pieces {
        match p {
            Piece::Lit(s) => out.push_str(s),
            Piece::Arg(spec) => {
                let Some(v) = lookup(&spec.arg) else { return panic("format string references a missing argument", pos) };
                let v = v.clone();
                let w = count(&spec.width)?;
                let pr = count(&spec.precision)?;
                out.push_str(&format_arg(ctx, &v, spec, w, pr, pos)?);
            }
        }
    }
    let _ = as_option;
    Ok(out)
}
