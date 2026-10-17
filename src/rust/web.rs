//! The `web` module: the page's DOM and browser APIs as seen from Rust.
//!
//! Everything goes through the page's JavaScript interpreter, so Rust code sees exactly the
//! same document scripts do. JS values are carried as `Value::Js`.

use std::rc::Rc;

use super::ast::Pos;
use super::runtime::*;
use super::value::*;
use crate::js::interp::Interp;
use crate::js::value::{PropKey, Value as Js};

type Out = R<Option<Value>>;

const FNS: &[&str] = &[
    "document", "window", "body", "query", "query_all", "by_id", "create", "create_text", "alert", "confirm", "prompt", "set_timeout", "set_interval", "clear_timeout", "clear_interval", "request_animation_frame", "now", "random", "eval", "fetch_text", "fetch_json",
    "local_storage_get", "local_storage_set", "location", "navigate", "console_log", "console_warn", "console_error", "global", "set_global", "on",
];

pub fn has_fn(name: &str) -> bool {
    FNS.contains(&name)
}

fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).cloned().unwrap_or(Value::Unit)
}

fn need_interp(ctx: &mut Ctx, pos: Pos) -> R<&mut Interp> {
    match ctx.interp() {
        Some(i) => Ok(i),
        None => panic("the `web` API is only available inside a page", pos),
    }
}

/// Turn a thrown JS value into a Rust panic.
fn js_err(it: &mut Interp, e: Js, pos: Pos) -> Ctrl {
    Ctrl::Panic(format!("JavaScript error: {}", it.describe_error(&e)), pos)
}

fn js_str(ctx: &mut Ctx, v: &Value, pos: Pos) -> R<String> {
    match v.deref_all() {
        Value::Str(s) => Ok(s.borrow().clone()),
        Value::Char(c) => Ok(c.to_string()),
        other => super::format::display(ctx, &other, pos),
    }
}

/// `snake_case` → `camelCase`, the usual mapping from Rust names to DOM names.
fn camel(name: &str) -> String {
    let mut out = String::new();
    let mut up = false;
    for c in name.chars() {
        if c == '_' {
            up = !out.is_empty();
        } else if up {
            out.extend(c.to_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------- conversions

pub fn to_js(ctx: &mut Ctx, v: &Value, pos: Pos) -> R<Js> {
    Ok(match v {
        Value::Unit => Js::Undefined,
        Value::Bool(b) => Js::Bool(*b),
        Value::Int(i, _) => Js::Num(*i as f64),
        Value::Float(f, _) => Js::Num(*f),
        Value::Char(c) => Js::str(&c.to_string()),
        Value::Str(s) => Js::str(&s.borrow()),
        Value::Js(j) => j.clone(),
        Value::Ref(p) => return to_js(ctx, &p.get(), pos),
        Value::RcBox(b) => return to_js(ctx, b, pos),
        Value::Boxed(c) | Value::Cell(c) => {
            let inner = c.borrow().clone();
            return to_js(ctx, &inner, pos);
        }
        Value::Vec(items) | Value::Array(items) | Value::Tuple(items) => {
            let items = items.borrow().clone();
            let mut out = Vec::with_capacity(items.len());
            for i in &items {
                out.push(to_js(ctx, i, pos)?);
            }
            need_interp(ctx, pos)?.new_array(out)
        }
        Value::Map(m) => {
            let (keys, vals, is_set) = {
                let m = m.borrow();
                (m.keys.clone(), m.vals.clone(), m.is_set)
            };
            if is_set {
                let mut out = Vec::new();
                for k in &keys {
                    out.push(to_js(ctx, k, pos)?);
                }
                need_interp(ctx, pos)?.new_array(out)
            } else {
                let mut pairs = Vec::new();
                for (k, v) in keys.iter().zip(&vals) {
                    pairs.push((js_str(ctx, k, pos)?, to_js(ctx, v, pos)?));
                }
                let it = need_interp(ctx, pos)?;
                let o = Js::Obj(it.new_object());
                for (k, v) in pairs {
                    it.set(&o, PropKey::from(k.as_str()), v).map_err(|e| js_err(it, e, pos))?;
                }
                o
            }
        }
        Value::Struct(s) => {
            let fields = s.fields.borrow().clone();
            let mut pairs = Vec::new();
            for (n, f) in s.ty.fields.iter().zip(&fields) {
                pairs.push((n.clone(), to_js(ctx, f, pos)?));
            }
            let it = need_interp(ctx, pos)?;
            if s.ty.is_tuple {
                let vals = pairs.into_iter().map(|p| p.1).collect();
                return Ok(it.new_array(vals));
            }
            let o = Js::Obj(it.new_object());
            for (k, v) in pairs {
                it.set(&o, PropKey::from(k.as_str()), v).map_err(|e| js_err(it, e, pos))?;
            }
            o
        }
        Value::Enum(e) => {
            if let Some(o) = as_option(v) {
                return match o {
                    Some(x) => to_js(ctx, &x, pos),
                    None => Ok(Js::Null),
                };
            }
            let fields = e.fields.borrow().clone();
            let vname = e.ty.variants[e.variant].0.clone();
            if fields.is_empty() {
                Js::str(&vname)
            } else if fields.len() == 1 && e.ty.name == "Result" {
                return to_js(ctx, &fields[0], pos);
            } else {
                let mut vals = Vec::new();
                for f in &fields {
                    vals.push(to_js(ctx, f, pos)?);
                }
                let it = need_interp(ctx, pos)?;
                let o = Js::Obj(it.new_object());
                let payload = if vals.len() == 1 { vals.pop().unwrap() } else { it.new_array(vals) };
                it.set(&o, PropKey::from(vname.as_str()), payload).map_err(|e| js_err(it, e, pos))?;
                o
            }
        }
        Value::Func(_) => wrap_callback(ctx, v.clone(), pos)?,
        Value::Iter(i) => {
            let mut out = Vec::new();
            loop {
                let n = i.borrow_mut().pull(ctx)?;
                match n {
                    Some(x) => out.push(to_js(ctx, &x, pos)?),
                    None => break,
                }
            }
            need_interp(ctx, pos)?.new_array(out)
        }
        Value::Range(_) | Value::Formatter(..) => Js::Undefined,
    })
}

/// JS → Rust. Integral numbers become untyped integer literals (they adopt the kind of
/// whatever they're combined with); DOM objects stay `JsValue`s.
pub fn from_js(v: &Js) -> Value {
    match v {
        Js::Undefined => Value::Unit,
        Js::Bool(b) => Value::Bool(*b),
        Js::Num(n) if n.fract() == 0.0 && n.abs() < 9.0e15 => Value::Int(*n as i128, IntKind::Lit),
        Js::Num(n) => Value::Float(*n, false),
        Js::BigInt(b) => Value::Int(*b, IntKind::I128),
        Js::Str(s) => Value::str(&**s),
        other => Value::Js(other.clone()),
    }
}

/// A JS function that runs a Rust closure/function value in this program.
pub fn wrap_callback(ctx: &mut Ctx, f: Value, pos: Pos) -> R<Js> {
    let prog = ctx.prog.clone();
    let it = need_interp(ctx, pos)?;
    Ok(it.native("rust", 0, move |it, _this, args| {
        let rargs: Vec<Value> = args.iter().map(from_js).collect();
        invoke(it, &prog, &f, rargs, pos)
    }))
}

/// Run a Rust function on behalf of JS (event handlers, timers, `rust.name(...)` exports).
pub fn invoke(it: &mut Interp, prog: &Rc<Program>, f: &Value, args: Vec<Value>, pos: Pos) -> Result<Js, Js> {
    let mut ctx = Ctx::new(prog.clone(), it as *mut Interp);
    let r = ctx.call_value(f, args, pos).and_then(|v| to_js(&mut ctx, &v, pos));
    let output = std::mem::take(&mut ctx.output);
    drop(ctx);
    flush(it, output);
    match r {
        Ok(v) => Ok(v),
        Err(Ctrl::Panic(msg, p)) => {
            let text = panic_text(&prog.file, &msg, p);
            it.console.push(crate::js::interp::ConsoleMsg { level: "error", text: text.clone() });
            Err(it.make_error("Error", &format!("Rust panic: {msg}")))
        }
        Err(_) => Ok(Js::Undefined),
    }
}

pub fn panic_text(file: &str, msg: &str, p: Pos) -> String {
    format!("thread 'main' panicked at {file}:{}:{}:\n{msg}", p.line, p.col)
}

pub fn flush(it: &mut Interp, output: Vec<(&'static str, String)>) {
    for (level, text) in output {
        let (level, text) = if level == "print" { ("log", text.strip_suffix('\n').map(String::from).unwrap_or(text)) } else { (level, text) };
        it.console.push(crate::js::interp::ConsoleMsg { level, text });
    }
}

// ---------------------------------------------------------------- JS helpers

fn get(ctx: &mut Ctx, obj: &Js, key: &str, pos: Pos) -> R<Js> {
    let it = need_interp(ctx, pos)?;
    it.get_str(obj, key).map_err(|e| js_err(it, e, pos))
}

fn set(ctx: &mut Ctx, obj: &Js, key: &str, v: Js, pos: Pos) -> R<()> {
    let it = need_interp(ctx, pos)?;
    it.set_str(obj, key, v).map_err(|e| js_err(it, e, pos))
}

fn call_js(ctx: &mut Ctx, obj: &Js, method: &str, args: &[Value], pos: Pos) -> R<Js> {
    let mut jargs = Vec::with_capacity(args.len());
    for a in args {
        jargs.push(to_js(ctx, a, pos)?);
    }
    let f = get(ctx, obj, method, pos)?;
    let it = need_interp(ctx, pos)?;
    if !f.is_callable() {
        let d = it.short_desc(obj);
        return panic(format!("`{method}` is not a method of {d}"), pos);
    }
    it.call(&f, obj.clone(), &jargs).map_err(|e| js_err(it, e, pos))
}

fn global(ctx: &mut Ctx, pos: Pos) -> R<Js> {
    let it = need_interp(ctx, pos)?;
    Ok(Js::Obj(it.global.clone()))
}

fn document(ctx: &mut Ctx, pos: Pos) -> R<Js> {
    let g = global(ctx, pos)?;
    get(ctx, &g, "document", pos)
}

/// `null`/`undefined` → `None`, anything else → `Some(JsValue)`.
fn opt_js(ctx: &Ctx, v: Js) -> Value {
    if v.is_nullish() {
        ctx.none()
    } else {
        ctx.some(Value::Js(v))
    }
}

fn opt_str(ctx: &Ctx, v: Js) -> Value {
    match v {
        Js::Str(s) => ctx.some(Value::str(&*s)),
        _ => ctx.none(),
    }
}

/// Array-like JS collection → `Vec<JsValue>`.
fn js_list(ctx: &mut Ctx, v: &Js, pos: Pos) -> R<Value> {
    let it = need_interp(ctx, pos)?;
    let items = match it.iterate_to_vec(v) {
        Ok(items) => items,
        Err(_) => {
            let n = it.get_str(v, "length").map_err(|e| js_err(it, e, pos))?;
            let n = it.to_number(&n).unwrap_or(0.0) as usize;
            let mut out = Vec::with_capacity(n);
            for i in 0..n {
                out.push(it.get_str(v, &i.to_string()).map_err(|e| js_err(it, e, pos))?);
            }
            out
        }
    };
    Ok(Value::vec(items.iter().map(from_js).collect()))
}

fn timer(ctx: &mut Ctx, js_name: &str, args: &[Value], pos: Pos) -> R<Value> {
    let cb = wrap_callback(ctx, arg(args, 0), pos)?;
    let ms = to_js(ctx, &arg(args, 1), pos)?;
    let g = global(ctx, pos)?;
    let f = get(ctx, &g, js_name, pos)?;
    let it = need_interp(ctx, pos)?;
    let id = it.call(&f, g, &[cb, ms]).map_err(|e| js_err(it, e, pos))?;
    Ok(match from_js(&id) {
        Value::Int(i, _) => Value::Int(i, IntKind::I32),
        v => v,
    })
}

pub fn now_ms(ctx: &mut Ctx) -> f64 {
    if let Some(it) = ctx.interp() {
        let g = Js::Obj(it.global.clone());
        if let Ok(perf) = it.get_str(&g, "performance") {
            if let Ok(now) = it.get_str(&perf, "now") {
                if let Ok(Js::Num(n)) = it.call(&now, perf, &[]) {
                    return n;
                }
            }
        }
    }
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0)
}

// ---------------------------------------------------------------- free functions

pub fn call_fn(ctx: &mut Ctx, name: &str, args: Vec<Value>, pos: Pos) -> R<Value> {
    let a0 = arg(&args, 0);
    match name {
        "now" => return Ok(Value::Float(now_ms(ctx), false)),
        "random" => {
            if ctx.interp().is_none() {
                let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(1);
                return Ok(Value::Float((t % 1_000_000) as f64 / 1_000_000.0, false));
            }
            let g = global(ctx, pos)?;
            let math = get(ctx, &g, "Math", pos)?;
            let r = call_js(ctx, &math, "random", &[], pos)?;
            return Ok(Value::Float(if let Js::Num(n) = r { n } else { 0.0 }, false));
        }
        "console_log" | "console_warn" | "console_error" => {
            let s = js_str(ctx, &a0, pos)?;
            ctx.print(match name {
                "console_warn" => "warn",
                "console_error" => "error",
                _ => "log",
            }, s);
            return Ok(Value::Unit);
        }
        _ => {}
    }
    let doc = document(ctx, pos)?;
    Ok(match name {
        "document" => Value::Js(doc),
        "window" | "global" if args.is_empty() => Value::Js(global(ctx, pos)?),
        "global" => {
            let g = global(ctx, pos)?;
            let k = js_str(ctx, &a0, pos)?;
            from_js(&get(ctx, &g, &k, pos)?)
        }
        "set_global" => {
            let g = global(ctx, pos)?;
            let k = js_str(ctx, &a0, pos)?;
            let v = to_js(ctx, &arg(&args, 1), pos)?;
            set(ctx, &g, &k, v, pos)?;
            Value::Unit
        }
        "body" => Value::Js(get(ctx, &doc, "body", pos)?),
        "query" => {
            let r = call_js(ctx, &doc, "querySelector", &args, pos)?;
            opt_js(ctx, r)
        }
        "query_all" => {
            let r = call_js(ctx, &doc, "querySelectorAll", &args, pos)?;
            js_list(ctx, &r, pos)?
        }
        "by_id" => {
            let r = call_js(ctx, &doc, "getElementById", &args, pos)?;
            opt_js(ctx, r)
        }
        "create" => Value::Js(call_js(ctx, &doc, "createElement", &args, pos)?),
        "create_text" => Value::Js(call_js(ctx, &doc, "createTextNode", &args, pos)?),
        "on" => {
            // web::on("event", closure): listen on the window
            let g = global(ctx, pos)?;
            let cb = wrap_callback(ctx, arg(&args, 1), pos)?;
            let ev = to_js(ctx, &a0, pos)?;
            let f = get(ctx, &g, "addEventListener", pos)?;
            let it = need_interp(ctx, pos)?;
            it.call(&f, g, &[ev, cb]).map_err(|e| js_err(it, e, pos))?;
            Value::Unit
        }
        "alert" | "confirm" | "prompt" => {
            let g = global(ctx, pos)?;
            let r = call_js(ctx, &g, name, &args, pos)?;
            match name {
                "confirm" => Value::Bool(r.truthy()),
                "prompt" => opt_str(ctx, r),
                _ => Value::Unit,
            }
        }
        "set_timeout" => timer(ctx, "setTimeout", &args, pos)?,
        "set_interval" => timer(ctx, "setInterval", &args, pos)?,
        "request_animation_frame" => {
            let cb = wrap_callback(ctx, a0, pos)?;
            let g = global(ctx, pos)?;
            let f = get(ctx, &g, "requestAnimationFrame", pos)?;
            let it = need_interp(ctx, pos)?;
            from_js(&it.call(&f, g, &[cb]).map_err(|e| js_err(it, e, pos))?)
        }
        "clear_timeout" | "clear_interval" => {
            let g = global(ctx, pos)?;
            call_js(ctx, &g, if name == "clear_timeout" { "clearTimeout" } else { "clearInterval" }, &args, pos)?;
            Value::Unit
        }
        "eval" => {
            let src = js_str(ctx, &a0, pos)?;
            let it = need_interp(ctx, pos)?;
            from_js(&it.eval_script(&src, "rust eval").map_err(|e| js_err(it, e, pos))?)
        }
        "fetch_text" | "fetch_json" => {
            // fetch_text(url, |result: Result<String, String>| ...)
            let src = if name == "fetch_text" {
                "(function(u, ok, bad){ fetch(u).then(function(r){ if (!r.ok) throw new Error('HTTP ' + r.status); return r.text(); }).then(ok, function(e){ bad(String(e && e.message || e)); }); })"
            } else {
                "(function(u, ok, bad){ fetch(u).then(function(r){ if (!r.ok) throw new Error('HTTP ' + r.status); return r.json(); }).then(ok, function(e){ bad(String(e && e.message || e)); }); })"
            };
            let cb = arg(&args, 1);
            let prog = ctx.prog.clone();
            let (cb1, prog1) = (cb.clone(), prog.clone());
            let it = need_interp(ctx, pos)?;
            let helper = it.eval_script(src, "rust fetch").map_err(|e| js_err(it, e, pos))?;
            let ok = it.native("ok", 1, move |it, _, a| {
                let c = Ctx::new(prog1.clone(), it as *mut Interp);
                let v = c.ok(from_js(&crate::js::interp::arg(a, 0)));
                drop(c);
                invoke(it, &prog1, &cb1, vec![v], pos)
            });
            let bad = it.native("bad", 1, move |it, _, a| {
                let c = Ctx::new(prog.clone(), it as *mut Interp);
                let v = c.err(from_js(&crate::js::interp::arg(a, 0)));
                drop(c);
                invoke(it, &prog, &cb, vec![v], pos)
            });
            let url = to_js(ctx, &a0, pos)?;
            let it = need_interp(ctx, pos)?;
            it.call(&helper, Js::Undefined, &[url, ok, bad]).map_err(|e| js_err(it, e, pos))?;
            Value::Unit
        }
        "local_storage_get" | "local_storage_set" => {
            let g = global(ctx, pos)?;
            let ls = get(ctx, &g, "localStorage", pos)?;
            if name == "local_storage_get" {
                let r = call_js(ctx, &ls, "getItem", &args, pos)?;
                opt_str(ctx, r)
            } else {
                call_js(ctx, &ls, "setItem", &args, pos)?;
                Value::Unit
            }
        }
        "location" => {
            let g = global(ctx, pos)?;
            let loc = get(ctx, &g, "location", pos)?;
            from_js(&get(ctx, &loc, "href", pos)?)
        }
        "navigate" => {
            let g = global(ctx, pos)?;
            let loc = get(ctx, &g, "location", pos)?;
            let url = to_js(ctx, &a0, pos)?;
            set(ctx, &loc, "href", url, pos)?;
            Value::Unit
        }
        _ => return panic(format!("cannot find function `web::{name}`"), pos),
    })
}

// ---------------------------------------------------------------- methods on JS values

pub fn js_method(ctx: &mut Ctx, v: &Value, name: &str, args: &[Value], pos: Pos) -> Out {
    let Value::Js(obj) = v else { return Ok(None) };
    let a0 = arg(args, 0);
    // conversions that work on any JS value
    match name {
        "is_null" => return Ok(Some(Value::Bool(obj.is_nullish()))),
        "as_f64" => {
            return Ok(Some(ctx.opt(match obj {
                Js::Num(n) => Some(Value::Float(*n, false)),
                _ => None,
            })))
        }
        "as_string" => {
            return Ok(Some(ctx.opt(match obj {
                Js::Str(s) => Some(Value::str(&**s)),
                _ => None,
            })))
        }
        "as_bool" => {
            return Ok(Some(ctx.opt(match obj {
                Js::Bool(b) => Some(Value::Bool(*b)),
                _ => None,
            })))
        }
        "to_string" => {
            let it = need_interp(ctx, pos)?;
            return Ok(Some(Value::str(it.to_string_lossy(obj))));
        }
        "clone" => return Ok(Some(v.clone())),
        "eq" => return Ok(Some(Value::Bool(match arg(args, 0).deref_all() {
            Value::Js(o) => crate::js::interp::strict_equals(obj, &o),
            _ => false,
        }))),
        _ => {}
    }
    if !matches!(obj, Js::Obj(_)) {
        return panic(format!("cannot call `{name}` on a JavaScript primitive"), pos);
    }
    let style = || -> R<Js> { Ok(Js::Undefined) };
    let _ = style;
    Ok(Some(match name {
        "text" => from_js(&get(ctx, obj, "textContent", pos)?),
        "set_text" => {
            let s = js_str(ctx, &a0, pos)?;
            set(ctx, obj, "textContent", Js::str(&s), pos)?;
            Value::Unit
        }
        "html" => from_js(&get(ctx, obj, "innerHTML", pos)?),
        "set_html" => {
            let s = js_str(ctx, &a0, pos)?;
            set(ctx, obj, "innerHTML", Js::str(&s), pos)?;
            Value::Unit
        }
        "attr" => {
            let r = call_js(ctx, obj, "getAttribute", args, pos)?;
            opt_str(ctx, r)
        }
        "set_attr" => {
            let k = js_str(ctx, &a0, pos)?;
            let val = js_str(ctx, &arg(args, 1), pos)?;
            call_js(ctx, obj, "setAttribute", &[Value::str(k), Value::str(val)], pos)?;
            Value::Unit
        }
        "remove_attr" => {
            call_js(ctx, obj, "removeAttribute", args, pos)?;
            Value::Unit
        }
        "has_attr" => Value::Bool(call_js(ctx, obj, "hasAttribute", args, pos)?.truthy()),
        "add_class" | "remove_class" | "toggle_class" | "has_class" => {
            let cl = get(ctx, obj, "classList", pos)?;
            let m = match name {
                "add_class" => "add",
                "remove_class" => "remove",
                "toggle_class" => "toggle",
                _ => "contains",
            };
            let r = call_js(ctx, &cl, m, args, pos)?;
            if name == "add_class" || name == "remove_class" { Value::Unit } else { Value::Bool(r.truthy()) }
        }
        "value" => {
            let r = get(ctx, obj, "value", pos)?;
            let it = need_interp(ctx, pos)?;
            Value::str(it.to_string_lossy(&r))
        }
        "set_value" => {
            let s = js_str(ctx, &a0, pos)?;
            set(ctx, obj, "value", Js::str(&s), pos)?;
            Value::Unit
        }
        "checked" => Value::Bool(get(ctx, obj, "checked", pos)?.truthy()),
        "set_checked" => {
            set(ctx, obj, "checked", Js::Bool(a0.truthy()), pos)?;
            Value::Unit
        }
        "style" => {
            let st = get(ctx, obj, "style", pos)?;
            let r = call_js(ctx, &st, "getPropertyValue", args, pos)?;
            let it = need_interp(ctx, pos)?;
            Value::str(it.to_string_lossy(&r))
        }
        "set_style" => {
            let st = get(ctx, obj, "style", pos)?;
            let k = js_str(ctx, &a0, pos)?;
            let val = js_str(ctx, &arg(args, 1), pos)?;
            call_js(ctx, &st, "setProperty", &[Value::str(k), Value::str(val)], pos)?;
            Value::Unit
        }
        "append" | "prepend" => {
            let child = match a0.deref_all() {
                Value::Js(j) => Value::Js(j),
                other => {
                    let s = js_str(ctx, &other, pos)?;
                    let doc = document(ctx, pos)?;
                    Value::Js(call_js(ctx, &doc, "createTextNode", &[Value::str(s)], pos)?)
                }
            };
            call_js(ctx, obj, if name == "append" { "appendChild" } else { "prepend" }, &[child], pos)?;
            Value::Unit
        }
        "remove" => {
            call_js(ctx, obj, "remove", &[], pos)?;
            Value::Unit
        }
        "clear" => {
            set(ctx, obj, "textContent", Js::str(""), pos)?;
            Value::Unit
        }
        "query" => {
            let r = call_js(ctx, obj, "querySelector", args, pos)?;
            opt_js(ctx, r)
        }
        "query_all" => {
            let r = call_js(ctx, obj, "querySelectorAll", args, pos)?;
            js_list(ctx, &r, pos)?
        }
        "children" => {
            let r = get(ctx, obj, "children", pos)?;
            js_list(ctx, &r, pos)?
        }
        "parent" => {
            let r = get(ctx, obj, "parentElement", pos)?;
            opt_js(ctx, r)
        }
        "on" | "off" => {
            let ev = to_js(ctx, &a0, pos)?;
            let cb = wrap_callback(ctx, arg(args, 1), pos)?;
            let f = get(ctx, obj, "addEventListener", pos)?;
            let it = need_interp(ctx, pos)?;
            it.call(&f, obj.clone(), &[ev, cb]).map_err(|e| js_err(it, e, pos))?;
            Value::Unit
        }
        "id" => from_js(&get(ctx, obj, "id", pos)?),
        "tag" => {
            let t = get(ctx, obj, "tagName", pos)?;
            Value::str(match t {
                Js::Str(s) => s.to_ascii_lowercase(),
                _ => String::new(),
            })
        }
        "target" => {
            let r = get(ctx, obj, "target", pos)?;
            opt_js(ctx, r)
        }
        "key" => from_js(&get(ctx, obj, "key", pos)?),
        "get" => {
            let k = js_str(ctx, &a0, pos)?;
            from_js(&get(ctx, obj, &k, pos)?)
        }
        "set" => {
            let k = js_str(ctx, &a0, pos)?;
            let val = to_js(ctx, &arg(args, 1), pos)?;
            set(ctx, obj, &k, val, pos)?;
            Value::Unit
        }
        "call" => {
            let m = js_str(ctx, &a0, pos)?;
            let rest: Vec<Value> = match arg(args, 1).deref_all() {
                Value::Vec(v) | Value::Array(v) | Value::Tuple(v) => v.borrow().clone(),
                Value::Unit => vec![],
                other => vec![other],
            };
            from_js(&call_js(ctx, obj, &m, &rest, pos)?)
        }
        "len" => from_js(&get(ctx, obj, "length", pos)?),
        "to_vec" | "iter" | "into_iter" => {
            let l = js_list(ctx, obj, pos)?;
            if name == "to_vec" {
                l
            } else {
                match super::stdlib::to_iter(ctx, &l, false, pos)? {
                    Some(i) => Value::Iter(i),
                    None => Value::Unit,
                }
            }
        }
        _ => {
            // fall back to the JS object itself: `el.focus()`, `ev.prevent_default()`,
            // `el.client_width()`, `el.set_title("x")`
            let js_name = camel(name);
            let f = get(ctx, obj, &js_name, pos)?;
            if f.is_callable() {
                from_js(&call_js(ctx, obj, &js_name, args, pos)?)
            } else if args.is_empty() {
                if f.is_undefined() {
                    let it = need_interp(ctx, pos)?;
                    let has = match obj {
                        Js::Obj(o) => it.has_property(o, &PropKey::from(js_name.as_str())),
                        _ => false,
                    };
                    if !has {
                        let d = it.short_desc(obj);
                        return panic(format!("no method named `{name}` found for {d}"), pos);
                    }
                }
                from_js(&f)
            } else if let Some(prop) = name.strip_prefix("set_") {
                let val = to_js(ctx, &a0, pos)?;
                set(ctx, obj, &camel(prop), val, pos)?;
                Value::Unit
            } else {
                let it = need_interp(ctx, pos)?;
                let d = it.short_desc(obj);
                return panic(format!("no method named `{name}` found for {d}"), pos);
            }
        }
    }))
}
