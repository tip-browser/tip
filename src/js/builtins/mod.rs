//! Standard library.

pub mod array;
pub mod collections;
pub mod date;
pub mod json;
pub mod number;
pub mod object;
pub mod promise;
pub mod regexp;
pub mod string;
pub mod typed;

use std::rc::Rc;

use super::interp::{arg, throw, ConsoleMsg, Interp, R};
use super::value::*;

pub fn host_inspect(it: &mut Interp, h: &Rc<dyn HostObject>) -> Option<String> {
    h.inspect(it)
}

pub fn setup(it: &mut Interp) {
    let g = it.global.clone();
    g.set_hidden("globalThis", Value::Obj(g.clone()));
    object::setup(it);
    function_setup(it);
    symbol_setup(it);
    error_setup(it);
    array::setup(it);
    string::setup(it);
    number::setup(it);
    json::setup(it);
    promise::setup(it);
    collections::setup(it);
    regexp::setup(it);
    date::setup(it);
    typed::setup(it);
    iterator_setup(it);
    globals_setup(it);
    console_setup(it);
}

// ---------------------------------------------------------------- Function

fn function_setup(it: &mut Interp) {
    let fp = it.p.function.clone();
    let ctor = it.define_ctor("Function", 1, &fp, |it, _this, args| {
        let mut params: Vec<String> = Vec::new();
        for a in &args[..args.len().saturating_sub(1)] {
            params.push(it.to_str(a)?.to_string());
        }
        let body = match args.last() {
            Some(b) => it.to_str(b)?.to_string(),
            None => String::new(),
        };
        let def = super::parser::parse_function_body(&params, &body).map_err(|e| it.make_error("SyntaxError", e.trim_start_matches("SyntaxError: ")))?;
        let env = it.global_env.clone();
        Ok(it.make_function(def, &env, None))
    });
    let _ = ctor;
    fp.set_hidden("name", Value::str(""));
    it.define_method(&fp, "call", 1, |it, this, args| {
        let t = arg(args, 0);
        let rest = if args.len() > 1 { &args[1..] } else { &[] };
        it.call(&this, t, rest)
    });
    it.define_method(&fp, "apply", 2, |it, this, args| {
        let t = arg(args, 0);
        let list = arg(args, 1);
        let argv = if list.is_nullish() { vec![] } else { array_like_to_vec(it, &list)? };
        it.call(&this, t, &argv)
    });
    it.define_method(&fp, "bind", 1, |it, this, args| {
        let Value::Obj(target) = &this else { throw!(it, "TypeError", "Bind must be called on a function") };
        if !target.is_callable() {
            throw!(it, "TypeError", "Bind must be called on a function");
        }
        let bthis = arg(args, 0);
        let bargs = if args.len() > 1 { args[1..].to_vec() } else { vec![] };
        let name = target.get_own("name").map(|n| it.to_string_lossy(&n)).unwrap_or_default();
        let len = match target.get_own("length") {
            Some(Value::Num(n)) => (n - bargs.len() as f64).max(0.0),
            _ => 0.0,
        };
        let f = Obj::new(ObjKind::Function(Func::Bound { target: target.clone(), this: bthis, args: bargs }), Some(it.p.function.clone()), "Function");
        f.set_hidden("name", Value::str(&format!("bound {name}")));
        f.set_hidden("length", Value::Num(len));
        Ok(Value::Obj(f))
    });
    it.define_method(&fp, "toString", 0, |it, this, _| {
        if let Value::Obj(o) = &this {
            if let ObjKind::Function(Func::Closure(c)) = &o.borrow().kind {
                return Ok(Value::Str(c.def.source.clone()));
            }
            let n = o.get_own("name").map(|n| it.to_string_lossy(&n)).unwrap_or_default();
            return Ok(Value::str(&format!("function {n}() {{ [native code] }}")));
        }
        throw!(it, "TypeError", "Function.prototype.toString requires that 'this' be a Function")
    });
    let hi = it.sym_has_instance.clone();
    it.define_method_sym(&fp, &hi, "[Symbol.hasInstance]", |it, this, args| {
        let v = arg(args, 0);
        Ok(Value::Bool(it.instance_of(&v, &this)?))
    });
}

pub fn array_like_to_vec(it: &mut Interp, v: &Value) -> R<Vec<Value>> {
    if let Value::Obj(o) = v {
        if let ObjKind::Array(items) = &o.borrow().kind {
            return Ok(items.clone());
        }
    }
    let len_v = it.get_str(v, "length")?;
    let len = it.to_length(&len_v)?.min(10_000_000);
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        out.push(it.get(v, &PropKey::Str(Rc::from(i.to_string().as_str())))?);
    }
    Ok(out)
}

// ---------------------------------------------------------------- Symbol

fn symbol_setup(it: &mut Interp) {
    let sp = it.p.symbol.clone();
    let ctor = it.define_ctor("Symbol", 0, &sp, |it, _this, args| {
        if it.constructing {
            it.constructing = false;
            throw!(it, "TypeError", "Symbol is not a constructor");
        }
        let d = arg(args, 0);
        let description = if d.is_undefined() { None } else { Some(it.to_str(&d)?) };
        Ok(Value::Sym(Sym(Rc::new(SymbolData { description }))))
    });
    for (name, s) in [
        ("iterator", it.sym_iterator.clone()),
        ("asyncIterator", it.sym_async_iterator.clone()),
        ("hasInstance", it.sym_has_instance.clone()),
        ("toPrimitive", it.sym_to_primitive.clone()),
        ("toStringTag", it.sym_to_string_tag.clone()),
    ] {
        ctor.set_hidden(name, Value::Sym(s));
    }
    for name in ["match", "replace", "search", "split", "species", "unscopables", "isConcatSpreadable", "matchAll"] {
        ctor.set_hidden(name, Value::Sym(Sym(Rc::new(SymbolData { description: Some(Rc::from(format!("Symbol.{name}").as_str())) }))));
    }
    it.define_method(&ctor, "for", 1, |it, _, args| {
        let k = it.to_str(&arg(args, 0))?;
        if let Some(s) = it.symbol_registry.get(&k) {
            return Ok(Value::Sym(s.clone()));
        }
        let s = Sym(Rc::new(SymbolData { description: Some(k.clone()) }));
        it.symbol_registry.insert(k, s.clone());
        Ok(Value::Sym(s))
    });
    it.define_method(&ctor, "keyFor", 1, |it, _, args| {
        if let Value::Sym(s) = arg(args, 0) {
            for (k, v) in &it.symbol_registry {
                if *v == s {
                    return Ok(Value::Str(k.clone()));
                }
            }
        }
        Ok(Value::Undefined)
    });
    it.define_method(&sp, "toString", 0, |it, this, _| {
        let s = this_symbol(it, &this)?;
        Ok(Value::str(&format!("Symbol({})", s.0.description.as_deref().unwrap_or(""))))
    });
    it.define_method(&sp, "valueOf", 0, |it, this, _| Ok(Value::Sym(this_symbol(it, &this)?)));
    it.define_getter(&sp, "description", |it, this, _| {
        let s = this_symbol(it, &this)?;
        Ok(s.0.description.clone().map(Value::Str).unwrap_or(Value::Undefined))
    });
}

fn this_symbol(it: &mut Interp, v: &Value) -> R<Sym> {
    match v {
        Value::Sym(s) => Ok(s.clone()),
        Value::Obj(o) => match &o.borrow().kind {
            ObjKind::Symbol(s) => Ok(s.clone()),
            _ => Err(it.make_error("TypeError", "not a symbol")),
        },
        _ => Err(it.make_error("TypeError", "not a symbol")),
    }
}

// ---------------------------------------------------------------- Errors

fn error_setup(it: &mut Interp) {
    let ep = it.p.error.clone();
    ep.set_hidden("name", Value::str("Error"));
    ep.set_hidden("message", Value::str(""));
    let make = |kind: &'static str| {
        move |it: &mut Interp, this: Value, args: &[Value]| -> R<Value> {
            let o = match &this {
                Value::Obj(o) if it.constructing => o.clone(),
                _ => {
                    let proto = it.p.errors.get(kind).cloned().unwrap_or_else(|| it.p.error.clone());
                    Obj::new(ObjKind::Ordinary, Some(proto), "Error")
                }
            };
            it.constructing = false;
            o.borrow_mut().kind = ObjKind::Error;
            o.borrow_mut().class = "Error";
            let (msg_idx, opts_idx) = if kind == "AggregateError" { (1, 2) } else { (0, 1) };
            let m = arg(args, msg_idx);
            if !m.is_undefined() {
                let s = it.to_str(&m)?;
                o.set_hidden("message", Value::Str(s));
            }
            if kind == "AggregateError" {
                let errs = it.iterate_to_vec(&arg(args, 0))?;
                let arr = it.new_array(errs);
                o.set_hidden("errors", arr);
            }
            let opts = arg(args, opts_idx);
            if let Value::Obj(_) = &opts {
                let cause = it.get_str(&opts, "cause")?;
                if !cause.is_undefined() {
                    o.set_hidden("cause", cause);
                }
            }
            let ov = Value::Obj(o.clone());
            let name = it.get_str(&ov, "name")?;
            let name = it.to_string_lossy(&name);
            let msg = it.get_str(&ov, "message")?;
            let msg = it.to_string_lossy(&msg);
            let head = if msg.is_empty() { name } else { format!("{name}: {msg}") };
            o.set_hidden("stack", Value::str(&format!("{head}\n    at {}:{}", it.script_name, it.cur_line + 1)));
            Ok(ov)
        }
    };
    let ector = it.define_ctor("Error", 1, &ep, make("Error"));
    it.define_method(&ector, "captureStackTrace", 1, |_, _, _| Ok(Value::Undefined));
    it.define_method(&ep, "toString", 0, |it, this, _| {
        let name = it.get_str(&this, "name")?;
        let name = if name.is_undefined() { "Error".to_string() } else { it.to_str(&name)?.to_string() };
        let msg = it.get_str(&this, "message")?;
        let msg = if msg.is_undefined() { String::new() } else { it.to_str(&msg)?.to_string() };
        Ok(Value::str(&if msg.is_empty() { name } else if name.is_empty() { msg } else { format!("{name}: {msg}") }))
    });
    for kind in ["TypeError", "RangeError", "SyntaxError", "ReferenceError", "EvalError", "URIError", "AggregateError"] {
        let p = it.p.errors.get(kind).unwrap().clone();
        p.set_hidden("name", Value::str(kind));
        p.set_hidden("message", Value::str(""));
        let c = it.define_ctor(kind, 1, &p, make(kind));
        c.borrow_mut().proto = Some(ector.clone());
    }
}

// ---------------------------------------------------------------- iterators

fn iterator_setup(it: &mut Interp) {
    let ip = it.p.iterator.clone();
    let si = it.sym_iterator.clone();
    it.define_method_sym(&ip, &si, "[Symbol.iterator]", |_, this, _| Ok(this));
    it.define_method(&ip, "next", 0, |it, this, _| {
        let Value::Obj(o) = &this else { throw!(it, "TypeError", "not an iterator") };
        let step: Option<Value> = {
            let mut b = o.borrow_mut();
            match &mut b.kind {
                ObjKind::Iterator(IterKind::List { items, index }) => {
                    let v = items.get(*index).cloned();
                    *index += 1;
                    v
                }
                ObjKind::Iterator(IterKind::Str { chars, index }) => {
                    let v = chars.get(*index).map(|c| Value::str(c));
                    *index += 1;
                    v
                }
                ObjKind::Iterator(IterKind::Array { .. }) | ObjKind::Iterator(IterKind::Map { .. }) => None,
                _ => {
                    drop(b);
                    throw!(it, "TypeError", "not an iterator")
                }
            }
        };
        if let Some(v) = step {
            return Ok(it.iter_result(v, false));
        }
        // live array / map iterators
        let (target, index, kind, is_map) = {
            let b = o.borrow();
            match &b.kind {
                ObjKind::Iterator(IterKind::Array { target, index, kind }) => (Some(target.clone()), *index, *kind, false),
                ObjKind::Iterator(IterKind::Map { map, index, kind }) => (Some(Value::Obj(map.clone())), *index, *kind, true),
                _ => (None, 0, 0, false),
            }
        };
        let Some(target) = target else { return Ok(it.iter_result(Value::Undefined, true)) };
        if is_map {
            let Value::Obj(m) = &target else { unreachable!() };
            let (entry, next_index) = {
                let b = m.borrow();
                let entries = match &b.kind {
                    ObjKind::Map(d) | ObjKind::Set(d) => &d.entries,
                    _ => return Ok(it.iter_result(Value::Undefined, true)),
                };
                let mut i = index;
                while i < entries.len() && entries[i].is_none() {
                    i += 1;
                }
                (entries.get(i).cloned().flatten(), i + 1)
            };
            if let ObjKind::Iterator(IterKind::Map { index, .. }) = &mut o.borrow_mut().kind {
                *index = next_index;
            }
            return Ok(match entry {
                None => it.iter_result(Value::Undefined, true),
                Some((k, v)) => {
                    let val = match kind {
                        0 => k,
                        1 => v,
                        _ => it.new_array(vec![k, v]),
                    };
                    it.iter_result(val, false)
                }
            });
        }
        let len_v = it.get_str(&target, "length")?;
        let len = it.to_length(&len_v)?;
        if index >= len {
            if let ObjKind::Iterator(IterKind::Array { index: ix, .. }) = &mut o.borrow_mut().kind {
                *ix = usize::MAX / 2;
            }
            return Ok(it.iter_result(Value::Undefined, true));
        }
        if let ObjKind::Iterator(IterKind::Array { index: ix, .. }) = &mut o.borrow_mut().kind {
            *ix = index + 1;
        }
        let val = match kind {
            0 => Value::Num(index as f64),
            1 => it.get(&target, &PropKey::Str(Rc::from(index.to_string().as_str())))?,
            _ => {
                let v = it.get(&target, &PropKey::Str(Rc::from(index.to_string().as_str())))?;
                it.new_array(vec![Value::Num(index as f64), v])
            }
        };
        Ok(it.iter_result(val, false))
    });
    // iterator helpers (subset)
    it.define_method(&ip, "toArray", 0, |it, this, _| {
        let v = it.iterate_to_vec(&this)?;
        Ok(it.new_array(v))
    });

    let gp = it.p.generator.clone();
    it.define_method(&gp, "next", 1, |it, this, args| it.generator_resume(&this, super::coro::Resume::Next(arg(args, 0))));
    it.define_method(&gp, "return", 1, |it, this, args| it.generator_resume(&this, super::coro::Resume::Return(arg(args, 0))));
    it.define_method(&gp, "throw", 1, |it, this, args| it.generator_resume(&this, super::coro::Resume::Throw(arg(args, 0))));
    let tag = it.sym_to_string_tag.clone();
    gp.borrow_mut().props.insert(PropKey::Sym(tag), Prop::hidden(Value::str("Generator")));
}

pub fn make_list_iterator(it: &mut Interp, items: Vec<Value>) -> Value {
    Value::Obj(Obj::new(ObjKind::Iterator(IterKind::List { items, index: 0 }), Some(it.p.iterator.clone()), "Iterator"))
}

// ---------------------------------------------------------------- globals

fn uri_encode(s: &str, keep: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || keep.contains(c) {
            out.push(c);
        } else {
            let mut buf = [0u8; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

fn uri_decode(s: &str, reserved: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let h = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            let b = u8::from_str_radix(h, 16).ok()?;
            if b < 0x80 && reserved.contains(b as char) {
                out.extend_from_slice(&bytes[i..i + 3]);
            } else {
                out.push(b);
            }
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn globals_setup(it: &mut Interp) {
    let g = it.global.clone();
    g.borrow_mut().props.insert(PropKey::from("NaN"), Prop { value: Value::Num(f64::NAN), getter: None, setter: None, enumerable: false, writable: false, configurable: false });
    g.borrow_mut().props.insert(PropKey::from("Infinity"), Prop { value: Value::Num(f64::INFINITY), getter: None, setter: None, enumerable: false, writable: false, configurable: false });
    g.borrow_mut().props.insert(PropKey::from("undefined"), Prop { value: Value::Undefined, getter: None, setter: None, enumerable: false, writable: false, configurable: false });
    it.define_method(&g, "parseInt", 2, |it, _, args| {
        let s = it.to_str(&arg(args, 0))?;
        let radix = it.to_integer(&arg(args, 1))? as u32;
        Ok(Value::Num(number::parse_int(&s, radix)))
    });
    it.define_method(&g, "parseFloat", 1, |it, _, args| {
        let s = it.to_str(&arg(args, 0))?;
        Ok(Value::Num(number::parse_float(&s)))
    });
    it.define_method(&g, "isNaN", 1, |it, _, args| Ok(Value::Bool(it.to_number(&arg(args, 0))?.is_nan())));
    it.define_method(&g, "isFinite", 1, |it, _, args| Ok(Value::Bool(it.to_number(&arg(args, 0))?.is_finite())));
    it.define_method(&g, "encodeURIComponent", 1, |it, _, args| {
        let s = it.to_str(&arg(args, 0))?;
        Ok(Value::str(&uri_encode(&s, "-_.!~*'()")))
    });
    it.define_method(&g, "encodeURI", 1, |it, _, args| {
        let s = it.to_str(&arg(args, 0))?;
        Ok(Value::str(&uri_encode(&s, "-_.!~*'();/?:@&=+$,#")))
    });
    it.define_method(&g, "decodeURIComponent", 1, |it, _, args| {
        let s = it.to_str(&arg(args, 0))?;
        match uri_decode(&s, "") {
            Some(d) => Ok(Value::str(&d)),
            None => throw!(it, "URIError", "URI malformed"),
        }
    });
    it.define_method(&g, "decodeURI", 1, |it, _, args| {
        let s = it.to_str(&arg(args, 0))?;
        match uri_decode(&s, ";/?:@&=+$,#") {
            Some(d) => Ok(Value::str(&d)),
            None => throw!(it, "URIError", "URI malformed"),
        }
    });
    it.define_method(&g, "escape", 1, |it, _, args| {
        let s = it.to_str(&arg(args, 0))?;
        Ok(Value::str(&uri_encode(&s, "@*_+-./")))
    });
    it.define_method(&g, "unescape", 1, |it, _, args| {
        let s = it.to_str(&arg(args, 0))?;
        Ok(Value::str(&uri_decode(&s, "").unwrap_or_else(|| s.to_string())))
    });
    it.define_method(&g, "queueMicrotask", 1, |it, _, args| {
        let f = arg(args, 0);
        if !f.is_callable() {
            throw!(it, "TypeError", "queueMicrotask requires a function");
        }
        it.enqueue(super::interp::Job::Call(f, vec![]));
        Ok(Value::Undefined)
    });
    it.define_method(&g, "structuredClone", 1, |it, _, args| {
        let v = arg(args, 0);
        let s = json::stringify(it, &v, &Value::Undefined, "")?;
        match s {
            Some(s) => json::parse(it, &s),
            None => Ok(Value::Undefined),
        }
    });
    it.define_method(&g, "__tip_import", 1, |it, _, args| super::module::global_import(it, arg(args, 0)));
    it.define_method(&g, "eval", 1, |it, _, args| {
        let src = arg(args, 0);
        let Value::Str(s) = src else { return Ok(src) };
        it.eval_script(&s, "eval")
    });
    let reflect = it.new_object();
    g.set_hidden("Reflect", Value::Obj(reflect.clone()));
    it.define_method(&reflect, "apply", 3, |it, _, args| {
        let list = array_like_to_vec(it, &arg(args, 2))?;
        it.call(&arg(args, 0), arg(args, 1), &list)
    });
    it.define_method(&reflect, "construct", 2, |it, _, args| {
        let list = array_like_to_vec(it, &arg(args, 1))?;
        let nt = args.get(2).cloned();
        it.construct(&arg(args, 0), &list, nt.as_ref())
    });
    it.define_method(&reflect, "get", 2, |it, _, args| {
        let k = it.to_key(&arg(args, 1))?;
        it.get(&arg(args, 0), &k)
    });
    it.define_method(&reflect, "set", 3, |it, _, args| {
        let k = it.to_key(&arg(args, 1))?;
        it.set(&arg(args, 0), k, arg(args, 2))?;
        Ok(Value::Bool(true))
    });
    it.define_method(&reflect, "has", 2, |it, _, args| {
        let Value::Obj(o) = arg(args, 0) else { throw!(it, "TypeError", "Reflect.has called on non-object") };
        let k = it.to_key(&arg(args, 1))?;
        Ok(Value::Bool(it.has_property(&o, &k)))
    });
    it.define_method(&reflect, "ownKeys", 1, |it, _, args| {
        let Value::Obj(o) = arg(args, 0) else { throw!(it, "TypeError", "Reflect.ownKeys called on non-object") };
        let mut keys: Vec<Value> = it.own_keys(&o, true).into_iter().map(|k| k.to_value()).collect();
        for (k, _) in o.borrow().props.iter() {
            if let PropKey::Sym(_) = k {
                keys.push(k.to_value());
            }
        }
        Ok(it.new_array(keys))
    });
    it.define_method(&reflect, "getPrototypeOf", 1, |it, _, args| {
        let o = it.to_object(&arg(args, 0))?;
        Ok(o.proto().map(Value::Obj).unwrap_or(Value::Null))
    });
    it.define_method(&reflect, "setPrototypeOf", 2, |_, _, args| {
        if let Value::Obj(o) = arg(args, 0) {
            o.borrow_mut().proto = arg(args, 1).as_obj().cloned();
        }
        Ok(Value::Bool(true))
    });
    it.define_method(&reflect, "defineProperty", 3, |it, _, args| {
        object::define_property(it, &arg(args, 0), &arg(args, 1), &arg(args, 2))?;
        Ok(Value::Bool(true))
    });
    it.define_method(&reflect, "deleteProperty", 2, |it, _, args| {
        let k = it.to_key(&arg(args, 1))?;
        Ok(Value::Bool(it.delete(&arg(args, 0), &k)?))
    });
}

// ---------------------------------------------------------------- console

fn format_args(it: &mut Interp, args: &[Value]) -> String {
    let mut parts = Vec::new();
    let mut i = 0;
    if let Some(Value::Str(fmt)) = args.first() {
        if fmt.contains('%') && args.len() > 1 {
            let mut out = String::new();
            let mut chars = fmt.chars().peekable();
            i = 1;
            while let Some(c) = chars.next() {
                if c == '%' {
                    match chars.peek().copied() {
                        Some('s') | Some('d') | Some('i') | Some('f') | Some('o') | Some('O') | Some('j') => {
                            let spec = chars.next().unwrap();
                            let a = arg(args, i);
                            i += 1;
                            match spec {
                                's' => out.push_str(&it.to_string_lossy(&a)),
                                'd' | 'i' => {
                                    let n = it.to_number(&a).unwrap_or(f64::NAN);
                                    out.push_str(&num_to_string(n.trunc()))
                                }
                                'f' => out.push_str(&num_to_string(it.to_number(&a).unwrap_or(f64::NAN))),
                                _ => out.push_str(&it.inspect(&a, 2)),
                            }
                            continue;
                        }
                        Some('c') => {
                            chars.next();
                            i += 1;
                            continue;
                        }
                        Some('%') => {
                            chars.next();
                            out.push('%');
                            continue;
                        }
                        _ => {}
                    }
                }
                out.push(c);
            }
            parts.push(out);
        }
    }
    for a in &args[i..] {
        parts.push(it.inspect(a, 3));
    }
    parts.join(" ")
}

fn console_setup(it: &mut Interp) {
    let c = it.new_object();
    it.global.set_hidden("console", Value::Obj(c.clone()));
    for (name, level) in [("log", "log"), ("info", "info"), ("debug", "debug"), ("warn", "warn"), ("error", "error"), ("trace", "log"), ("dir", "log"), ("dirxml", "log")] {
        it.define_method(&c, name, 0, move |it, _, args| {
            let text = format_args(it, args);
            it.console.push(ConsoleMsg { level, text });
            Ok(Value::Undefined)
        });
    }
    it.define_method(&c, "assert", 0, |it, _, args| {
        if !arg(args, 0).truthy() {
            let rest = if args.len() > 1 { format_args(it, &args[1..]) } else { String::new() };
            it.console.push(ConsoleMsg { level: "error", text: format!("Assertion failed{}{}", if rest.is_empty() { "" } else { ": " }, rest) });
        }
        Ok(Value::Undefined)
    });
    it.define_method(&c, "table", 1, |it, _, args| {
        let text = it.inspect(&arg(args, 0), 3);
        it.console.push(ConsoleMsg { level: "log", text });
        Ok(Value::Undefined)
    });
    for name in ["group", "groupCollapsed", "groupEnd", "time", "timeEnd", "timeLog", "count", "countReset", "clear", "profile", "profileEnd"] {
        it.define_method(&c, name, 0, move |it, _, args| {
            if matches!(name, "group" | "groupCollapsed") && !args.is_empty() {
                let text = format_args(it, args);
                it.console.push(ConsoleMsg { level: "log", text });
            }
            Ok(Value::Undefined)
        });
    }
}
