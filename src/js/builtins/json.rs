//! JSON.parse / JSON.stringify.

use std::rc::Rc;

use crate::js::interp::{arg, throw, Interp, R};
use crate::js::value::*;

struct P<'a> {
    s: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }
    fn err(&self) -> String {
        if self.i >= self.s.len() {
            "Unexpected end of JSON input".into()
        } else {
            format!("Unexpected token '{}' in JSON at position {}", self.s[self.i] as char, self.i)
        }
    }
    fn value(&mut self, it: &mut Interp) -> Result<Value, String> {
        self.ws();
        let c = *self.s.get(self.i).ok_or_else(|| self.err())?;
        match c {
            b'{' => {
                self.i += 1;
                let o = it.new_object();
                self.ws();
                if self.s.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Value::Obj(o));
                }
                loop {
                    self.ws();
                    if self.s.get(self.i) != Some(&b'"') {
                        return Err(self.err());
                    }
                    let k = self.string()?;
                    self.ws();
                    if self.s.get(self.i) != Some(&b':') {
                        return Err(self.err());
                    }
                    self.i += 1;
                    let v = self.value(it)?;
                    if k == "__proto__" {
                        o.borrow_mut().props.insert(PropKey::from("__proto__"), Prop::data(v));
                    } else {
                        o.borrow_mut().props.insert(PropKey::Str(Rc::from(k.as_str())), Prop::data(v));
                    }
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Value::Obj(o));
                        }
                        _ => return Err(self.err()),
                    }
                }
            }
            b'[' => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.s.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(it.new_array(items));
                }
                loop {
                    items.push(self.value(it)?);
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(it.new_array(items));
                        }
                        _ => return Err(self.err()),
                    }
                }
            }
            b'"' => Ok(Value::str(&self.string()?)),
            b't' if self.s[self.i..].starts_with(b"true") => {
                self.i += 4;
                Ok(Value::Bool(true))
            }
            b'f' if self.s[self.i..].starts_with(b"false") => {
                self.i += 5;
                Ok(Value::Bool(false))
            }
            b'n' if self.s[self.i..].starts_with(b"null") => {
                self.i += 4;
                Ok(Value::Null)
            }
            b'-' | b'0'..=b'9' => {
                let st = self.i;
                if self.s[self.i] == b'-' {
                    self.i += 1;
                }
                let ds = self.i;
                while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                    self.i += 1;
                }
                if self.i == ds {
                    return Err(self.err());
                }
                if self.i < self.s.len() && self.s[self.i] == b'.' {
                    self.i += 1;
                    let fs = self.i;
                    while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                        self.i += 1;
                    }
                    if self.i == fs {
                        return Err(self.err());
                    }
                }
                if self.i < self.s.len() && (self.s[self.i] == b'e' || self.s[self.i] == b'E') {
                    self.i += 1;
                    if self.i < self.s.len() && (self.s[self.i] == b'+' || self.s[self.i] == b'-') {
                        self.i += 1;
                    }
                    while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                        self.i += 1;
                    }
                }
                let t = std::str::from_utf8(&self.s[st..self.i]).unwrap();
                Ok(Value::Num(t.parse().map_err(|_| self.err())?))
            }
            _ => Err(self.err()),
        }
    }
    fn string(&mut self) -> Result<String, String> {
        self.i += 1;
        let mut out: Vec<u16> = Vec::new();
        let mut run_start = self.i;
        let mut buf = String::new();
        loop {
            let c = *self.s.get(self.i).ok_or("Unterminated string in JSON")?;
            match c {
                b'"' => {
                    buf.push_str(std::str::from_utf8(&self.s[run_start..self.i]).map_err(|_| "invalid UTF-8")?);
                    self.i += 1;
                    if !out.is_empty() {
                        let mut u: Vec<u16> = buf.encode_utf16().collect();
                        out.append(&mut u);
                        return Ok(String::from_utf16_lossy(&out));
                    }
                    return Ok(buf);
                }
                b'\\' => {
                    buf.push_str(std::str::from_utf8(&self.s[run_start..self.i]).map_err(|_| "invalid UTF-8")?);
                    self.i += 1;
                    let e = *self.s.get(self.i).ok_or("bad escape")?;
                    self.i += 1;
                    match e {
                        b'n' => buf.push('\n'),
                        b't' => buf.push('\t'),
                        b'r' => buf.push('\r'),
                        b'b' => buf.push('\u{8}'),
                        b'f' => buf.push('\u{c}'),
                        b'/' => buf.push('/'),
                        b'\\' => buf.push('\\'),
                        b'"' => buf.push('"'),
                        b'u' => {
                            let h = std::str::from_utf8(self.s.get(self.i..self.i + 4).ok_or("bad \\u escape")?).map_err(|_| "bad escape")?;
                            let v = u16::from_str_radix(h, 16).map_err(|_| "Bad Unicode escape in JSON")?;
                            self.i += 4;
                            // flush buf to utf16 accumulation to handle surrogates
                            let mut u: Vec<u16> = buf.encode_utf16().collect();
                            out.append(&mut u);
                            buf.clear();
                            out.push(v);
                        }
                        _ => return Err(self.err()),
                    }
                    run_start = self.i;
                }
                0..=0x1f => return Err(format!("Bad control character in string literal in JSON at position {}", self.i)),
                _ => self.i += 1,
            }
        }
    }
}

pub fn parse(it: &mut Interp, s: &str) -> R<Value> {
    let mut p = P { s: s.as_bytes(), i: 0 };
    match p.value(it) {
        Ok(v) => {
            p.ws();
            if p.i < p.s.len() {
                let e = p.err();
                throw!(it, "SyntaxError", "{e}");
            }
            Ok(v)
        }
        Err(e) => throw!(it, "SyntaxError", "{e}"),
    }
}

pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

struct S<'a> {
    replacer: &'a Value,
    allow: Option<Vec<PropKey>>,
    gap: String,
    stack: Vec<usize>,
}

fn ser(it: &mut Interp, st: &mut S, holder: &Value, key: &PropKey, value: Value, indent: &str) -> R<Option<String>> {
    let mut v = value;
    if let Value::Obj(_) | Value::BigInt(_) = &v {
        let tj = it.get_str(&v, "toJSON")?;
        if tj.is_callable() {
            v = it.call(&tj, v.clone(), &[key.to_value()])?;
        }
    }
    if st.replacer.is_callable() {
        v = it.call(st.replacer, holder.clone(), &[key.to_value(), v])?;
    }
    if let Value::Obj(o) = &v {
        let prim = match &o.borrow().kind {
            ObjKind::Num(n) => Some(Value::Num(*n)),
            ObjKind::Str(s) => Some(Value::Str(s.clone())),
            ObjKind::Bool(b) => Some(Value::Bool(*b)),
            _ => None,
        };
        if let Some(p) = prim {
            v = p;
        }
    }
    Ok(Some(match &v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Num(n) => {
            if n.is_finite() {
                num_to_string(*n)
            } else {
                "null".into()
            }
        }
        Value::BigInt(_) => throw!(it, "TypeError", "Do not know how to serialize a BigInt"),
        Value::Str(s) => quote(s),
        Value::Undefined | Value::Sym(_) => return Ok(None),
        Value::Obj(o) => {
            if o.is_callable() {
                return Ok(None);
            }
            if st.stack.contains(&o.id()) {
                throw!(it, "TypeError", "Converting circular structure to JSON");
            }
            if st.stack.len() > 1000 {
                throw!(it, "RangeError", "Maximum call stack size exceeded");
            }
            st.stack.push(o.id());
            let inner = format!("{indent}{}", st.gap);
            let r = if o.is_array() {
                let items = super::array_like_to_vec(it, &v)?;
                let mut parts = Vec::new();
                for (i, x) in items.into_iter().enumerate() {
                    let k = PropKey::Str(Rc::from(i.to_string().as_str()));
                    parts.push(ser(it, st, &v, &k, x, &inner)?.unwrap_or_else(|| "null".into()));
                }
                if parts.is_empty() {
                    "[]".into()
                } else if st.gap.is_empty() {
                    format!("[{}]", parts.join(","))
                } else {
                    format!("[\n{inner}{}\n{indent}]", parts.join(&format!(",\n{inner}")))
                }
            } else {
                let keys = match &st.allow {
                    Some(a) => a.clone(),
                    None => it.own_keys(o, false),
                };
                let mut parts = Vec::new();
                for k in keys {
                    let x = it.get(&v, &k)?;
                    if let Some(s) = ser(it, st, &v, &k, x, &inner)? {
                        let ks = quote(k.as_str().unwrap_or(""));
                        parts.push(if st.gap.is_empty() { format!("{ks}:{s}") } else { format!("{ks}: {s}") });
                    }
                }
                if parts.is_empty() {
                    "{}".into()
                } else if st.gap.is_empty() {
                    format!("{{{}}}", parts.join(","))
                } else {
                    format!("{{\n{inner}{}\n{indent}}}", parts.join(&format!(",\n{inner}")))
                }
            };
            st.stack.pop();
            r
        }
    }))
}

pub fn stringify(it: &mut Interp, v: &Value, replacer: &Value, gap: &str) -> R<Option<String>> {
    let allow = if let Value::Obj(o) = replacer {
        if o.is_array() {
            let items = super::array_like_to_vec(it, replacer)?;
            let mut keys = Vec::new();
            for i in items {
                keys.push(it.to_key(&i)?);
            }
            Some(keys)
        } else {
            None
        }
    } else {
        None
    };
    let mut st = S { replacer, allow, gap: gap.to_string(), stack: vec![] };
    let holder = Value::Obj(it.new_object());
    ser(it, &mut st, &holder, &PropKey::from(""), v.clone(), "")
}

fn revive(it: &mut Interp, holder: &Value, key: PropKey, reviver: &Value) -> R<Value> {
    let val = it.get(holder, &key)?;
    if let Value::Obj(o) = &val {
        for k in it.own_keys(o, false) {
            let nv = revive(it, &val, k.clone(), reviver)?;
            if nv.is_undefined() {
                it.delete(&val, &k)?;
            } else {
                it.set(&val, k, nv)?;
            }
        }
    }
    it.call(reviver, holder.clone(), &[key.to_value(), val])
}

pub fn setup(it: &mut Interp) {
    let j = it.new_object();
    it.global.set_hidden("JSON", Value::Obj(j.clone()));
    let tag = it.sym_to_string_tag.clone();
    j.borrow_mut().props.insert(PropKey::Sym(tag), Prop::hidden(Value::str("JSON")));
    it.define_method(&j, "parse", 2, |it, _, args| {
        let s = it.to_str(&arg(args, 0))?;
        let v = parse(it, &s)?;
        let reviver = arg(args, 1);
        if reviver.is_callable() {
            let root = it.new_object();
            root.set_prop("", v);
            return revive(it, &Value::Obj(root), PropKey::from(""), &reviver);
        }
        Ok(v)
    });
    it.define_method(&j, "stringify", 3, |it, _, args| {
        let space = arg(args, 2);
        let gap = match &space {
            Value::Num(n) => " ".repeat((*n).clamp(0.0, 10.0) as usize),
            Value::Str(s) => s.chars().take(10).collect(),
            _ => String::new(),
        };
        Ok(match stringify(it, &arg(args, 0), &arg(args, 1), &gap)? {
            Some(s) => Value::str(&s),
            None => Value::Undefined,
        })
    });
}
