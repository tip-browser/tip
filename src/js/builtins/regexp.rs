//! RegExp objects.

use std::rc::Rc;

use crate::js::interp::{arg, throw, Interp, R};
use crate::js::regex::{Caps, Regex};
use crate::js::value::*;

pub fn new_regexp(it: &mut Interp, pattern: &str, flags: &str) -> R<Value> {
    let re = match Regex::new(pattern, flags) {
        Ok(r) => r,
        Err(e) => throw!(it, "SyntaxError", "{e}: /{pattern}/"),
    };
    let o = Obj::new(ObjKind::RegExp(Rc::new(re)), Some(it.p.regexp.clone()), "RegExp");
    o.borrow_mut().props.insert(PropKey::from("lastIndex"), Prop { value: Value::Num(0.0), getter: None, setter: None, enumerable: false, writable: true, configurable: false });
    Ok(Value::Obj(o))
}

pub fn regex_of(v: &Value) -> Option<Rc<Regex>> {
    match v {
        Value::Obj(o) => match &o.borrow().kind {
            ObjKind::RegExp(r) => Some(r.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// UTF-16 offset of char index `ci`.
pub fn u16_of(chars: &[char], ci: usize) -> usize {
    chars[..ci.min(chars.len())].iter().map(|c| c.len_utf16()).sum()
}

/// Char index of UTF-16 offset `ui`.
pub fn char_of(chars: &[char], ui: usize) -> usize {
    let mut acc = 0;
    for (i, c) in chars.iter().enumerate() {
        if acc >= ui {
            return i;
        }
        acc += c.len_utf16();
    }
    chars.len()
}

fn sub(chars: &[char], a: usize, b: usize) -> String {
    chars[a.min(chars.len())..b.min(chars.len())].iter().collect()
}

fn get_last_index(it: &mut Interp, rx: &Value) -> R<usize> {
    let v = it.get_str(rx, "lastIndex")?;
    Ok(it.to_length(&v)?)
}

/// Run exec, honouring lastIndex for global/sticky regexps. Returns captures in char indices.
pub fn exec_raw(it: &mut Interp, rx: &Value, chars: &[char]) -> R<Option<Caps>> {
    let Some(re) = regex_of(rx) else { throw!(it, "TypeError", "not a RegExp") };
    let uses_last = re.global || re.sticky;
    let start = if uses_last { char_of(chars, get_last_index(it, rx)?) } else { 0 };
    if start > chars.len() {
        it.set_str(rx, "lastIndex", Value::Num(0.0))?;
        return Ok(None);
    }
    let m = re.exec_at(chars, start);
    if uses_last {
        match &m {
            Some(c) => {
                let end = c[0].unwrap().1;
                it.set_str(rx, "lastIndex", Value::Num(u16_of(chars, end) as f64))?;
            }
            None => it.set_str(rx, "lastIndex", Value::Num(0.0))?,
        }
    }
    Ok(m)
}

pub fn match_object(it: &mut Interp, re: &Regex, chars: &[char], caps: &Caps, input: &JsStr) -> Value {
    let items: Vec<Value> = caps.iter().map(|c| match c {
        Some((a, b)) => Value::str(&sub(chars, *a, *b)),
        None => Value::Undefined,
    }).collect();
    let arr = it.new_array(items);
    if let Value::Obj(o) = &arr {
        o.set_prop("index", Value::Num(u16_of(chars, caps[0].unwrap().0) as f64));
        o.set_prop("input", Value::Str(input.clone()));
        if re.names.is_empty() {
            o.set_prop("groups", Value::Undefined);
        } else {
            let g = it.new_object();
            for (n, i) in &re.names {
                let v = match caps.get(*i).copied().flatten() {
                    Some((a, b)) => Value::str(&sub(chars, a, b)),
                    None => Value::Undefined,
                };
                g.set_prop(n, v);
            }
            o.set_prop("groups", Value::Obj(g));
        }
    }
    arr
}

pub fn expand_replacement(tpl: &str, chars: &[char], caps: &Caps, names: &[(String, usize)]) -> String {
    let (ms, me) = caps[0].unwrap();
    let mut out = String::new();
    let t: Vec<char> = tpl.chars().collect();
    let mut i = 0;
    while i < t.len() {
        if t[i] == '$' && i + 1 < t.len() {
            match t[i + 1] {
                '$' => {
                    out.push('$');
                    i += 2;
                    continue;
                }
                '&' => {
                    out.push_str(&sub(chars, ms, me));
                    i += 2;
                    continue;
                }
                '`' => {
                    out.push_str(&sub(chars, 0, ms));
                    i += 2;
                    continue;
                }
                '\'' => {
                    out.push_str(&sub(chars, me, chars.len()));
                    i += 2;
                    continue;
                }
                '<' => {
                    if let Some(end) = t[i + 2..].iter().position(|c| *c == '>') {
                        let name: String = t[i + 2..i + 2 + end].iter().collect();
                        if let Some((_, gi)) = names.iter().find(|(n, _)| *n == name) {
                            if let Some(Some((a, b))) = caps.get(*gi) {
                                out.push_str(&sub(chars, *a, *b));
                            }
                        }
                        i += 3 + end;
                        continue;
                    }
                }
                d if d.is_ascii_digit() => {
                    let d1 = d.to_digit(10).unwrap() as usize;
                    let two = t.get(i + 2).and_then(|c| c.to_digit(10)).map(|d2| d1 * 10 + d2 as usize);
                    let (n, len) = match two {
                        Some(n) if n >= 1 && n < caps.len() => (n, 3),
                        _ => (d1, 2),
                    };
                    if n >= 1 && n < caps.len() {
                        if let Some((a, b)) = caps[n] {
                            out.push_str(&sub(chars, a, b));
                        }
                        i += len;
                        continue;
                    }
                }
                _ => {}
            }
        }
        out.push(t[i]);
        i += 1;
    }
    out
}

/// String.prototype.replace / replaceAll with a RegExp pattern.
pub fn regex_replace(it: &mut Interp, rx: &Value, s: &JsStr, replacement: &Value, force_global: bool) -> R<Value> {
    let Some(re) = regex_of(rx) else { throw!(it, "TypeError", "not a RegExp") };
    let chars: Vec<char> = s.chars().collect();
    let global = re.global || force_global;
    let mut matches: Vec<Caps> = Vec::new();
    if global {
        it.set_str(rx, "lastIndex", Value::Num(0.0))?;
        let mut pos = 0;
        while pos <= chars.len() {
            let Some(c) = re.exec_at(&chars, pos) else { break };
            let (a, b) = c[0].unwrap();
            pos = if b == a { b + 1 } else { b };
            matches.push(c);
            if matches.len() > 10_000_000 {
                break;
            }
        }
    } else if let Some(c) = exec_raw(it, rx, &chars)? {
        matches.push(c);
    }
    let mut out = String::new();
    let mut last = 0;
    for caps in &matches {
        let (a, b) = caps[0].unwrap();
        out.push_str(&sub(&chars, last, a));
        if replacement.is_callable() {
            let mut args: Vec<Value> = caps.iter().map(|c| match c {
                Some((x, y)) => Value::str(&sub(&chars, *x, *y)),
                None => Value::Undefined,
            }).collect();
            args.push(Value::Num(u16_of(&chars, a) as f64));
            args.push(Value::Str(s.clone()));
            if !re.names.is_empty() {
                let g = it.new_object();
                for (n, i) in &re.names {
                    let v = match caps.get(*i).copied().flatten() {
                        Some((x, y)) => Value::str(&sub(&chars, x, y)),
                        None => Value::Undefined,
                    };
                    g.set_prop(n, v);
                }
                args.push(Value::Obj(g));
            }
            let r = it.call(replacement, Value::Undefined, &args)?;
            out.push_str(&it.to_str(&r)?);
        } else {
            let tpl = it.to_str(replacement)?;
            out.push_str(&expand_replacement(&tpl, &chars, caps, &re.names));
        }
        last = b;
    }
    out.push_str(&sub(&chars, last, chars.len()));
    Ok(Value::str(&out))
}

pub fn regex_split(it: &mut Interp, rx: &Value, s: &str, limit: usize) -> R<Value> {
    let Some(re) = regex_of(rx) else { throw!(it, "TypeError", "not a RegExp") };
    let chars: Vec<char> = s.chars().collect();
    let mut out: Vec<Value> = Vec::new();
    if limit == 0 {
        return Ok(it.new_array(out));
    }
    if chars.is_empty() {
        if re.exec_at(&chars, 0).is_none() {
            out.push(Value::str(""));
        }
        return Ok(it.new_array(out));
    }
    // ECMAScript split: p = end of last split, q = search position
    let mut p = 0;
    let mut q = 0;
    while q < chars.len() {
        let Some(caps) = re.exec_at(&chars, q) else { break };
        let (a, e) = caps[0].unwrap();
        if a >= chars.len() {
            break;
        }
        if e == p || (e == a && a == q && a == p) {
            q = a + 1;
            continue;
        }
        out.push(Value::str(&sub(&chars, p, a)));
        if out.len() >= limit {
            return Ok(it.new_array(out));
        }
        for c in caps.iter().skip(1) {
            out.push(match c {
                Some((x, y)) => Value::str(&sub(&chars, *x, *y)),
                None => Value::Undefined,
            });
            if out.len() >= limit {
                return Ok(it.new_array(out));
            }
        }
        p = e;
        q = if e == a { e + 1 } else { e };
    }
    out.push(Value::str(&sub(&chars, p, chars.len())));
    Ok(it.new_array(out))
}

pub fn setup(it: &mut Interp) {
    let rp = it.p.regexp.clone();
    it.define_ctor("RegExp", 2, &rp, |it, _this, args| {
        let p = arg(args, 0);
        let f = arg(args, 1);
        let (pattern, mut flags) = match regex_of(&p) {
            Some(r) => (r.source.clone(), r.flags.clone()),
            None => (if p.is_undefined() { "(?:)".to_string() } else { it.to_str(&p)?.to_string() }, String::new()),
        };
        if !f.is_undefined() {
            flags = it.to_str(&f)?.to_string();
        }
        it.constructing = false;
        new_regexp(it, &pattern, &flags)
    });
    it.define_method(&rp, "exec", 1, |it, this, args| {
        let s = it.to_str(&arg(args, 0))?;
        let chars: Vec<char> = s.chars().collect();
        let Some(re) = regex_of(&this) else { throw!(it, "TypeError", "RegExp.prototype.exec called on incompatible receiver") };
        match exec_raw(it, &this, &chars)? {
            Some(caps) => Ok(match_object(it, &re, &chars, &caps, &s)),
            None => Ok(Value::Null),
        }
    });
    it.define_method(&rp, "test", 1, |it, this, args| {
        let s = it.to_str(&arg(args, 0))?;
        let chars: Vec<char> = s.chars().collect();
        Ok(Value::Bool(exec_raw(it, &this, &chars)?.is_some()))
    });
    it.define_method(&rp, "toString", 0, |it, this, _| {
        let src = it.get_str(&this, "source")?;
        let flags = it.get_str(&this, "flags")?;
        Ok(Value::str(&format!("/{}/{}", it.to_str(&src)?, it.to_str(&flags)?)))
    });
    it.define_method(&rp, "compile", 2, |_, this, _| Ok(this));
    it.define_getter(&rp, "source", |_, this, _| Ok(regex_of(&this).map(|r| Value::str(&r.source)).unwrap_or(Value::str("(?:)"))));
    it.define_getter(&rp, "flags", |_, this, _| Ok(regex_of(&this).map(|r| {
        let mut f: Vec<char> = r.flags.chars().collect();
        f.sort_by_key(|c| "dgimsuvy".find(*c).unwrap_or(9));
        Value::str(&f.into_iter().collect::<String>())
    }).unwrap_or(Value::str(""))));
    for (name, flag) in [("global", 'g'), ("ignoreCase", 'i'), ("multiline", 'm'), ("sticky", 'y'), ("unicode", 'u'), ("dotAll", 's'), ("hasIndices", 'd')] {
        it.define_getter(&rp, name, move |_, this, _| Ok(regex_of(&this).map(|r| Value::Bool(r.flags.contains(flag))).unwrap_or(Value::Undefined)));
    }
}
