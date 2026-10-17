//! String constructor and prototype (UTF-16 index semantics).

use std::rc::Rc;

use super::regexp::{self, regex_of};
use crate::js::interp::{arg, throw, Interp, R};
use crate::js::value::*;

fn this_str(it: &mut Interp, this: &Value) -> R<JsStr> {
    match this {
        Value::Str(s) => Ok(s.clone()),
        Value::Undefined | Value::Null => throw!(it, "TypeError", "String.prototype method called on null or undefined"),
        v => it.to_str(v),
    }
}

fn units(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn from_units(u: &[u16]) -> String {
    String::from_utf16_lossy(u)
}

fn rel(it: &mut Interp, v: &Value, len: usize, default: f64) -> R<usize> {
    let n = if v.is_undefined() { default } else { it.to_integer(v)? };
    let l = len as f64;
    Ok(if n < 0.0 { (l + n).max(0.0) as usize } else { n.min(l) as usize })
}

fn clamp_idx(it: &mut Interp, v: &Value, len: usize, default: f64) -> R<usize> {
    let n = if v.is_undefined() { default } else { it.to_integer(v)? };
    Ok(n.clamp(0.0, len as f64) as usize)
}

fn find_units(hay: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(from.min(hay.len()));
    }
    if needle.len() > hay.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

fn is_js_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

pub fn setup(it: &mut Interp) {
    let sp = it.p.string.clone();
    let ctor = it.define_ctor("String", 1, &sp, |it, this, args| {
        let s = match args.first() {
            None => Rc::from(""),
            Some(Value::Sym(s)) if !it.constructing => Rc::from(format!("Symbol({})", s.0.description.as_deref().unwrap_or("")).as_str()),
            Some(v) => it.to_str(v)?,
        };
        if it.constructing {
            it.constructing = false;
            if let Value::Obj(o) = &this {
                o.borrow_mut().kind = ObjKind::Str(s);
                o.borrow_mut().class = "String";
                return Ok(this);
            }
        }
        Ok(Value::Str(s))
    });
    it.define_method(&ctor, "fromCharCode", 1, |it, _, args| {
        let mut u = Vec::new();
        for a in args {
            u.push(to_uint32(it.to_number(a)?) as u16);
        }
        Ok(Value::str(&from_units(&u)))
    });
    it.define_method(&ctor, "fromCodePoint", 1, |it, _, args| {
        let mut s = String::new();
        for a in args {
            let n = it.to_number(a)?;
            match char::from_u32(n as u32) {
                Some(c) if n.fract() == 0.0 && n >= 0.0 => s.push(c),
                _ => throw!(it, "RangeError", "Invalid code point {}", num_to_string(n)),
            }
        }
        Ok(Value::str(&s))
    });
    it.define_method(&ctor, "raw", 1, |it, _, args| {
        let strs = arg(args, 0);
        let raw = it.get_str(&strs, "raw")?;
        let parts = super::array_like_to_vec(it, &raw)?;
        let mut s = String::new();
        for (i, p) in parts.iter().enumerate() {
            s.push_str(&it.to_str(p)?);
            if i + 1 < parts.len() {
                if let Some(v) = args.get(i + 1) {
                    s.push_str(&it.to_str(v)?);
                }
            }
        }
        Ok(Value::str(&s))
    });

    it.define_method(&sp, "toString", 0, |it, this, _| {
        match &this {
            Value::Str(_) => Ok(this.clone()),
            Value::Obj(o) => match &o.borrow().kind {
                ObjKind::Str(s) => Ok(Value::Str(s.clone())),
                _ => throw!(it, "TypeError", "String.prototype.toString requires that 'this' be a String"),
            },
            _ => throw!(it, "TypeError", "String.prototype.toString requires that 'this' be a String"),
        }
    });
    let ts = sp.get_own("toString").unwrap();
    sp.set_hidden("valueOf", ts);
    it.define_method(&sp, "charAt", 1, |it, this, args| {
        let s = this_str(it, &this)?;
        let i = it.to_integer(&arg(args, 0))?;
        if i < 0.0 {
            return Ok(Value::str(""));
        }
        Ok(Value::str(&crate::js::interp::string_index(&s, i as usize).unwrap_or_default()))
    });
    it.define_method(&sp, "charCodeAt", 1, |it, this, args| {
        let s = this_str(it, &this)?;
        let i = it.to_integer(&arg(args, 0))?;
        if i < 0.0 {
            return Ok(Value::Num(f64::NAN));
        }
        let u = if s.is_ascii() { s.as_bytes().get(i as usize).map(|b| *b as u16) } else { s.encode_utf16().nth(i as usize) };
        Ok(Value::Num(u.map(|v| v as f64).unwrap_or(f64::NAN)))
    });
    it.define_method(&sp, "codePointAt", 1, |it, this, args| {
        let s = this_str(it, &this)?;
        let i = it.to_integer(&arg(args, 0))?;
        let u = units(&s);
        if i < 0.0 || i as usize >= u.len() {
            return Ok(Value::Undefined);
        }
        let i = i as usize;
        let hi = u[i];
        if (0xD800..0xDC00).contains(&hi) && i + 1 < u.len() && (0xDC00..0xE000).contains(&u[i + 1]) {
            return Ok(Value::Num((0x10000 + ((hi as u32 - 0xD800) << 10) + (u[i + 1] as u32 - 0xDC00)) as f64));
        }
        Ok(Value::Num(hi as f64))
    });
    it.define_method(&sp, "at", 1, |it, this, args| {
        let s = this_str(it, &this)?;
        let u = units(&s);
        let i = it.to_integer(&arg(args, 0))?;
        let idx = if i < 0.0 { u.len() as f64 + i } else { i };
        if idx < 0.0 || idx as usize >= u.len() {
            return Ok(Value::Undefined);
        }
        Ok(Value::str(&from_units(&u[idx as usize..idx as usize + 1])))
    });
    it.define_method(&sp, "indexOf", 1, |it, this, args| {
        let s = this_str(it, &this)?;
        let n = it.to_str(&arg(args, 0))?;
        let hay = units(&s);
        let from = clamp_idx(it, &arg(args, 1), hay.len(), 0.0)?;
        Ok(Value::Num(find_units(&hay, &units(&n), from).map(|i| i as f64).unwrap_or(-1.0)))
    });
    it.define_method(&sp, "lastIndexOf", 1, |it, this, args| {
        let s = this_str(it, &this)?;
        let n = units(&it.to_str(&arg(args, 0))?);
        let hay = units(&s);
        let from = match arg(args, 1) {
            Value::Undefined => hay.len(),
            v => {
                let x = it.to_number(&v)?;
                if x.is_nan() { hay.len() } else { x.max(0.0) as usize }
            }
        };
        if n.len() > hay.len() {
            return Ok(Value::Num(-1.0));
        }
        let start = from.min(hay.len() - n.len());
        for i in (0..=start).rev() {
            if hay[i..i + n.len()] == n[..] {
                return Ok(Value::Num(i as f64));
            }
        }
        Ok(Value::Num(-1.0))
    });
    it.define_method(&sp, "includes", 1, |it, this, args| {
        let s = this_str(it, &this)?;
        if regex_of(&arg(args, 0)).is_some() {
            throw!(it, "TypeError", "First argument to String.prototype.includes must not be a regular expression");
        }
        let n = it.to_str(&arg(args, 0))?;
        let hay = units(&s);
        let from = clamp_idx(it, &arg(args, 1), hay.len(), 0.0)?;
        Ok(Value::Bool(find_units(&hay, &units(&n), from).is_some()))
    });
    it.define_method(&sp, "startsWith", 1, |it, this, args| {
        let s = this_str(it, &this)?;
        let n = units(&it.to_str(&arg(args, 0))?);
        let hay = units(&s);
        let from = clamp_idx(it, &arg(args, 1), hay.len(), 0.0)?;
        Ok(Value::Bool(hay.len() >= from + n.len() && hay[from..from + n.len()] == n[..]))
    });
    it.define_method(&sp, "endsWith", 1, |it, this, args| {
        let s = this_str(it, &this)?;
        let n = units(&it.to_str(&arg(args, 0))?);
        let hay = units(&s);
        let end = clamp_idx(it, &arg(args, 1), hay.len(), hay.len() as f64)?;
        Ok(Value::Bool(end >= n.len() && hay[end - n.len()..end] == n[..]))
    });
    it.define_method(&sp, "slice", 2, |it, this, args| {
        let s = this_str(it, &this)?;
        if s.is_ascii() {
            let a = rel(it, &arg(args, 0), s.len(), 0.0)?;
            let b = rel(it, &arg(args, 1), s.len(), s.len() as f64)?;
            return Ok(Value::str(if a < b { &s[a..b] } else { "" }));
        }
        let u = units(&s);
        let a = rel(it, &arg(args, 0), u.len(), 0.0)?;
        let b = rel(it, &arg(args, 1), u.len(), u.len() as f64)?;
        Ok(Value::str(&if a < b { from_units(&u[a..b]) } else { String::new() }))
    });
    it.define_method(&sp, "substring", 2, |it, this, args| {
        let s = this_str(it, &this)?;
        let u = units(&s);
        let a = clamp_idx(it, &arg(args, 0), u.len(), 0.0)?;
        let b = clamp_idx(it, &arg(args, 1), u.len(), u.len() as f64)?;
        let (a, b) = if a > b { (b, a) } else { (a, b) };
        Ok(Value::str(&from_units(&u[a..b])))
    });
    it.define_method(&sp, "substr", 2, |it, this, args| {
        let s = this_str(it, &this)?;
        let u = units(&s);
        let a = rel(it, &arg(args, 0), u.len(), 0.0)?;
        let len = if arg(args, 1).is_undefined() { u.len() } else { it.to_integer(&arg(args, 1))?.max(0.0) as usize };
        let b = (a + len).min(u.len());
        Ok(Value::str(&from_units(&u[a..b])))
    });
    for (name, up) in [("toUpperCase", true), ("toLowerCase", false), ("toLocaleUpperCase", true), ("toLocaleLowerCase", false)] {
        it.define_method(&sp, name, 0, move |it, this, _| {
            let s = this_str(it, &this)?;
            Ok(Value::str(&if up { s.to_uppercase() } else { s.to_lowercase() }))
        });
    }
    it.define_method(&sp, "trim", 0, |it, this, _| Ok(Value::str(this_str(it, &this)?.trim_matches(is_js_space))));
    it.define_method(&sp, "trimStart", 0, |it, this, _| Ok(Value::str(this_str(it, &this)?.trim_start_matches(is_js_space))));
    it.define_method(&sp, "trimEnd", 0, |it, this, _| Ok(Value::str(this_str(it, &this)?.trim_end_matches(is_js_space))));
    let ts = sp.get_own("trimStart").unwrap();
    sp.set_hidden("trimLeft", ts);
    let te = sp.get_own("trimEnd").unwrap();
    sp.set_hidden("trimRight", te);
    for (name, start) in [("padStart", true), ("padEnd", false)] {
        it.define_method(&sp, name, 2, move |it, this, args| {
            let s = this_str(it, &this)?;
            let target = it.to_length(&arg(args, 0))?;
            let fill = match arg(args, 1) {
                Value::Undefined => " ".to_string(),
                v => it.to_str(&v)?.to_string(),
            };
            let u = units(&s);
            if target <= u.len() || fill.is_empty() {
                return Ok(Value::Str(s));
            }
            let fu = units(&fill);
            let need = target - u.len();
            let pad: Vec<u16> = fu.iter().cycle().take(need).copied().collect();
            let out = if start { [pad, u].concat() } else { [u, pad].concat() };
            Ok(Value::str(&from_units(&out)))
        });
    }
    it.define_method(&sp, "repeat", 1, |it, this, args| {
        let s = this_str(it, &this)?;
        let n = it.to_integer(&arg(args, 0))?;
        if n < 0.0 || n.is_infinite() || (n * s.len() as f64) > 1e9 {
            throw!(it, "RangeError", "Invalid count value: {}", num_to_string(n));
        }
        Ok(Value::str(&s.repeat(n as usize)))
    });
    it.define_method(&sp, "concat", 1, |it, this, args| {
        let mut s = this_str(it, &this)?.to_string();
        for a in args {
            s.push_str(&it.to_str(a)?);
        }
        Ok(Value::str(&s))
    });
    it.define_method(&sp, "localeCompare", 1, |it, this, args| {
        let a = this_str(it, &this)?;
        let b = it.to_str(&arg(args, 0))?;
        // approximate locale collation: case-insensitive first, then by code point
        let ka = a.to_lowercase();
        let kb = b.to_lowercase();
        let o = ka.cmp(&kb).then_with(|| b.cmp(&a));
        Ok(Value::Num(match o {
            std::cmp::Ordering::Less => -1.0,
            std::cmp::Ordering::Equal => 0.0,
            std::cmp::Ordering::Greater => 1.0,
        }))
    });
    it.define_method(&sp, "normalize", 0, |it, this, _| Ok(Value::Str(this_str(it, &this)?)));
    it.define_method(&sp, "isWellFormed", 0, |_, _, _| Ok(Value::Bool(true)));
    it.define_method(&sp, "split", 2, |it, this, args| {
        let s = this_str(it, &this)?;
        let sep = arg(args, 0);
        let limit = if arg(args, 1).is_undefined() { u32::MAX as usize } else { to_uint32(it.to_number(&arg(args, 1))?) as usize };
        if regex_of(&sep).is_some() {
            return regexp::regex_split(it, &sep, &s, limit);
        }
        if sep.is_undefined() {
            return Ok(it.new_array(vec![Value::Str(s)]));
        }
        let sep = it.to_str(&sep)?;
        let parts: Vec<Value> = if sep.is_empty() {
            units(&s).iter().map(|u| Value::str(&from_units(&[*u]))).take(limit).collect()
        } else {
            s.split(&*sep).map(Value::str).take(limit).collect()
        };
        Ok(it.new_array(parts))
    });
    for (name, all) in [("replace", false), ("replaceAll", true)] {
        it.define_method(&sp, name, 2, move |it, this, args| {
            let s = this_str(it, &this)?;
            let pat = arg(args, 0);
            let rep = arg(args, 1);
            if let Some(re) = regex_of(&pat) {
                if all && !re.global {
                    throw!(it, "TypeError", "replaceAll must be called with a global RegExp");
                }
                return regexp::regex_replace(it, &pat, &s, &rep, false);
            }
            let p = it.to_str(&pat)?;
            let chars: Vec<char> = s.chars().collect();
            let pchars: Vec<char> = p.chars().collect();
            let mut out = String::new();
            let mut last = 0;
            let mut pos = 0;
            loop {
                let found = if pchars.is_empty() {
                    if pos <= chars.len() { Some(pos) } else { None }
                } else {
                    (pos..=chars.len().saturating_sub(pchars.len())).find(|&i| i + pchars.len() <= chars.len() && chars[i..i + pchars.len()] == pchars[..])
                };
                let Some(i) = found else { break };
                out.extend(&chars[last..i]);
                let caps = vec![Some((i, i + pchars.len()))];
                if rep.is_callable() {
                    let r = it.call(&rep, Value::Undefined, &[Value::Str(p.clone()), Value::Num(regexp::u16_of(&chars, i) as f64), Value::Str(s.clone())])?;
                    out.push_str(&it.to_str(&r)?);
                } else {
                    let t = it.to_str(&rep)?;
                    out.push_str(&regexp::expand_replacement(&t, &chars, &caps, &[]));
                }
                last = i + pchars.len();
                pos = if pchars.is_empty() { i + 1 } else { last };
                if !all {
                    break;
                }
                if pchars.is_empty() && i < chars.len() {
                    out.push(chars[i]);
                    last = i + 1;
                }
            }
            if last <= chars.len() {
                out.extend(&chars[last..]);
            }
            Ok(Value::str(&out))
        });
    }
    it.define_method(&sp, "match", 1, |it, this, args| {
        let s = this_str(it, &this)?;
        let pat = arg(args, 0);
        let rx = if regex_of(&pat).is_some() {
            pat
        } else {
            let p = if pat.is_undefined() { String::new() } else { it.to_str(&pat)?.to_string() };
            regexp::new_regexp(it, &p, "")?
        };
        let re = regex_of(&rx).unwrap();
        let chars: Vec<char> = s.chars().collect();
        if re.global {
            let mut out = Vec::new();
            let mut pos = 0;
            while pos <= chars.len() {
                let Some(c) = re.exec_at(&chars, pos) else { break };
                let (a, b) = c[0].unwrap();
                out.push(Value::str(&chars[a..b].iter().collect::<String>()));
                pos = if a == b { b + 1 } else { b };
            }
            it.set_str(&rx, "lastIndex", Value::Num(0.0))?;
            return Ok(if out.is_empty() { Value::Null } else { it.new_array(out) });
        }
        match regexp::exec_raw(it, &rx, &chars)? {
            Some(caps) => Ok(regexp::match_object(it, &re, &chars, &caps, &s)),
            None => Ok(Value::Null),
        }
    });
    it.define_method(&sp, "matchAll", 1, |it, this, args| {
        let s = this_str(it, &this)?;
        let pat = arg(args, 0);
        let rx = if regex_of(&pat).is_some() {
            pat
        } else {
            let p = it.to_str(&pat)?;
            regexp::new_regexp(it, &p, "g")?
        };
        let re = regex_of(&rx).unwrap();
        if !re.global {
            throw!(it, "TypeError", "matchAll must be called with a global RegExp");
        }
        let chars: Vec<char> = s.chars().collect();
        let mut out = Vec::new();
        let mut pos = 0;
        while pos <= chars.len() {
            let Some(c) = re.exec_at(&chars, pos) else { break };
            let (a, b) = c[0].unwrap();
            out.push(regexp::match_object(it, &re, &chars, &c, &s));
            pos = if a == b { b + 1 } else { b };
        }
        Ok(super::make_list_iterator(it, out))
    });
    it.define_method(&sp, "search", 1, |it, this, args| {
        let s = this_str(it, &this)?;
        let pat = arg(args, 0);
        let re = match regex_of(&pat) {
            Some(r) => r,
            None => {
                let p = it.to_str(&pat)?;
                let rx = regexp::new_regexp(it, &p, "")?;
                regex_of(&rx).unwrap()
            }
        };
        let chars: Vec<char> = s.chars().collect();
        Ok(Value::Num(match re.exec_at(&chars, 0) {
            Some(c) => regexp::u16_of(&chars, c[0].unwrap().0) as f64,
            None => -1.0,
        }))
    });
    let si = it.sym_iterator.clone();
    it.define_method_sym(&sp, &si, "[Symbol.iterator]", |it, this, _| {
        let s = this_str(it, &this)?;
        let chars: Vec<String> = s.chars().map(|c| c.to_string()).collect();
        Ok(Value::Obj(Obj::new(ObjKind::Iterator(IterKind::Str { chars, index: 0 }), Some(it.p.iterator.clone()), "String Iterator")))
    });
    // HTML methods occasionally used by old scripts
    it.define_method(&sp, "link", 1, |it, this, args| {
        let s = this_str(it, &this)?;
        let h = it.to_str(&arg(args, 0))?;
        Ok(Value::str(&format!("<a href=\"{h}\">{s}</a>")))
    });
}
