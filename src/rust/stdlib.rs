//! Built-in methods and associated functions (the parts of `std` pages use most).

use std::cell::RefCell;
use std::cmp::Ordering;
use std::rc::Rc;

use super::ast::{Pos, Type};
use super::runtime::*;
use super::value::*;

type Out = R<Option<Value>>;

fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).cloned().unwrap_or(Value::Unit)
}

fn int_of(v: &Value, pos: Pos) -> R<i128> {
    match v.deref_all() {
        Value::Int(i, _) => Ok(i),
        Value::Float(f, _) => Ok(f as i128),
        other => panic(format!("expected an integer, found `{}`", type_name(&other)), pos),
    }
}

fn usize_of(v: &Value, pos: Pos) -> R<usize> {
    let i = int_of(v, pos)?;
    if i < 0 {
        return panic("expected a non-negative index", pos);
    }
    Ok(i as usize)
}

fn f64_of(v: &Value, pos: Pos) -> R<f64> {
    match v.deref_all() {
        Value::Float(f, _) => Ok(f),
        Value::Int(i, _) => Ok(i as f64),
        other => panic(format!("expected a float, found `{}`", type_name(&other)), pos),
    }
}

fn str_of(v: &Value, pos: Pos) -> R<String> {
    match v.deref_all() {
        Value::Str(s) => Ok(s.borrow().clone()),
        Value::Char(c) => Ok(c.to_string()),
        other => panic(format!("expected a string, found `{}`", type_name(&other)), pos),
    }
}

fn bool_v(b: bool) -> Option<Value> {
    Some(Value::Bool(b))
}

fn usize_v(n: usize) -> Option<Value> {
    Some(Value::usize(n))
}

fn call1(ctx: &mut Ctx, f: &Value, a: Value, pos: Pos) -> R<Value> {
    ctx.call_value(f, vec![a], pos)
}

fn call2(ctx: &mut Ctx, f: &Value, a: Value, b: Value, pos: Pos) -> R<Value> {
    ctx.call_value(f, vec![a, b], pos)
}

fn ordering_of(v: &Value, pos: Pos) -> R<Ordering> {
    match v.deref_all() {
        Value::Enum(e) if e.ty.name == "Ordering" => Ok(match e.variant {
            0 => Ordering::Less,
            1 => Ordering::Equal,
            _ => Ordering::Greater,
        }),
        other => match as_option(&other) {
            Some(Some(inner)) => ordering_of(&inner, pos),
            _ => panic(format!("expected `Ordering`, found `{}`", type_name(&other)), pos),
        },
    }
}

/// Stable merge sort with a fallible comparator.
fn sort_values(ctx: &mut Ctx, items: &mut Vec<Value>, cmp: &mut dyn FnMut(&mut Ctx, &Value, &Value) -> R<Ordering>) -> R<()> {
    let n = items.len();
    if n < 2 {
        return Ok(());
    }
    let mut src = std::mem::take(items);
    let mut buf: Vec<Value> = Vec::with_capacity(n);
    let mut width = 1;
    while width < n {
        buf.clear();
        let mut i = 0;
        while i < n {
            let mid = (i + width).min(n);
            let hi = (i + 2 * width).min(n);
            let (mut a, mut b) = (i, mid);
            while a < mid && b < hi {
                if cmp(ctx, &src[b], &src[a])? == Ordering::Less {
                    buf.push(src[b].clone());
                    b += 1;
                } else {
                    buf.push(src[a].clone());
                    a += 1;
                }
            }
            buf.extend_from_slice(&src[a..mid]);
            buf.extend_from_slice(&src[b..hi]);
            i = hi;
        }
        std::mem::swap(&mut src, &mut buf);
        width *= 2;
    }
    *items = src;
    Ok(())
}

// ---------------------------------------------------------------- iterators

pub fn iter_from_vec(items: Vec<Value>) -> Value {
    let mut i = 0;
    Value::Iter(Rc::new(RefCell::new(IterVal::new(move |_| {
        let v = items.get(i).cloned();
        i += 1;
        Ok(v)
    }))))
}

/// Turn an iterable into an iterator. `refs` yields `&mut` references into vectors/maps.
pub fn to_iter(ctx: &mut Ctx, v: &Value, refs: bool, pos: Pos) -> R<Option<Rc<RefCell<IterVal>>>> {
    Ok(Some(match v.deref_all() {
        Value::Iter(i) => i,
        Value::Vec(items) | Value::Array(items) => {
            if refs {
                let n = items.borrow().len();
                let mut i = 0;
                Rc::new(RefCell::new(IterVal::new(move |_| {
                    if i >= n {
                        return Ok(None);
                    }
                    i += 1;
                    Ok(Some(Value::Ref(Rc::new(Place::Elem(items.clone(), i - 1)))))
                })))
            } else {
                let snapshot = items.borrow().clone();
                match iter_from_vec(snapshot) {
                    Value::Iter(i) => i,
                    _ => unreachable!(),
                }
            }
        }
        Value::Range(r) => {
            let (lo, hi, incl) = (r.0.clone(), r.1.clone(), r.2);
            match (lo, hi) {
                (Value::Int(a, k), Value::Int(b, _)) => {
                    let k = if k == IntKind::Lit { IntKind::I32 } else { k };
                    let end = if incl { b + 1 } else { b };
                    let mut i = a;
                    Rc::new(RefCell::new(IterVal::new(move |_| {
                        if i >= end {
                            return Ok(None);
                        }
                        i += 1;
                        Ok(Some(Value::Int(i - 1, k)))
                    })))
                }
                (Value::Int(a, k), Value::Unit) => {
                    let k = if k == IntKind::Lit { IntKind::I32 } else { k };
                    let mut i = a;
                    Rc::new(RefCell::new(IterVal::new(move |_| {
                        i += 1;
                        Ok(Some(Value::Int(i - 1, k)))
                    })))
                }
                (Value::Char(a), Value::Char(b)) => {
                    let end = b as u32 + incl as u32;
                    let mut i = a as u32;
                    Rc::new(RefCell::new(IterVal::new(move |_| {
                        while i < end {
                            i += 1;
                            if let Some(c) = char::from_u32(i - 1) {
                                return Ok(Some(Value::Char(c)));
                            }
                        }
                        Ok(None)
                    })))
                }
                _ => return panic("cannot iterate over this range", pos),
            }
        }
        Value::Map(m) => {
            let (keys, vals, is_set) = {
                let m = m.borrow();
                (m.keys.clone(), m.vals.clone(), m.is_set)
            };
            let items: Vec<Value> = keys
                .into_iter()
                .zip(vals)
                .map(|(k, v)| if is_set { k } else if refs { Value::tuple(vec![k.clone(), Value::Ref(Rc::new(Place::MapVal(m.clone(), k)))]) } else { Value::tuple(vec![k, v]) })
                .collect();
            match iter_from_vec(items) {
                Value::Iter(i) => i,
                _ => unreachable!(),
            }
        }
        Value::Enum(e) if e.ty.name == "Option" => {
            let items = e.fields.borrow().clone();
            match iter_from_vec(items) {
                Value::Iter(i) => i,
                _ => unreachable!(),
            }
        }
        other @ (Value::Struct(_) | Value::Enum(_)) => {
            // a user type implementing Iterator
            let tn = type_name(&other);
            let Some(code) = ctx.prog.method(&tn, "next") else { return Ok(None) };
            let holder = Rc::new(Place::Temp(RefCell::new(other.clone())));
            Rc::new(RefCell::new(IterVal::new(move |c| {
                let n = c.call_code(&code, &[], vec![Value::Ref(holder.clone())], pos)?;
                Ok(as_option(&n).flatten())
            })))
        }
        _ => return Ok(None),
    }))
}

fn new_iter(f: impl FnMut(&mut Ctx) -> R<Option<Value>> + 'static) -> Value {
    Value::Iter(Rc::new(RefCell::new(IterVal::new(f))))
}

fn drain(ctx: &mut Ctx, it: &Rc<RefCell<IterVal>>, pos: Pos) -> R<Vec<Value>> {
    let mut out = Vec::new();
    loop {
        ctx.tick(pos)?;
        let n = it.borrow_mut().pull(ctx)?;
        match n {
            Some(v) => out.push(v),
            None => return Ok(out),
        }
        if out.len() > 50_000_000 {
            return panic("iterator produced too many items", pos);
        }
    }
}

fn collect(ctx: &mut Ctx, items: Vec<Value>, hint: Option<&Type>, pos: Pos) -> R<Value> {
    let name = hint.map(|t| t.name()).unwrap_or("Vec");
    Ok(match name {
        "String" => {
            let mut s = String::new();
            for v in items {
                match v.deref_all() {
                    Value::Char(c) => s.push(c),
                    Value::Str(x) => s.push_str(&x.borrow()),
                    other => s.push_str(&super::format::display(ctx, &other, pos)?),
                }
            }
            Value::str(s)
        }
        "HashMap" | "BTreeMap" => {
            let mut m = MapVal::new(name == "BTreeMap", false);
            for v in items {
                let Value::Tuple(t) = v.deref_all() else { return panic("collecting into a map needs (key, value) pairs", pos) };
                let t = t.borrow();
                m.insert(t.first().cloned().unwrap_or(Value::Unit), t.get(1).cloned().unwrap_or(Value::Unit));
            }
            Value::Map(Rc::new(RefCell::new(m)))
        }
        "HashSet" | "BTreeSet" => {
            let mut m = MapVal::new(name == "BTreeSet", true);
            for v in items {
                m.insert(v, Value::Unit);
            }
            Value::Map(Rc::new(RefCell::new(m)))
        }
        "Result" => {
            let inner = hint.and_then(|t| t.args().first());
            let mut ok = Vec::new();
            for v in items {
                match as_result(&v) {
                    Some(Ok(x)) => ok.push(x),
                    Some(Err(e)) => return Ok(ctx.err(e)),
                    None => ok.push(v),
                }
            }
            let c = collect(ctx, ok, inner, pos)?;
            ctx.ok(c)
        }
        "Option" => {
            let inner = hint.and_then(|t| t.args().first());
            let mut ok = Vec::new();
            for v in items {
                match as_option(&v) {
                    Some(Some(x)) => ok.push(x),
                    Some(None) => return Ok(ctx.none()),
                    None => ok.push(v),
                }
            }
            let c = collect(ctx, ok, inner, pos)?;
            ctx.some(c)
        }
        _ => {
            let elem = hint.and_then(|t| t.args().first()).cloned();
            match elem {
                Some(t) if IntKind::from_name(t.name()).is_some() => {
                    let k = IntKind::from_name(t.name()).unwrap();
                    Value::vec(items.into_iter().map(|v| match v {
                        Value::Int(x, _) => Value::Int(x, k),
                        v => v,
                    }).collect())
                }
                _ => Value::vec(items),
            }
        }
    })
}

fn sum_values(ctx: &mut Ctx, items: Vec<Value>, product: bool, hint: Option<&Type>, pos: Pos) -> R<Value> {
    let hint_name = hint.map(|t| t.name()).unwrap_or("");
    if items.is_empty() {
        let unit = if product { 1 } else { 0 };
        return Ok(if hint_name == "f64" || hint_name == "f32" { Value::Float(unit as f64, hint_name == "f32") } else { Value::Int(unit, IntKind::from_name(hint_name).unwrap_or(IntKind::I32)) });
    }
    let mut acc = items[0].deref_all();
    for v in &items[1..] {
        acc = binary(ctx, if product { super::ast::BinOp::Mul } else { super::ast::BinOp::Add }, &acc, v, pos)?;
    }
    if let (Some(k), Value::Int(x, _)) = (IntKind::from_name(hint_name), &acc) {
        acc = Value::Int(*x, k);
    }
    Ok(acc)
}

fn iter_method(ctx: &mut Ctx, it: Rc<RefCell<IterVal>>, name: &str, generics: &[Type], args: &[Value], pos: Pos) -> Out {
    let f = arg(args, 0);
    Ok(Some(match name {
        "next" => {
            let n = it.borrow_mut().pull(ctx)?;
            ctx.opt(n)
        }
        "peek" | "peek_mut" => {
            let mut b = it.borrow_mut();
            if b.peeked.is_none() {
                let n = (b.next)(ctx)?;
                b.peeked = Some(n);
            }
            let p = b.peeked.clone().unwrap();
            drop(b);
            ctx.opt(p)
        }
        "next_if" => {
            let mut b = it.borrow_mut();
            if b.peeked.is_none() {
                let n = (b.next)(ctx)?;
                b.peeked = Some(n);
            }
            let p = b.peeked.clone().unwrap();
            drop(b);
            match p {
                Some(v) if call1(ctx, &f, v.clone(), pos)?.truthy() => {
                    it.borrow_mut().peeked = None;
                    ctx.some(v)
                }
                _ => ctx.none(),
            }
        }
        "iter" | "into_iter" | "by_ref" | "fuse" | "cloned" | "copied" | "peekable" | "iter_mut" => {
            if name == "cloned" || name == "copied" {
                let inner = it.clone();
                new_iter(move |c| Ok(inner.borrow_mut().pull(c)?.map(|v| v.deref_all().deep_clone())))
            } else {
                Value::Iter(it)
            }
        }
        "map" => {
            let inner = it.clone();
            new_iter(move |c| match inner.borrow_mut().pull(c)? {
                Some(v) => Ok(Some(call1(c, &f, v, pos)?)),
                None => Ok(None),
            })
        }
        "filter" => {
            let inner = it.clone();
            new_iter(move |c| loop {
                let n = inner.borrow_mut().pull(c)?;
                match n {
                    Some(v) => {
                        if call1(c, &f, v.clone(), pos)?.truthy() {
                            return Ok(Some(v));
                        }
                    }
                    None => return Ok(None),
                }
            })
        }
        "filter_map" => {
            let inner = it.clone();
            new_iter(move |c| loop {
                let n = inner.borrow_mut().pull(c)?;
                match n {
                    Some(v) => {
                        let r = call1(c, &f, v, pos)?;
                        if let Some(Some(x)) = as_option(&r) {
                            return Ok(Some(x));
                        }
                    }
                    None => return Ok(None),
                }
            })
        }
        "map_while" => {
            let inner = it.clone();
            let mut done = false;
            new_iter(move |c| {
                if done {
                    return Ok(None);
                }
                let n = inner.borrow_mut().pull(c)?;
                match n {
                    Some(v) => match as_option(&call1(c, &f, v, pos)?) {
                        Some(Some(x)) => Ok(Some(x)),
                        _ => {
                            done = true;
                            Ok(None)
                        }
                    },
                    None => Ok(None),
                }
            })
        }
        "enumerate" => {
            let inner = it.clone();
            let mut i = 0usize;
            new_iter(move |c| match inner.borrow_mut().pull(c)? {
                Some(v) => {
                    i += 1;
                    Ok(Some(Value::tuple(vec![Value::usize(i - 1), v])))
                }
                None => Ok(None),
            })
        }
        "zip" => {
            let inner = it.clone();
            let Some(other) = to_iter(ctx, &f, false, pos)? else { return panic("`zip` needs an iterable", pos) };
            new_iter(move |c| {
                let a = inner.borrow_mut().pull(c)?;
                let Some(a) = a else { return Ok(None) };
                let b = other.borrow_mut().pull(c)?;
                Ok(b.map(|b| Value::tuple(vec![a, b])))
            })
        }
        "chain" => {
            let inner = it.clone();
            let Some(other) = to_iter(ctx, &f, false, pos)? else { return panic("`chain` needs an iterable", pos) };
            let mut first_done = false;
            new_iter(move |c| {
                if !first_done {
                    if let Some(v) = inner.borrow_mut().pull(c)? {
                        return Ok(Some(v));
                    }
                    first_done = true;
                }
                other.borrow_mut().pull(c)
            })
        }
        "skip" => {
            let inner = it.clone();
            let mut n = usize_of(&f, pos)?;
            new_iter(move |c| {
                while n > 0 {
                    n -= 1;
                    if inner.borrow_mut().pull(c)?.is_none() {
                        return Ok(None);
                    }
                }
                inner.borrow_mut().pull(c)
            })
        }
        "take" => {
            let inner = it.clone();
            let mut n = usize_of(&f, pos)?;
            new_iter(move |c| {
                if n == 0 {
                    return Ok(None);
                }
                n -= 1;
                inner.borrow_mut().pull(c)
            })
        }
        "step_by" => {
            let inner = it.clone();
            let step = usize_of(&f, pos)?.max(1);
            let mut first = true;
            new_iter(move |c| {
                if first {
                    first = false;
                    return inner.borrow_mut().pull(c);
                }
                for _ in 0..step - 1 {
                    if inner.borrow_mut().pull(c)?.is_none() {
                        return Ok(None);
                    }
                }
                inner.borrow_mut().pull(c)
            })
        }
        "take_while" => {
            let inner = it.clone();
            let mut done = false;
            new_iter(move |c| {
                if done {
                    return Ok(None);
                }
                match inner.borrow_mut().pull(c)? {
                    Some(v) if call1(c, &f, v.clone(), pos)?.truthy() => Ok(Some(v)),
                    _ => {
                        done = true;
                        Ok(None)
                    }
                }
            })
        }
        "skip_while" => {
            let inner = it.clone();
            let mut skipping = true;
            new_iter(move |c| loop {
                let n = inner.borrow_mut().pull(c)?;
                match n {
                    Some(v) => {
                        if skipping && call1(c, &f, v.clone(), pos)?.truthy() {
                            continue;
                        }
                        skipping = false;
                        return Ok(Some(v));
                    }
                    None => return Ok(None),
                }
            })
        }
        "inspect" => {
            let inner = it.clone();
            new_iter(move |c| match inner.borrow_mut().pull(c)? {
                Some(v) => {
                    call1(c, &f, v.clone(), pos)?;
                    Ok(Some(v))
                }
                None => Ok(None),
            })
        }
        "flat_map" | "flatten" => {
            let inner = it.clone();
            let is_flat_map = name == "flat_map";
            let mut cur: Option<Rc<RefCell<IterVal>>> = None;
            new_iter(move |c| loop {
                if let Some(ci) = &cur {
                    if let Some(v) = ci.borrow_mut().pull(c)? {
                        return Ok(Some(v));
                    }
                    cur = None;
                }
                let n = inner.borrow_mut().pull(c)?;
                match n {
                    Some(v) => {
                        let v = if is_flat_map { call1(c, &f, v, pos)? } else { v };
                        if let Some(o) = as_option(&v) {
                            if let Some(x) = o {
                                return Ok(Some(x));
                            }
                            continue;
                        }
                        if let Value::Str(s) = v.deref_all() {
                            // `flat_map(|s| s.chars())` already produces an iterator; a plain string flattens to chars
                            let chars: Vec<Value> = s.borrow().chars().map(Value::Char).collect();
                            if let Value::Iter(i) = iter_from_vec(chars) {
                                cur = Some(i);
                            }
                            continue;
                        }
                        cur = to_iter(c, &v, false, pos)?;
                        if cur.is_none() {
                            return panic("`flatten` needs nested iterables", pos);
                        }
                    }
                    None => return Ok(None),
                }
            })
        }
        "scan" => {
            let inner = it.clone();
            let state = Rc::new(Place::Temp(RefCell::new(f.clone())));
            let g = arg(args, 1);
            let mut done = false;
            new_iter(move |c| {
                if done {
                    return Ok(None);
                }
                let n = inner.borrow_mut().pull(c)?;
                match n {
                    Some(v) => match as_option(&c.call_value(&g, vec![Value::Ref(state.clone()), v], pos)?) {
                        Some(Some(x)) => Ok(Some(x)),
                        _ => {
                            done = true;
                            Ok(None)
                        }
                    },
                    None => Ok(None),
                }
            })
        }
        "rev" => {
            let mut items = drain(ctx, &it, pos)?;
            items.reverse();
            iter_from_vec(items)
        }
        "cycle" => {
            let items = drain(ctx, &it, pos)?;
            let mut i = 0usize;
            new_iter(move |_| {
                if items.is_empty() {
                    return Ok(None);
                }
                i += 1;
                Ok(Some(items[(i - 1) % items.len()].clone()))
            })
        }
        "chunks" => {
            let items = drain(ctx, &it, pos)?;
            let n = usize_of(&f, pos)?.max(1);
            iter_from_vec(items.chunks(n).map(|c| Value::vec(c.to_vec())).collect())
        }
        "collect" => {
            let items = drain(ctx, &it, pos)?;
            collect(ctx, items, generics.first(), pos)?
        }
        "count" => Value::usize(drain(ctx, &it, pos)?.len()),
        "sum" | "product" => {
            let items = drain(ctx, &it, pos)?;
            sum_values(ctx, items, name == "product", generics.first(), pos)?
        }
        "last" => {
            let items = drain(ctx, &it, pos)?;
            let l = items.last().cloned();
            ctx.opt(l)
        }
        "nth" => {
            let n = usize_of(&f, pos)?;
            let mut out = None;
            for _ in 0..=n {
                out = it.borrow_mut().pull(ctx)?;
                if out.is_none() {
                    break;
                }
            }
            ctx.opt(out)
        }
        "min" | "max" => {
            let items = drain(ctx, &it, pos)?;
            let mut best: Option<Value> = None;
            for v in items {
                best = Some(match best {
                    None => v,
                    Some(b) => {
                        let o = user_cmp(ctx, &v.deref_all(), &b.deref_all(), pos)?;
                        if (name == "min" && o == Ordering::Less) || (name == "max" && o != Ordering::Less) { v } else { b }
                    }
                });
            }
            ctx.opt(best)
        }
        "min_by_key" | "max_by_key" => {
            let items = drain(ctx, &it, pos)?;
            let mut best: Option<(Value, Value)> = None;
            for v in items {
                let k = call1(ctx, &f, v.clone(), pos)?;
                best = Some(match best {
                    None => (v, k),
                    Some((bv, bk)) => {
                        let o = user_cmp(ctx, &k.deref_all(), &bk.deref_all(), pos)?;
                        if (name == "min_by_key" && o == Ordering::Less) || (name == "max_by_key" && o != Ordering::Less) { (v, k) } else { (bv, bk) }
                    }
                });
            }
            ctx.opt(best.map(|b| b.0))
        }
        "min_by" | "max_by" => {
            let items = drain(ctx, &it, pos)?;
            let mut best: Option<Value> = None;
            for v in items {
                best = Some(match best {
                    None => v,
                    Some(b) => {
                        let o = ordering_of(&call2(ctx, &f, v.clone(), b.clone(), pos)?, pos)?;
                        if (name == "min_by" && o == Ordering::Less) || (name == "max_by" && o != Ordering::Less) { v } else { b }
                    }
                });
            }
            ctx.opt(best)
        }
        "fold" => {
            let mut acc = f.clone();
            let g = arg(args, 1);
            loop {
                ctx.tick(pos)?;
                let n = it.borrow_mut().pull(ctx)?;
                match n {
                    Some(v) => acc = call2(ctx, &g, acc, v, pos)?,
                    None => break,
                }
            }
            acc
        }
        "reduce" => {
            let first = it.borrow_mut().pull(ctx)?;
            let Some(mut acc) = first else { return Ok(Some(ctx.none())) };
            loop {
                let n = it.borrow_mut().pull(ctx)?;
                match n {
                    Some(v) => acc = call2(ctx, &f, acc, v, pos)?,
                    None => break,
                }
            }
            ctx.some(acc)
        }
        "for_each" => {
            loop {
                ctx.tick(pos)?;
                let n = it.borrow_mut().pull(ctx)?;
                match n {
                    Some(v) => {
                        call1(ctx, &f, v, pos)?;
                    }
                    None => break,
                }
            }
            Value::Unit
        }
        "any" | "all" => {
            let want_any = name == "any";
            loop {
                let n = it.borrow_mut().pull(ctx)?;
                match n {
                    Some(v) => {
                        let r = call1(ctx, &f, v, pos)?.truthy();
                        if r == want_any {
                            return Ok(Some(Value::Bool(want_any)));
                        }
                    }
                    None => return Ok(Some(Value::Bool(!want_any))),
                }
            }
        }
        "find" => loop {
            let n = it.borrow_mut().pull(ctx)?;
            match n {
                Some(v) => {
                    if call1(ctx, &f, v.clone(), pos)?.truthy() {
                        return Ok(Some(ctx.some(v)));
                    }
                }
                None => return Ok(Some(ctx.none())),
            }
        },
        "find_map" => loop {
            let n = it.borrow_mut().pull(ctx)?;
            match n {
                Some(v) => {
                    let r = call1(ctx, &f, v, pos)?;
                    if let Some(Some(_)) = as_option(&r) {
                        return Ok(Some(r));
                    }
                }
                None => return Ok(Some(ctx.none())),
            }
        },
        "position" => {
            let mut i = 0usize;
            loop {
                let n = it.borrow_mut().pull(ctx)?;
                match n {
                    Some(v) => {
                        if call1(ctx, &f, v, pos)?.truthy() {
                            return Ok(Some(ctx.some(Value::usize(i))));
                        }
                        i += 1;
                    }
                    None => return Ok(Some(ctx.none())),
                }
            }
        }
        "partition" => {
            let items = drain(ctx, &it, pos)?;
            let (mut a, mut b) = (Vec::new(), Vec::new());
            for v in items {
                if call1(ctx, &f, v.clone(), pos)?.truthy() {
                    a.push(v);
                } else {
                    b.push(v);
                }
            }
            Value::tuple(vec![Value::vec(a), Value::vec(b)])
        }
        "unzip" => {
            let items = drain(ctx, &it, pos)?;
            let (mut a, mut b) = (Vec::new(), Vec::new());
            for v in items {
                if let Value::Tuple(t) = v.deref_all() {
                    let t = t.borrow();
                    a.push(t.first().cloned().unwrap_or(Value::Unit));
                    b.push(t.get(1).cloned().unwrap_or(Value::Unit));
                }
            }
            Value::tuple(vec![Value::vec(a), Value::vec(b)])
        }
        "len" => Value::usize(drain(ctx, &it, pos)?.len()),
        "sorted" => {
            let mut items = drain(ctx, &it, pos)?;
            sort_values(ctx, &mut items, &mut |c, a, b| user_cmp(c, &a.deref_all(), &b.deref_all(), pos))?;
            iter_from_vec(items)
        }
        "join" => {
            let items = drain(ctx, &it, pos)?;
            let sep = if args.is_empty() { String::new() } else { str_of(&f, pos)? };
            let mut parts = Vec::new();
            for v in items {
                parts.push(super::format::display(ctx, &v, pos)?);
            }
            Value::str(parts.join(&sep))
        }
        _ => return Ok(None),
    }))
}

const ITER_METHODS: &[&str] = &[
    "map", "filter", "filter_map", "map_while", "enumerate", "zip", "chain", "skip", "take", "step_by", "take_while", "skip_while", "inspect", "flat_map", "flatten", "scan", "rev", "cycle", "collect", "count", "sum", "product", "last", "nth", "min", "max", "min_by_key",
    "max_by_key", "min_by", "max_by", "fold", "reduce", "for_each", "any", "all", "find", "find_map", "position", "partition", "unzip", "cloned", "copied", "peekable", "next", "peek", "fuse", "by_ref",
];

// ---------------------------------------------------------------- dispatch

/// Built-in method on a value; `Ok(None)` if there's no such method.
pub fn call_builtin(ctx: &mut Ctx, v: &Value, place: Option<Rc<Place>>, name: &str, generics: &[Type], args: &[Value], pos: Pos) -> Out {
    // iterator comparisons compare the yielded sequences
    if let Value::Iter(it) = v {
        if matches!(name, "eq" | "ne" | "cmp" | "partial_cmp" | "lt" | "le" | "gt" | "ge") && args.len() == 1 {
            let a = Value::vec(drain(ctx, it, pos)?);
            let b = match to_iter(ctx, &args[0], false, pos)? {
                Some(o) => Value::vec(drain(ctx, &o, pos)?),
                None => return Ok(None),
            };
            let o = user_cmp(ctx, &a, &b, pos)?;
            return Ok(Some(match name {
                "eq" => Value::Bool(user_eq(ctx, &a, &b, pos)?),
                "ne" => Value::Bool(!user_eq(ctx, &a, &b, pos)?),
                "cmp" => ctx.ordering(o),
                "partial_cmp" => {
                    let ord = ctx.ordering(o);
                    ctx.some(ord)
                }
                "lt" => Value::Bool(o == Ordering::Less),
                "le" => Value::Bool(o != Ordering::Greater),
                "gt" => Value::Bool(o == Ordering::Greater),
                _ => Value::Bool(o != Ordering::Less),
            }));
        }
    }
    if name == "unwrap_or_default" {
        let missing = match as_option(v) {
            Some(o) => o.is_none(),
            None => matches!(as_result(v), Some(Err(_))),
        };
        if missing {
            if let Some(t) = generics.first() {
                if let Some(d) = default_for_type(ctx, t, pos)? {
                    return Ok(Some(d));
                }
            }
        }
    }
    // methods every type has
    match name {
        "clone" | "to_owned" if args.is_empty() => {
            return Ok(Some(match v {
                Value::RcBox(_) => v.clone(),
                Value::Ref(p) => p.get().deep_clone(),
                _ => v.deep_clone(),
            }));
        }
        "to_string" if args.is_empty() && !matches!(v, Value::Ref(_) | Value::RcBox(_) | Value::Boxed(_)) => {
            if let Value::Iter(it) = v {
                let items = drain(ctx, it, pos)?;
                let mut s = String::new();
                for i in items {
                    s.push_str(&super::format::display(ctx, &i, pos)?);
                }
                return Ok(Some(Value::str(s)));
            }
            return Ok(Some(Value::str(super::format::display(ctx, v, pos)?)));
        }
        "fmt" if args.len() == 1 => {
            if let Value::Formatter(buf, _) = arg(args, 0).deref_all() {
                let s = super::format::display(ctx, v, pos)?;
                buf.borrow_mut().push_str(&s);
                return Ok(Some(ctx.ok(Value::Unit)));
            }
        }
        "eq" | "ne" if args.len() == 1 && !matches!(v, Value::Ref(_) | Value::Js(_)) => {
            let e = user_eq(ctx, &v.deref_all(), &arg(args, 0).deref_all(), pos)?;
            return Ok(bool_v(if name == "eq" { e } else { !e }));
        }
        "cmp" | "partial_cmp" | "total_cmp" if args.len() == 1 && !matches!(v, Value::Ref(_) | Value::Js(_)) => {
            let o = user_cmp(ctx, &v.deref_all(), &arg(args, 0).deref_all(), pos)?;
            let ord = ctx.ordering(o);
            return Ok(Some(if name == "partial_cmp" { ctx.some(ord) } else { ord }));
        }
        "into" | "as_ref" | "as_str" | "as_slice" | "as_mut_slice" | "borrow" | "as_mut" | "to_vec" | "into_boxed_slice" | "into_boxed_str" | "as_deref" | "iter_mut" | "as_mut_str"
            if args.is_empty() && !matches!(v, Value::Cell(_) | Value::Enum(_) | Value::Js(_) | Value::Ref(_) | Value::RcBox(_) | Value::Vec(_) | Value::Array(_) | Value::Map(_)) =>
        {
            if name == "into" {
                if let Some(t) = generics.first() {
                    return Ok(Some(convert_into(ctx, v, t, pos)?));
                }
            }
            return Ok(Some(v.clone()));
        }
        "hash" => return Ok(Some(Value::Unit)),
        _ => {}
    }
    match v {
        Value::Int(x, k) => int_method(ctx, *x, *k, place, name, args, pos),
        Value::Float(x, f32) => float_method(*x, *f32, name, args, pos),
        Value::Char(c) => char_method(ctx, *c, name, args, pos),
        Value::Bool(b) => Ok(match name {
            "then" => Some(if *b { let r = ctx.call_value(&arg(args, 0), vec![], pos)?; ctx.some(r) } else { ctx.none() }),
            "then_some" => Some(if *b { ctx.some(arg(args, 0)) } else { ctx.none() }),
            _ => None,
        }),
        Value::Str(s) => str_method(ctx, s, place, name, generics, args, pos),
        Value::Vec(items) | Value::Array(items) => vec_method(ctx, v, items, name, generics, args, pos),
        Value::Map(m) => map_method(ctx, m, name, generics, args, pos),
        Value::Enum(e) if e.ty.name == "Option" => option_method(ctx, v, e, place, name, args, pos),
        Value::Enum(e) if e.ty.name == "Result" => result_method(ctx, e, name, args, pos),
        Value::Enum(e) if e.ty.name == "Ordering" => {
            let o = ordering_of(v, pos)?;
            Ok(match name {
                "reverse" => Some(ctx.ordering(o.reverse())),
                "then" => Some(if o == Ordering::Equal { arg(args, 0) } else { v.clone() }),
                "then_with" => Some(if o == Ordering::Equal { ctx.call_value(&arg(args, 0), vec![], pos)? } else { v.clone() }),
                "is_lt" => bool_v(o == Ordering::Less),
                "is_le" => bool_v(o != Ordering::Greater),
                "is_gt" => bool_v(o == Ordering::Greater),
                "is_ge" => bool_v(o != Ordering::Less),
                "is_eq" => bool_v(o == Ordering::Equal),
                "is_ne" => bool_v(o != Ordering::Equal),
                _ => None,
            })
        }
        Value::Struct(s) if s.ty.name == "Entry" => entry_method(ctx, s, name, args, pos),
        Value::Cell(c) => Ok(match name {
            "borrow" | "borrow_mut" | "lock" | "read" | "write" | "get_mut" => {
                let r = Value::Ref(Rc::new(Place::Cell(c.clone())));
                Some(if matches!(name, "lock" | "read" | "write") { ctx.ok(r) } else { r })
            }
            "get" => Some(c.borrow().clone().moved()),
            "set" => {
                *c.borrow_mut() = arg(args, 0);
                Some(Value::Unit)
            }
            "replace" => Some(std::mem::replace(&mut *c.borrow_mut(), arg(args, 0))),
            "take" => {
                let old = c.borrow().clone();
                *c.borrow_mut() = default_like(ctx, &old);
                Some(old)
            }
            "into_inner" => Some(c.borrow().clone()),
            "update" => {
                let old = c.borrow().clone();
                let nv = call1(ctx, &arg(args, 0), old, pos)?;
                *c.borrow_mut() = nv.clone();
                Some(nv)
            }
            _ => None,
        }),
        Value::RcBox(_) => Ok(None),
        Value::Range(r) => {
            match name {
                "contains" => {
                    let x = arg(args, 0).deref_all();
                    let lo_ok = matches!(r.0, Value::Unit) || cmp_values(&x, &r.0) != Ordering::Less;
                    let hi_ok = match &r.1 {
                        Value::Unit => true,
                        hi => {
                            let o = cmp_values(&x, hi);
                            o == Ordering::Less || (r.2 && o == Ordering::Equal)
                        }
                    };
                    return Ok(bool_v(lo_ok && hi_ok));
                }
                "len" => {
                    let (Value::Int(a, _), Value::Int(b, _)) = (&r.0, &r.1) else { return Ok(None) };
                    return Ok(usize_v((b - a + r.2 as i128).max(0) as usize));
                }
                "is_empty" => {
                    let (Value::Int(a, _), Value::Int(b, _)) = (&r.0, &r.1) else { return Ok(None) };
                    return Ok(bool_v(if r.2 { a > b } else { a >= b }));
                }
                "start" => return Ok(Some(r.0.clone())),
                "end" => return Ok(Some(r.1.clone())),
                _ => {}
            }
            match to_iter(ctx, v, false, pos)? {
                Some(it) => iter_method(ctx, it, name, generics, args, pos),
                None => Ok(None),
            }
        }
        Value::Iter(it) => iter_method(ctx, it.clone(), name, generics, args, pos),
        Value::Tuple(_) => Ok(None),
        Value::Formatter(buf, spec) => Ok(match name {
            "write_str" | "pad" => {
                buf.borrow_mut().push_str(&str_of(&arg(args, 0), pos)?);
                Some(ctx.ok(Value::Unit))
            }
            "write_char" => {
                buf.borrow_mut().push_str(&str_of(&arg(args, 0), pos)?);
                Some(ctx.ok(Value::Unit))
            }
            "alternate" => bool_v(spec.alternate),
            "width" => Some(ctx.opt(spec.width.map(Value::usize))),
            "precision" => Some(ctx.opt(spec.precision.map(Value::usize))),
            _ => None,
        }),
        Value::Js(_) => super::web::js_method(ctx, v, name, args, pos),
        Value::Struct(_) | Value::Enum(_) => {
            // derived Default / generic helpers
            if let Some(it) = to_iter(ctx, v, false, pos)? {
                if ITER_METHODS.contains(&name) {
                    return iter_method(ctx, it, name, generics, args, pos);
                }
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn convert_into(ctx: &mut Ctx, v: &Value, t: &Type, pos: Pos) -> R<Value> {
    let n = t.name();
    if let Some(k) = IntKind::from_name(n) {
        return Ok(match v.deref_all() {
            Value::Int(x, _) => Value::Int(x, k),
            Value::Char(c) => Value::Int(c as i128, k),
            Value::Bool(b) => Value::Int(b as i128, k),
            other => other,
        });
    }
    Ok(match (n, v.deref_all()) {
        ("f64" | "f32", Value::Int(x, _)) => Value::Float(x as f64, n == "f32"),
        ("String", Value::Char(c)) => Value::str(c.to_string()),
        ("String", Value::Str(s)) => Value::str(s.borrow().clone()),
        ("String", other) => Value::str(super::format::display(ctx, &other, pos)?),
        ("char", Value::Int(x, _)) => Value::Char(char::from_u32(x as u32).unwrap_or('\u{fffd}')),
        (_, other) => other,
    })
}

/// `<T as Default>::default()` for a type known at run time.
pub fn default_for_type(ctx: &mut Ctx, t: &Type, pos: Pos) -> R<Option<Value>> {
    Ok(Some(match t {
        Type::Tuple(ts) => {
            let mut v = Vec::new();
            for t in ts {
                match default_for_type(ctx, t, pos)? {
                    Some(x) => v.push(x),
                    None => return Ok(None),
                }
            }
            if v.is_empty() { Value::Unit } else { Value::tuple(v) }
        }
        Type::Ref(_, t) => return default_for_type(ctx, t, pos),
        Type::Path(_, args) => {
            let name = t.name();
            if let Some(k) = IntKind::from_name(name) {
                return Ok(Some(Value::Int(0, k)));
            }
            let inner = |ctx: &mut Ctx| -> R<Option<Value>> {
                match args.first() {
                    Some(a) => default_for_type(ctx, a, pos),
                    None => Ok(None),
                }
            };
            match name {
                "f64" | "f32" => Value::Float(0.0, name == "f32"),
                "bool" => Value::Bool(false),
                "char" => Value::Char('\0'),
                "String" | "str" => Value::str(""),
                "Vec" | "VecDeque" => Value::vec(vec![]),
                "HashMap" | "BTreeMap" | "HashSet" | "BTreeSet" => {
                    let mut m = MapVal::new(name.starts_with("BTree"), name.ends_with("Set"));
                    if let Some(vt) = args.get(1) {
                        m.val_default = default_for_type(ctx, vt, pos)?;
                    }
                    Value::Map(Rc::new(RefCell::new(m)))
                }
                "Option" => ctx.none(),
                "Box" => return inner(ctx),
                "Rc" | "Arc" => match inner(ctx)? {
                    Some(v) => Value::RcBox(Rc::new(v)),
                    None => return Ok(None),
                },
                "RefCell" | "Cell" | "Mutex" | "RwLock" => match inner(ctx)? {
                    Some(v) => Value::Cell(Rc::new(RefCell::new(v))),
                    None => return Ok(None),
                },
                _ => {
                    let f = format!("{name}::default");
                    let code = ctx.prog.fns.borrow().get(&f).and_then(|c| c.borrow().clone());
                    match code {
                        Some(code) => ctx.call_code(&code, &[], vec![], pos)?,
                        None => return Ok(None),
                    }
                }
            }
        }
        _ => return Ok(None),
    }))
}

/// A "zero" value of the same shape (for `mem::take`, `Cell::take`, `unwrap_or_default`).
pub fn default_like(ctx: &Ctx, v: &Value) -> Value {
    match v.deref_all() {
        Value::Int(_, k) => Value::Int(0, k),
        Value::Float(_, f) => Value::Float(0.0, f),
        Value::Bool(_) => Value::Bool(false),
        Value::Char(_) => Value::Char('\0'),
        Value::Str(_) => Value::str(""),
        Value::Vec(_) => Value::vec(vec![]),
        Value::Map(m) => {
            let m = m.borrow();
            Value::Map(Rc::new(RefCell::new(MapVal::new(m.sorted, m.is_set))))
        }
        Value::Enum(e) if e.ty.name == "Option" => ctx.none(),
        _ => Value::Unit,
    }
}

fn int_method(ctx: &mut Ctx, x: i128, k: IntKind, place: Option<Rc<Place>>, name: &str, args: &[Value], pos: Pos) -> Out {
    let k = if k == IntKind::Lit { IntKind::I32 } else { k };
    let a = || int_of(&arg(args, 0), pos);
    let (lo, hi) = k.range();
    let fits = |v: i128| v >= lo && v <= hi;
    let int = |v: i128| Some(Value::Int(v, k));
    let opt = |ctx: &Ctx, v: Option<i128>| Some(ctx.opt(v.filter(|v| fits(*v)).map(|v| Value::Int(v, k))));
    let _ = place;
    Ok(match name {
        "abs" => int(x.abs()),
        "pow" => {
            let e = a()? as u32;
            match x.checked_pow(e) {
                Some(v) if fits(v) => int(v),
                _ => return panic("attempt to multiply with overflow", pos),
            }
        }
        "checked_add" => opt(ctx, x.checked_add(a()?)),
        "checked_sub" => opt(ctx, x.checked_sub(a()?)),
        "checked_mul" => opt(ctx, x.checked_mul(a()?)),
        "checked_div" => {
            let d = a()?;
            opt(ctx, if d == 0 { None } else { Some(x / d) })
        }
        "checked_rem" => {
            let d = a()?;
            opt(ctx, if d == 0 { None } else { Some(x % d) })
        }
        "checked_pow" => opt(ctx, x.checked_pow(a()? as u32)),
        "checked_neg" => opt(ctx, Some(-x)),
        "wrapping_add" => int(k.wrap(x + a()?)),
        "wrapping_sub" => int(k.wrap(x - a()?)),
        "wrapping_mul" => int(k.wrap(x.wrapping_mul(a()?))),
        "wrapping_neg" => int(k.wrap(-x)),
        "wrapping_pow" => int(k.wrap(x.wrapping_pow(a()? as u32))),
        "overflowing_add" => {
            let r = x + a()?;
            Some(Value::tuple(vec![Value::Int(k.wrap(r), k), Value::Bool(!fits(r))]))
        }
        "saturating_add" => int((x + a()?).clamp(lo, hi)),
        "saturating_sub" => int((x - a()?).clamp(lo, hi)),
        "saturating_mul" => int(x.saturating_mul(a()?).clamp(lo, hi)),
        "min" => int(x.min(a()?)),
        "max" => int(x.max(a()?)),
        "clamp" => int(x.clamp(a()?, int_of(&arg(args, 1), pos)?)),
        "signum" => int(x.signum()),
        "is_positive" => bool_v(x > 0),
        "is_negative" => bool_v(x < 0),
        "rem_euclid" => {
            let d = a()?;
            if d == 0 {
                return panic("attempt to calculate the remainder with a divisor of zero", pos);
            }
            int(x.rem_euclid(d))
        }
        "div_euclid" => {
            let d = a()?;
            if d == 0 {
                return panic("attempt to divide by zero", pos);
            }
            int(x.div_euclid(d))
        }
        "abs_diff" => Some(Value::Int((x - a()?).abs(), if k.signed() { match k { IntKind::I8 => IntKind::U8, IntKind::I16 => IntKind::U16, IntKind::I32 => IntKind::U32, IntKind::I64 => IntKind::U64, IntKind::Isize => IntKind::Usize, _ => IntKind::U128 } } else { k })),
        "count_ones" => Some(Value::Int((k.wrap(x) as u128 & mask(k)).count_ones() as i128, IntKind::U32)),
        "count_zeros" => Some(Value::Int((k.bits() - (k.wrap(x) as u128 & mask(k)).count_ones()) as i128, IntKind::U32)),
        "leading_zeros" => Some(Value::Int(((x as u128 & mask(k)).leading_zeros() - (128 - k.bits())) as i128, IntKind::U32)),
        "trailing_zeros" => Some(Value::Int(if x == 0 { k.bits() as i128 } else { (x as u128).trailing_zeros() as i128 }, IntKind::U32)),
        "is_power_of_two" => bool_v(x > 0 && (x & (x - 1)) == 0),
        "next_power_of_two" => int(if x <= 1 { 1 } else { 1i128 << (128 - ((x - 1) as u128).leading_zeros()) }),
        "isqrt" => int((x as f64).sqrt() as i128),
        "ilog2" => Some(Value::Int(127 - (x as u128).leading_zeros() as i128, IntKind::U32)),
        "ilog10" => Some(Value::Int((x as f64).log10().floor() as i128, IntKind::U32)),
        "to_le_bytes" | "to_be_bytes" => {
            let n = (k.bits() / 8) as usize;
            let mut bytes: Vec<Value> = (0..n).map(|i| Value::Int(((x >> (8 * i)) & 0xff) as i128, IntKind::U8)).collect();
            if name == "to_be_bytes" {
                bytes.reverse();
            }
            Some(Value::Array(Rc::new(RefCell::new(bytes))))
        }
        "is_ascii_digit" | "is_ascii_alphabetic" | "is_ascii_whitespace" | "is_ascii_uppercase" | "is_ascii_lowercase" | "is_ascii_alphanumeric" | "is_ascii_punctuation" | "to_ascii_uppercase" | "to_ascii_lowercase" | "is_ascii" if k == IntKind::U8 => {
            let c = x as u8;
            match name {
                "to_ascii_uppercase" => int(c.to_ascii_uppercase() as i128),
                "to_ascii_lowercase" => int(c.to_ascii_lowercase() as i128),
                _ => bool_v(match name {
                    "is_ascii_digit" => c.is_ascii_digit(),
                    "is_ascii_alphabetic" => c.is_ascii_alphabetic(),
                    "is_ascii_whitespace" => c.is_ascii_whitespace(),
                    "is_ascii_uppercase" => c.is_ascii_uppercase(),
                    "is_ascii_lowercase" => c.is_ascii_lowercase(),
                    "is_ascii_alphanumeric" => c.is_ascii_alphanumeric(),
                    "is_ascii_punctuation" => c.is_ascii_punctuation(),
                    _ => c.is_ascii(),
                }),
            }
        }
        "sqrt" | "sin" | "cos" | "floor" | "ceil" | "round" | "powi" | "powf" => return float_method(x as f64, false, name, args, pos),
        _ => None,
    })
}

fn mask(k: IntKind) -> u128 {
    if k.bits() == 128 { u128::MAX } else { (1u128 << k.bits()) - 1 }
}

fn float_method(x: f64, f32: bool, name: &str, args: &[Value], pos: Pos) -> Out {
    let fl = |v: f64| Some(Value::Float(if f32 { v as f32 as f64 } else { v }, f32));
    let a = || f64_of(&arg(args, 0), pos);
    Ok(match name {
        "sqrt" => fl(x.sqrt()),
        "cbrt" => fl(x.cbrt()),
        "abs" => fl(x.abs()),
        "powi" => fl(x.powi(int_of(&arg(args, 0), pos)? as i32)),
        "powf" => fl(x.powf(a()?)),
        "floor" => fl(x.floor()),
        "ceil" => fl(x.ceil()),
        "round" => fl(x.round()),
        "trunc" => fl(x.trunc()),
        "fract" => fl(x.fract()),
        "sin" => fl(x.sin()),
        "cos" => fl(x.cos()),
        "tan" => fl(x.tan()),
        "asin" => fl(x.asin()),
        "acos" => fl(x.acos()),
        "atan" => fl(x.atan()),
        "atan2" => fl(x.atan2(a()?)),
        "sinh" => fl(x.sinh()),
        "cosh" => fl(x.cosh()),
        "tanh" => fl(x.tanh()),
        "exp" => fl(x.exp()),
        "exp2" => fl(x.exp2()),
        "ln" => fl(x.ln()),
        "log10" => fl(x.log10()),
        "log2" => fl(x.log2()),
        "log" => fl(x.log(a()?)),
        "hypot" => fl(x.hypot(a()?)),
        "min" => fl(x.min(a()?)),
        "max" => fl(x.max(a()?)),
        "clamp" => fl(x.clamp(a()?, f64_of(&arg(args, 1), pos)?)),
        "signum" => fl(x.signum()),
        "mul_add" => fl(x.mul_add(a()?, f64_of(&arg(args, 1), pos)?)),
        "to_degrees" => fl(x.to_degrees()),
        "to_radians" => fl(x.to_radians()),
        "recip" => fl(x.recip()),
        "is_nan" => bool_v(x.is_nan()),
        "is_finite" => bool_v(x.is_finite()),
        "is_infinite" => bool_v(x.is_infinite()),
        "is_sign_negative" => bool_v(x.is_sign_negative()),
        "is_sign_positive" => bool_v(x.is_sign_positive()),
        "rem_euclid" => fl(x.rem_euclid(a()?)),
        "to_bits" => Some(Value::Int(if f32 { (x as f32).to_bits() as i128 } else { x.to_bits() as i128 }, if f32 { IntKind::U32 } else { IntKind::U64 })),
        _ => None,
    })
}

fn char_method(ctx: &mut Ctx, c: char, name: &str, args: &[Value], pos: Pos) -> Out {
    Ok(match name {
        "is_alphabetic" => bool_v(c.is_alphabetic()),
        "is_numeric" => bool_v(c.is_numeric()),
        "is_alphanumeric" => bool_v(c.is_alphanumeric()),
        "is_whitespace" => bool_v(c.is_whitespace()),
        "is_uppercase" => bool_v(c.is_uppercase()),
        "is_lowercase" => bool_v(c.is_lowercase()),
        "is_control" => bool_v(c.is_control()),
        "is_ascii" => bool_v(c.is_ascii()),
        "is_ascii_digit" => bool_v(c.is_ascii_digit()),
        "is_ascii_hexdigit" => bool_v(c.is_ascii_hexdigit()),
        "is_ascii_alphabetic" => bool_v(c.is_ascii_alphabetic()),
        "is_ascii_alphanumeric" => bool_v(c.is_ascii_alphanumeric()),
        "is_ascii_punctuation" => bool_v(c.is_ascii_punctuation()),
        "is_ascii_uppercase" => bool_v(c.is_ascii_uppercase()),
        "is_ascii_lowercase" => bool_v(c.is_ascii_lowercase()),
        "is_ascii_whitespace" => bool_v(c.is_ascii_whitespace()),
        "is_ascii_graphic" => bool_v(c.is_ascii_graphic()),
        "is_digit" => bool_v(c.is_digit(int_of(&arg(args, 0), pos)? as u32)),
        "to_digit" => {
            let r = c.to_digit(int_of(&arg(args, 0), pos)? as u32);
            Some(ctx.opt(r.map(|d| Value::Int(d as i128, IntKind::U32))))
        }
        "to_ascii_uppercase" => Some(Value::Char(c.to_ascii_uppercase())),
        "to_ascii_lowercase" => Some(Value::Char(c.to_ascii_lowercase())),
        "to_uppercase" => Some(iter_from_vec(c.to_uppercase().map(Value::Char).collect())),
        "to_lowercase" => Some(iter_from_vec(c.to_lowercase().map(Value::Char).collect())),
        "len_utf8" => usize_v(c.len_utf8()),
        "eq_ignore_ascii_case" => match arg(args, 0).deref_all() {
            Value::Char(d) => bool_v(c.eq_ignore_ascii_case(&d)),
            _ => bool_v(false),
        },
        _ => None,
    })
}

/// Split a string by a pattern: &str, char, or a char predicate closure.
fn pattern_matcher(ctx: &mut Ctx, pat: &Value, pos: Pos) -> R<Box<dyn FnMut(&mut Ctx, char) -> R<bool>>> {
    Ok(match pat.deref_all() {
        Value::Char(c) => Box::new(move |_, x| Ok(x == c)),
        Value::Func(_) => {
            let f = pat.clone();
            Box::new(move |c, x| Ok(c.call_value(&f, vec![Value::Char(x)], pos)?.truthy()))
        }
        Value::Vec(v) | Value::Array(v) => {
            let set: Vec<char> = v.borrow().iter().filter_map(|x| if let Value::Char(c) = x.deref_all() { Some(c) } else { None }).collect();
            Box::new(move |_, x| Ok(set.contains(&x)))
        }
        other => {
            let _ = ctx;
            return panic(format!("expected a char pattern, found `{}`", type_name(&other)), pos);
        }
    })
}

fn split_by(ctx: &mut Ctx, s: &str, pat: &Value, limit: Option<usize>, pos: Pos) -> R<Vec<String>> {
    if let Value::Str(p) = pat.deref_all() {
        let p = p.borrow().clone();
        return Ok(match limit {
            Some(n) => s.splitn(n, p.as_str()).map(String::from).collect(),
            None => {
                if p.is_empty() {
                    let mut v = vec![String::new()];
                    v.extend(s.chars().map(|c| c.to_string()));
                    v.push(String::new());
                    v
                } else {
                    s.split(p.as_str()).map(String::from).collect()
                }
            }
        });
    }
    let mut m = pattern_matcher(ctx, pat, pos)?;
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
        if limit.is_none_or(|n| out.len() + 1 < n) && m(ctx, ch)? {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(ch);
        }
    }
    out.push(cur);
    Ok(out)
}

fn strs(v: Vec<String>) -> Value {
    iter_from_vec(v.into_iter().map(Value::str).collect())
}

fn char_index_to_byte(s: &str, byte: usize) -> usize {
    byte.min(s.len())
}

fn parse_str(ctx: &mut Ctx, s: &str, ty: Option<&Type>) -> Value {
    let t = s.trim_end_matches('\n');
    let err = |ctx: &Ctx, msg: &str| ctx.err(Value::str(msg));
    let name = ty.map(|t| t.name()).unwrap_or("");
    if let Some(k) = IntKind::from_name(name) {
        return match t.parse::<i128>() {
            Ok(v) if v >= k.range().0 && v <= k.range().1 => ctx.ok(Value::Int(v, k)),
            Ok(_) => err(ctx, "number too large to fit in target type"),
            Err(_) if t.is_empty() => err(ctx, "cannot parse integer from empty string"),
            Err(_) => err(ctx, "invalid digit found in string"),
        };
    }
    match name {
        "f64" | "f32" => match t.parse::<f64>() {
            Ok(v) => ctx.ok(Value::Float(v, name == "f32")),
            Err(_) => err(ctx, "invalid float literal"),
        },
        "bool" => match t {
            "true" => ctx.ok(Value::Bool(true)),
            "false" => ctx.ok(Value::Bool(false)),
            _ => err(ctx, "provided string was not `true` or `false`"),
        },
        "char" => {
            let mut it = t.chars();
            match (it.next(), it.next()) {
                (Some(c), None) => ctx.ok(Value::Char(c)),
                _ => err(ctx, "too many characters in string"),
            }
        }
        "String" => ctx.ok(Value::str(t)),
        _ => {
            if let Ok(v) = t.parse::<i128>() {
                ctx.ok(Value::Int(v, IntKind::Lit))
            } else if let Ok(v) = t.parse::<f64>() {
                ctx.ok(Value::Float(v, false))
            } else {
                err(ctx, "invalid digit found in string")
            }
        }
    }
}

fn str_method(ctx: &mut Ctx, s: &Rc<RefCell<String>>, place: Option<Rc<Place>>, name: &str, generics: &[Type], args: &[Value], pos: Pos) -> Out {
    let st = s.borrow().clone();
    let a0 = arg(args, 0);
    let _ = place;
    Ok(Some(match name {
        "len" => Value::usize(st.len()),
        "is_empty" => Value::Bool(st.is_empty()),
        "chars" => iter_from_vec(st.chars().map(Value::Char).collect()),
        "bytes" | "as_bytes" | "into_bytes" => {
            let b: Vec<Value> = st.bytes().map(|b| Value::Int(b as i128, IntKind::U8)).collect();
            if name == "bytes" { iter_from_vec(b) } else { Value::vec(b) }
        }
        "char_indices" => iter_from_vec(st.char_indices().map(|(i, c)| Value::tuple(vec![Value::usize(i), Value::Char(c)])).collect()),
        "lines" => strs(st.lines().map(String::from).collect()),
        "split_whitespace" | "split_ascii_whitespace" => strs(st.split_whitespace().map(String::from).collect()),
        "split" => strs(split_by(ctx, &st, &a0, None, pos)?),
        "splitn" => strs(split_by(ctx, &st, &arg(args, 1), Some(usize_of(&a0, pos)?), pos)?),
        "rsplit" => {
            let mut v = split_by(ctx, &st, &a0, None, pos)?;
            v.reverse();
            strs(v)
        }
        "split_terminator" => {
            let mut v = split_by(ctx, &st, &a0, None, pos)?;
            if v.last().is_some_and(|l| l.is_empty()) {
                v.pop();
            }
            strs(v)
        }
        "split_once" | "rsplit_once" => {
            let p = str_of(&a0, pos)?;
            let r = if name == "split_once" { st.split_once(p.as_str()) } else { st.rsplit_once(p.as_str()) };
            ctx.opt(r.map(|(a, b)| Value::tuple(vec![Value::str(a), Value::str(b)])))
        }
        "split_at" => {
            let i = char_index_to_byte(&st, usize_of(&a0, pos)?);
            if !st.is_char_boundary(i) {
                return panic("byte index is not a char boundary", pos);
            }
            Value::tuple(vec![Value::str(&st[..i]), Value::str(&st[i..])])
        }
        "trim" => Value::str(st.trim()),
        "trim_start" | "trim_left" => Value::str(st.trim_start()),
        "trim_end" | "trim_right" => Value::str(st.trim_end()),
        "trim_matches" | "trim_start_matches" | "trim_end_matches" => {
            let out = match a0.deref_all() {
                Value::Str(p) => {
                    let p = p.borrow().clone();
                    match name {
                        "trim_matches" => st.trim_start_matches(p.as_str()).trim_end_matches(p.as_str()).to_string(),
                        "trim_start_matches" => st.trim_start_matches(p.as_str()).to_string(),
                        _ => st.trim_end_matches(p.as_str()).to_string(),
                    }
                }
                _ => {
                    let mut m = pattern_matcher(ctx, &a0, pos)?;
                    let chars: Vec<char> = st.chars().collect();
                    let (mut a, mut b) = (0, chars.len());
                    if name != "trim_end_matches" {
                        while a < b && m(ctx, chars[a])? {
                            a += 1;
                        }
                    }
                    if name != "trim_start_matches" {
                        while b > a && m(ctx, chars[b - 1])? {
                            b -= 1;
                        }
                    }
                    chars[a..b].iter().collect()
                }
            };
            Value::str(out)
        }
        "strip_prefix" | "strip_suffix" => {
            let p = str_of(&a0, pos)?;
            let r = if name == "strip_prefix" { st.strip_prefix(p.as_str()) } else { st.strip_suffix(p.as_str()) };
            ctx.opt(r.map(Value::str))
        }
        "starts_with" | "ends_with" | "contains" => match a0.deref_all() {
            Value::Str(p) => {
                let p = p.borrow();
                Value::Bool(match name {
                    "starts_with" => st.starts_with(p.as_str()),
                    "ends_with" => st.ends_with(p.as_str()),
                    _ => st.contains(p.as_str()),
                })
            }
            _ => {
                let mut m = pattern_matcher(ctx, &a0, pos)?;
                Value::Bool(match name {
                    "starts_with" => match st.chars().next() {
                        Some(c) => m(ctx, c)?,
                        None => false,
                    },
                    "ends_with" => match st.chars().last() {
                        Some(c) => m(ctx, c)?,
                        None => false,
                    },
                    _ => {
                        let mut found = false;
                        for c in st.chars() {
                            if m(ctx, c)? {
                                found = true;
                                break;
                            }
                        }
                        found
                    }
                })
            }
        },
        "find" | "rfind" => match a0.deref_all() {
            Value::Str(p) => {
                let p = p.borrow().clone();
                let r = if name == "find" { st.find(p.as_str()) } else { st.rfind(p.as_str()) };
                ctx.opt(r.map(Value::usize))
            }
            _ => {
                let mut m = pattern_matcher(ctx, &a0, pos)?;
                let mut found = None;
                let idx: Vec<(usize, char)> = st.char_indices().collect();
                let order: Box<dyn Iterator<Item = &(usize, char)>> = if name == "find" { Box::new(idx.iter()) } else { Box::new(idx.iter().rev()) };
                for (i, c) in order {
                    if m(ctx, *c)? {
                        found = Some(*i);
                        break;
                    }
                }
                ctx.opt(found.map(Value::usize))
            }
        },
        "matches" => {
            let p = str_of(&a0, pos)?;
            strs(st.matches(p.as_str()).map(String::from).collect())
        }
        "match_indices" => {
            let p = str_of(&a0, pos)?;
            iter_from_vec(st.match_indices(p.as_str()).map(|(i, m)| Value::tuple(vec![Value::usize(i), Value::str(m)])).collect())
        }
        "replace" | "replacen" => {
            let to = str_of(&arg(args, 1), pos)?;
            match a0.deref_all() {
                Value::Str(p) => {
                    let p = p.borrow().clone();
                    Value::str(if name == "replace" { st.replace(p.as_str(), &to) } else { st.replacen(p.as_str(), &to, usize_of(&arg(args, 2), pos)?) })
                }
                Value::Char(c) => Value::str(st.replace(c, &to)),
                _ => return panic("`replace` needs a string or char pattern", pos),
            }
        }
        "to_uppercase" => Value::str(st.to_uppercase()),
        "to_lowercase" => Value::str(st.to_lowercase()),
        "to_ascii_uppercase" => Value::str(st.to_ascii_uppercase()),
        "to_ascii_lowercase" => Value::str(st.to_ascii_lowercase()),
        "make_ascii_uppercase" => {
            s.borrow_mut().make_ascii_uppercase();
            Value::Unit
        }
        "make_ascii_lowercase" => {
            s.borrow_mut().make_ascii_lowercase();
            Value::Unit
        }
        "eq_ignore_ascii_case" => Value::Bool(st.eq_ignore_ascii_case(&str_of(&a0, pos)?)),
        "repeat" => Value::str(st.repeat(usize_of(&a0, pos)?)),
        "parse" => parse_str(ctx, &st, generics.first()),
        "push" => {
            match a0.deref_all() {
                Value::Char(c) => s.borrow_mut().push(c),
                other => return panic(format!("`push` expects a char, found `{}`", type_name(&other)), pos),
            }
            Value::Unit
        }
        "push_str" => {
            let add = str_of(&a0, pos)?;
            s.borrow_mut().push_str(&add);
            Value::Unit
        }
        "pop" => {
            let c = s.borrow_mut().pop();
            ctx.opt(c.map(Value::Char))
        }
        "insert" => {
            let i = usize_of(&a0, pos)?;
            let Value::Char(c) = arg(args, 1).deref_all() else { return panic("`insert` expects a char", pos) };
            if i > st.len() || !st.is_char_boundary(i) {
                return panic("insertion index is not a char boundary", pos);
            }
            s.borrow_mut().insert(i, c);
            Value::Unit
        }
        "insert_str" => {
            let i = usize_of(&a0, pos)?;
            let t = str_of(&arg(args, 1), pos)?;
            if i > st.len() || !st.is_char_boundary(i) {
                return panic("insertion index is not a char boundary", pos);
            }
            s.borrow_mut().insert_str(i, &t);
            Value::Unit
        }
        "remove" => {
            let i = usize_of(&a0, pos)?;
            if i >= st.len() || !st.is_char_boundary(i) {
                return panic("cannot remove a char from the end of a string", pos);
            }
            Value::Char(s.borrow_mut().remove(i))
        }
        "truncate" => {
            let i = usize_of(&a0, pos)?;
            if i < st.len() {
                if !st.is_char_boundary(i) {
                    return panic("new length does not lie on a char boundary", pos);
                }
                s.borrow_mut().truncate(i);
            }
            Value::Unit
        }
        "clear" => {
            s.borrow_mut().clear();
            Value::Unit
        }
        "retain" => {
            let f = a0.clone();
            let mut out = String::new();
            for c in st.chars() {
                if call1(ctx, &f, Value::Char(c), pos)?.truthy() {
                    out.push(c);
                }
            }
            *s.borrow_mut() = out;
            Value::Unit
        }
        "drain" => {
            let (a, b) = range_bounds(&a0, st.len(), pos)?;
            let removed: String = s.borrow_mut().drain(a..b).collect();
            iter_from_vec(removed.chars().map(Value::Char).collect())
        }
        "get" => {
            let (a, b) = range_bounds(&a0, st.len(), pos)?;
            ctx.opt(if a <= b && b <= st.len() && st.is_char_boundary(a) && st.is_char_boundary(b) { Some(Value::str(&st[a..b])) } else { None })
        }
        "capacity" => Value::usize(st.capacity()),
        "is_char_boundary" => Value::Bool(st.is_char_boundary(usize_of(&a0, pos)?)),
        "chars_rev" => iter_from_vec(st.chars().rev().map(Value::Char).collect()),
        "concat" | "join" => return Ok(None),
        "extend" => {
            let Some(it) = to_iter(ctx, &a0, false, pos)? else { return panic("`extend` needs an iterable", pos) };
            for v in drain(ctx, &it, pos)? {
                let add = super::format::display(ctx, &v, pos)?;
                s.borrow_mut().push_str(&add);
            }
            Value::Unit
        }
        _ => return Ok(None),
    }))
}

fn range_bounds(r: &Value, len: usize, pos: Pos) -> R<(usize, usize)> {
    match r.deref_all() {
        Value::Range(r) => {
            let a = match &r.0 {
                Value::Unit => 0,
                v => usize_of(v, pos)?,
            };
            let b = match &r.1 {
                Value::Unit => len,
                v => usize_of(v, pos)? + r.2 as usize,
            };
            Ok((a, b))
        }
        Value::Int(i, _) => Ok((i as usize, i as usize + 1)),
        other => panic(format!("expected a range, found `{}`", type_name(&other)), pos),
    }
}

fn vec_method(ctx: &mut Ctx, this: &Value, items: &Rc<RefCell<Vec<Value>>>, name: &str, generics: &[Type], args: &[Value], pos: Pos) -> Out {
    let a0 = arg(args, 0);
    let len = items.borrow().len();
    Ok(Some(match name {
        "len" => Value::usize(len),
        "is_empty" => Value::Bool(len == 0),
        "push" | "push_back" => {
            items.borrow_mut().push(a0);
            Value::Unit
        }
        "push_front" => {
            items.borrow_mut().insert(0, a0);
            Value::Unit
        }
        "pop" | "pop_back" => {
            let v = items.borrow_mut().pop();
            ctx.opt(v)
        }
        "pop_front" => {
            let v = if len == 0 { None } else { Some(items.borrow_mut().remove(0)) };
            ctx.opt(v)
        }
        "insert" => {
            let i = usize_of(&a0, pos)?;
            if i > len {
                return panic(format!("insertion index (is {i}) should be <= len (is {len})"), pos);
            }
            items.borrow_mut().insert(i, arg(args, 1));
            Value::Unit
        }
        "remove" | "swap_remove" => {
            let i = usize_of(&a0, pos)?;
            if i >= len {
                return panic(format!("removal index (is {i}) should be < len (is {len})"), pos);
            }
            let v = if name == "remove" { items.borrow_mut().remove(i) } else { items.borrow_mut().swap_remove(i) };
            if matches!(this, Value::Vec(_)) && name == "remove" && generics.is_empty() {
                // VecDeque::remove returns Option; Vec::remove returns T — callers using `.unwrap()` on Vec would fail, so keep T
            }
            v
        }
        "clear" => {
            items.borrow_mut().clear();
            Value::Unit
        }
        "truncate" => {
            items.borrow_mut().truncate(usize_of(&a0, pos)?);
            Value::Unit
        }
        "resize" => {
            let n = usize_of(&a0, pos)?;
            let fill = arg(args, 1);
            let mut v = items.borrow_mut();
            if n < v.len() {
                v.truncate(n);
            } else {
                while v.len() < n {
                    v.push(fill.deep_clone());
                }
            }
            Value::Unit
        }
        "fill" => {
            for slot in items.borrow_mut().iter_mut() {
                *slot = a0.deep_clone();
            }
            Value::Unit
        }
        "extend" | "extend_from_slice" | "append" => {
            let Some(it) = to_iter(ctx, &a0, false, pos)? else { return panic("`extend` needs an iterable", pos) };
            let new = drain(ctx, &it, pos)?;
            items.borrow_mut().extend(new);
            if name == "append" {
                if let Value::Ref(p) = &a0 {
                    if let Value::Vec(other) = p.get() {
                        other.borrow_mut().clear();
                    }
                } else if let Value::Vec(other) = a0.deref_all() {
                    other.borrow_mut().clear();
                }
            }
            Value::Unit
        }
        "iter" | "into_iter" => to_iter(ctx, this, false, pos)?.map(Value::Iter).unwrap_or(Value::Unit),
        "iter_mut" => to_iter(ctx, this, true, pos)?.map(Value::Iter).unwrap_or(Value::Unit),
        "get" => match a0.deref_all() {
            Value::Range(_) => {
                let (a, b) = range_bounds(&a0, len, pos)?;
                ctx.opt(if a <= b && b <= len { Some(Value::vec(items.borrow()[a..b].to_vec())) } else { None })
            }
            _ => {
                let i = int_of(&a0, pos)?;
                ctx.opt(if i >= 0 { items.borrow().get(i as usize).cloned() } else { None })
            }
        },
        "get_mut" => {
            let i = int_of(&a0, pos)?;
            ctx.opt(if i >= 0 && (i as usize) < len { Some(Value::Ref(Rc::new(Place::Elem(items.clone(), i as usize)))) } else { None })
        }
        "first" | "front" => {
            let v = items.borrow().first().cloned();
            ctx.opt(v)
        }
        "last" | "back" => {
            let v = items.borrow().last().cloned();
            ctx.opt(v)
        }
        "first_mut" | "front_mut" => ctx.opt(if len > 0 { Some(Value::Ref(Rc::new(Place::Elem(items.clone(), 0)))) } else { None }),
        "last_mut" | "back_mut" => ctx.opt(if len > 0 { Some(Value::Ref(Rc::new(Place::Elem(items.clone(), len - 1)))) } else { None }),
        "contains" => {
            let snapshot = items.borrow().clone();
            let mut found = false;
            for v in &snapshot {
                if user_eq(ctx, &v.deref_all(), &a0.deref_all(), pos)? {
                    found = true;
                    break;
                }
            }
            Value::Bool(found)
        }
        "sort" | "sort_unstable" => {
            let mut v = items.borrow().clone();
            sort_values(ctx, &mut v, &mut |c, a, b| user_cmp(c, &a.deref_all(), &b.deref_all(), pos))?;
            *items.borrow_mut() = v;
            Value::Unit
        }
        "sort_by" | "sort_unstable_by" => {
            let f = a0.clone();
            let mut v = items.borrow().clone();
            sort_values(ctx, &mut v, &mut |c, a, b| ordering_of(&call2(c, &f, a.clone(), b.clone(), pos)?, pos))?;
            *items.borrow_mut() = v;
            Value::Unit
        }
        "sort_by_key" | "sort_unstable_by_key" | "sort_by_cached_key" => {
            let f = a0.clone();
            let mut v = items.borrow().clone();
            let mut keyed = Vec::with_capacity(v.len());
            for x in v.drain(..) {
                let k = call1(ctx, &f, x.clone(), pos)?;
                keyed.push(Value::tuple(vec![k, x]));
            }
            sort_values(ctx, &mut keyed, &mut |c, a, b| {
                let (Value::Tuple(a), Value::Tuple(b)) = (a, b) else { return Ok(Ordering::Equal) };
                let (ka, kb) = (a.borrow()[0].clone(), b.borrow()[0].clone());
                user_cmp(c, &ka.deref_all(), &kb.deref_all(), pos)
            })?;
            *items.borrow_mut() = keyed.into_iter().map(|t| if let Value::Tuple(t) = t { t.borrow()[1].clone() } else { t }).collect();
            Value::Unit
        }
        "reverse" => {
            items.borrow_mut().reverse();
            Value::Unit
        }
        "dedup" => {
            let mut v = items.borrow().clone();
            v.dedup_by(|a, b| eq_values(a, b));
            *items.borrow_mut() = v;
            Value::Unit
        }
        "dedup_by_key" => {
            let f = a0.clone();
            let v = items.borrow().clone();
            let mut out: Vec<Value> = Vec::new();
            let mut last_key: Option<Value> = None;
            for x in v {
                let k = call1(ctx, &f, x.clone(), pos)?;
                if last_key.as_ref().is_some_and(|lk| eq_values(lk, &k)) {
                    continue;
                }
                last_key = Some(k);
                out.push(x);
            }
            *items.borrow_mut() = out;
            Value::Unit
        }
        "retain" | "retain_mut" => {
            let f = a0.clone();
            let v = items.borrow().clone();
            let mut out = Vec::new();
            for (i, x) in v.into_iter().enumerate() {
                let arg_v = if name == "retain_mut" { Value::Ref(Rc::new(Place::Elem(items.clone(), i))) } else { x.clone() };
                if call1(ctx, &f, arg_v, pos)?.truthy() {
                    out.push(items.borrow()[i].clone());
                }
            }
            *items.borrow_mut() = out;
            Value::Unit
        }
        "join" | "concat" => {
            let sep = if name == "join" { str_of(&a0, pos)? } else { String::new() };
            let snapshot = items.borrow().clone();
            if snapshot.iter().any(|x| matches!(x.deref_all(), Value::Vec(_) | Value::Array(_))) {
                // [[1,2],[3]].concat()
                let mut out = Vec::new();
                for x in snapshot {
                    if let Value::Vec(v) | Value::Array(v) = x.deref_all() {
                        out.extend(v.borrow().iter().cloned());
                    }
                }
                Value::vec(out)
            } else {
                let mut parts = Vec::new();
                for x in snapshot {
                    parts.push(super::format::display(ctx, &x, pos)?);
                }
                Value::str(parts.join(&sep))
            }
        }
        "to_vec" | "into_vec" | "as_slice" | "as_mut_slice" | "into_boxed_slice" => Value::vec(items.borrow().clone()),
        "swap" => {
            let (i, j) = (usize_of(&a0, pos)?, usize_of(&arg(args, 1), pos)?);
            if i >= len || j >= len {
                return panic(format!("index out of bounds: the len is {len}"), pos);
            }
            items.borrow_mut().swap(i, j);
            Value::Unit
        }
        "split_at" => {
            let i = usize_of(&a0, pos)?;
            if i > len {
                return panic("mid > len", pos);
            }
            let v = items.borrow();
            Value::tuple(vec![Value::vec(v[..i].to_vec()), Value::vec(v[i..].to_vec())])
        }
        "split_off" => {
            let i = usize_of(&a0, pos)?;
            if i > len {
                return panic("`at` split index out of bounds", pos);
            }
            Value::vec(items.borrow_mut().split_off(i))
        }
        "drain" => {
            let (a, b) = range_bounds(&a0, len, pos)?;
            if a > b || b > len {
                return panic("range out of bounds", pos);
            }
            let removed: Vec<Value> = items.borrow_mut().drain(a..b).collect();
            iter_from_vec(removed)
        }
        "binary_search" => {
            let v = items.borrow().clone();
            let r = v.binary_search_by(|x| cmp_values(x, &a0));
            match r {
                Ok(i) => ctx.ok(Value::usize(i)),
                Err(i) => ctx.err(Value::usize(i)),
            }
        }
        "chunks" | "chunks_exact" => {
            let n = usize_of(&a0, pos)?.max(1);
            let v = items.borrow().clone();
            let chunks: Vec<Value> = if name == "chunks" { v.chunks(n).map(|c| Value::vec(c.to_vec())).collect() } else { v.chunks_exact(n).map(|c| Value::vec(c.to_vec())).collect() };
            iter_from_vec(chunks)
        }
        "windows" => {
            let n = usize_of(&a0, pos)?.max(1);
            let v = items.borrow().clone();
            iter_from_vec(v.windows(n).map(|c| Value::vec(c.to_vec())).collect())
        }
        "starts_with" | "ends_with" => {
            let (Value::Vec(p) | Value::Array(p)) = a0.deref_all() else { return Ok(None) };
            let (v, p) = (items.borrow(), p.borrow());
            Value::Bool(p.len() <= v.len() && if name == "starts_with" { v.iter().zip(p.iter()).all(|(a, b)| eq_values(a, b)) } else { v[v.len() - p.len()..].iter().zip(p.iter()).all(|(a, b)| eq_values(a, b)) })
        }
        "repeat" => {
            let n = usize_of(&a0, pos)?;
            let v = items.borrow().clone();
            let mut out = Vec::with_capacity(v.len() * n);
            for _ in 0..n {
                out.extend(v.iter().map(|x| x.deep_clone()));
            }
            Value::vec(out)
        }
        "rotate_left" | "rotate_right" => {
            let n = usize_of(&a0, pos)?;
            if len > 0 {
                if name == "rotate_left" {
                    items.borrow_mut().rotate_left(n % len);
                } else {
                    items.borrow_mut().rotate_right(n % len);
                }
            }
            Value::Unit
        }
        "capacity" => Value::usize(items.borrow().capacity()),
        "reserve" | "shrink_to_fit" => Value::Unit,
        "make_contiguous" => Value::vec(items.borrow().clone()),
        "iter_rev" => iter_from_vec(items.borrow().iter().rev().cloned().collect()),
        _ => {
            if ITER_METHODS.contains(&name) || name == "join" {
                let it = to_iter(ctx, this, false, pos)?.unwrap();
                return iter_method(ctx, it, name, generics, args, pos);
            }
            return Ok(None);
        }
    }))
}

fn map_method(ctx: &mut Ctx, m: &Rc<RefCell<MapVal>>, name: &str, generics: &[Type], args: &[Value], pos: Pos) -> Out {
    let a0 = arg(args, 0).deref_all();
    let is_set = m.borrow().is_set;
    Ok(Some(match name {
        "len" => Value::usize(m.borrow().len()),
        "is_empty" => Value::Bool(m.borrow().len() == 0),
        "clear" => {
            m.borrow_mut().clear();
            Value::Unit
        }
        "insert" => {
            if is_set {
                let had = m.borrow().get(&a0).is_some();
                if !had {
                    m.borrow_mut().insert(a0, Value::Unit);
                }
                Value::Bool(!had)
            } else {
                let old = m.borrow_mut().insert(a0, arg(args, 1));
                ctx.opt(old)
            }
        }
        "get" => {
            let v = m.borrow().get(&a0).cloned();
            if is_set { ctx.opt(v.map(|_| a0)) } else { ctx.opt(v) }
        }
        "get_mut" => {
            let has = m.borrow().get(&a0).is_some();
            ctx.opt(if has { Some(Value::Ref(Rc::new(Place::MapVal(m.clone(), a0)))) } else { None })
        }
        "contains_key" | "contains" => Value::Bool(m.borrow().get(&a0).is_some()),
        "remove" | "take" => {
            let r = m.borrow_mut().remove(&a0);
            if is_set { Value::Bool(r.is_some()) } else { ctx.opt(r.map(|(_, v)| v)) }
        }
        "remove_entry" => {
            let r = m.borrow_mut().remove(&a0);
            ctx.opt(r.map(|(k, v)| Value::tuple(vec![k, v])))
        }
        "entry" => {
            let ty = Rc::new(TypeInfo { name: "Entry".into(), fields: vec!["map".into(), "key".into()], is_tuple: false, variants: vec![], is_enum: false, copy: false, derives: vec![] });
            Value::Struct(Rc::new(StructVal { ty, fields: RefCell::new(vec![Value::Map(m.clone()), a0]) }))
        }
        "keys" | "into_keys" => iter_from_vec(m.borrow().keys.clone()),
        "values" | "into_values" => iter_from_vec(m.borrow().vals.clone()),
        "values_mut" => {
            let keys = m.borrow().keys.clone();
            iter_from_vec(keys.into_iter().map(|k| Value::Ref(Rc::new(Place::MapVal(m.clone(), k)))).collect())
        }
        "iter" | "into_iter" | "drain" => {
            let it = to_iter(ctx, &Value::Map(m.clone()), false, pos)?.unwrap();
            if name == "drain" {
                let items = drain(ctx, &it, pos)?;
                m.borrow_mut().clear();
                iter_from_vec(items)
            } else {
                Value::Iter(it)
            }
        }
        "iter_mut" => Value::Iter(to_iter(ctx, &Value::Map(m.clone()), true, pos)?.unwrap()),
        "extend" => {
            let Some(it) = to_iter(ctx, &arg(args, 0), false, pos)? else { return panic("`extend` needs an iterable", pos) };
            for v in drain(ctx, &it, pos)? {
                if is_set {
                    m.borrow_mut().insert(v, Value::Unit);
                } else if let Value::Tuple(t) = v.deref_all() {
                    let t = t.borrow();
                    m.borrow_mut().insert(t[0].clone(), t[1].clone());
                }
            }
            Value::Unit
        }
        "retain" => {
            let f = arg(args, 0);
            let (keys, vals) = {
                let b = m.borrow();
                (b.keys.clone(), b.vals.clone())
            };
            for (k, v) in keys.into_iter().zip(vals) {
                let keep = if is_set { call1(ctx, &f, k.clone(), pos)? } else { call2(ctx, &f, k.clone(), Value::Ref(Rc::new(Place::MapVal(m.clone(), k.clone()))), pos)? };
                let _ = v;
                if !keep.truthy() {
                    m.borrow_mut().remove(&k);
                }
            }
            Value::Unit
        }
        "first_key_value" | "last_key_value" | "first" | "last" | "pop_first" | "pop_last" => {
            let b = m.borrow();
            let i = if name.contains("first") { 0 } else { b.len().wrapping_sub(1) };
            let r = if b.len() == 0 { None } else { Some((b.keys[i].clone(), b.vals[i].clone())) };
            drop(b);
            if name.starts_with("pop") {
                if let Some((k, _)) = &r {
                    m.borrow_mut().remove(k);
                }
            }
            ctx.opt(r.map(|(k, v)| if is_set { k } else { Value::tuple(vec![k, v]) }))
        }
        "union" | "intersection" | "difference" | "symmetric_difference" => {
            let Value::Map(other) = a0 else { return panic("set operation needs a set", pos) };
            let (a, b) = (m.borrow().keys.clone(), other.borrow());
            let mut out: Vec<Value> = Vec::new();
            match name {
                "union" => {
                    out.extend(a.iter().cloned());
                    out.extend(b.keys.iter().filter(|k| m.borrow().get(k).is_none()).cloned());
                }
                "intersection" => out.extend(a.iter().filter(|k| b.get(k).is_some()).cloned()),
                "difference" => out.extend(a.iter().filter(|k| b.get(k).is_none()).cloned()),
                _ => {
                    out.extend(a.iter().filter(|k| b.get(k).is_none()).cloned());
                    out.extend(b.keys.iter().filter(|k| m.borrow().get(k).is_none()).cloned());
                }
            }
            iter_from_vec(out)
        }
        "is_subset" | "is_superset" | "is_disjoint" => {
            let Value::Map(other) = a0 else { return panic("set operation needs a set", pos) };
            let (a, b) = (m.borrow(), other.borrow());
            Value::Bool(match name {
                "is_subset" => a.keys.iter().all(|k| b.get(k).is_some()),
                "is_superset" => b.keys.iter().all(|k| a.get(k).is_some()),
                _ => a.keys.iter().all(|k| b.get(k).is_none()),
            })
        }
        _ => {
            if ITER_METHODS.contains(&name) {
                let it = to_iter(ctx, &Value::Map(m.clone()), false, pos)?.unwrap();
                return iter_method(ctx, it, name, generics, args, pos);
            }
            return Ok(None);
        }
    }))
}

fn entry_method(ctx: &mut Ctx, s: &Rc<StructVal>, name: &str, args: &[Value], pos: Pos) -> Out {
    let (map, key) = {
        let f = s.fields.borrow();
        (f[0].clone(), f[1].clone())
    };
    let Value::Map(m) = map else { return Ok(None) };
    let present = m.borrow().get(&key).is_some();
    let place = || Value::Ref(Rc::new(Place::MapVal(m.clone(), key.clone())));
    Ok(Some(match name {
        "or_insert" => {
            if !present {
                m.borrow_mut().insert(key.clone(), arg(args, 0));
            }
            place()
        }
        "or_insert_with" => {
            if !present {
                let v = ctx.call_value(&arg(args, 0), vec![], pos)?;
                m.borrow_mut().insert(key.clone(), v);
            }
            place()
        }
        "or_insert_with_key" => {
            if !present {
                let v = ctx.call_value(&arg(args, 0), vec![key.clone()], pos)?;
                m.borrow_mut().insert(key.clone(), v);
            }
            place()
        }
        "or_default" => {
            if !present {
                // the default depends on the value type: the declared type, else existing values, else 0
                let (template, sample) = {
                    let b = m.borrow();
                    (b.val_default.clone(), b.vals.first().cloned())
                };
                let d = match (template, sample) {
                    (Some(t), _) => t.deep_clone(),
                    (None, Some(v)) => default_like(ctx, &v),
                    (None, None) => Value::Int(0, IntKind::Lit),
                };
                m.borrow_mut().insert(key.clone(), d);
            }
            place()
        }
        "and_modify" => {
            if present {
                ctx.call_value(&arg(args, 0), vec![place()], pos)?;
            }
            Value::Struct(s.clone())
        }
        "key" => key.clone(),
        _ => return Ok(None),
    }))
}

fn option_method(ctx: &mut Ctx, this: &Value, e: &Rc<EnumVal>, place: Option<Rc<Place>>, name: &str, args: &[Value], pos: Pos) -> Out {
    let inner = if e.variant == 1 { e.fields.borrow().first().cloned() } else { None };
    let a0 = arg(args, 0);
    Ok(Some(match name {
        "is_some" => Value::Bool(inner.is_some()),
        "is_none" => Value::Bool(inner.is_none()),
        "is_some_and" => Value::Bool(match inner {
            Some(v) => call1(ctx, &a0, v, pos)?.truthy(),
            None => false,
        }),
        "is_none_or" => Value::Bool(match inner {
            Some(v) => call1(ctx, &a0, v, pos)?.truthy(),
            None => true,
        }),
        "unwrap" => match inner {
            Some(v) => v,
            None => return panic("called `Option::unwrap()` on a `None` value", pos),
        },
        "expect" => match inner {
            Some(v) => v,
            None => return panic(str_of(&a0, pos)?, pos),
        },
        "unwrap_or" => inner.unwrap_or(a0),
        "unwrap_or_else" => match inner {
            Some(v) => v,
            None => ctx.call_value(&a0, vec![], pos)?,
        },
        "unwrap_or_default" => match inner {
            Some(v) => v,
            None => Value::Int(0, IntKind::Lit),
        },
        "map" => match inner {
            Some(v) => {
                let r = call1(ctx, &a0, v, pos)?;
                ctx.some(r)
            }
            None => ctx.none(),
        },
        "map_or" => match inner {
            Some(v) => call1(ctx, &arg(args, 1), v, pos)?,
            None => a0,
        },
        "map_or_else" => match inner {
            Some(v) => call1(ctx, &arg(args, 1), v, pos)?,
            None => ctx.call_value(&a0, vec![], pos)?,
        },
        "and_then" => match inner {
            Some(v) => call1(ctx, &a0, v, pos)?,
            None => ctx.none(),
        },
        "and" => match inner {
            Some(_) => a0,
            None => ctx.none(),
        },
        "or" => match inner {
            Some(_) => this.clone(),
            None => a0,
        },
        "or_else" => match inner {
            Some(_) => this.clone(),
            None => ctx.call_value(&a0, vec![], pos)?,
        },
        "xor" => {
            let other = as_option(&a0).flatten();
            match (inner, other) {
                (Some(v), None) | (None, Some(v)) => ctx.some(v),
                _ => ctx.none(),
            }
        }
        "filter" => match inner {
            Some(v) if call1(ctx, &a0, v.clone(), pos)?.truthy() => ctx.some(v),
            _ => ctx.none(),
        },
        "ok_or" => match inner {
            Some(v) => ctx.ok(v),
            None => ctx.err(a0),
        },
        "ok_or_else" => match inner {
            Some(v) => ctx.ok(v),
            None => {
                let e = ctx.call_value(&a0, vec![], pos)?;
                ctx.err(e)
            }
        },
        "zip" => match (inner, as_option(&a0).flatten()) {
            (Some(a), Some(b)) => ctx.some(Value::tuple(vec![a, b])),
            _ => ctx.none(),
        },
        "flatten" => inner.unwrap_or_else(|| ctx.none()),
        "cloned" | "copied" => match inner {
            Some(v) => ctx.some(v.deref_all().deep_clone()),
            None => ctx.none(),
        },
        "as_ref" | "as_deref" | "as_deref_mut" => this.clone(),
        "as_mut" => match (inner, &place) {
            (Some(_), _) => ctx.some(Value::Ref(Rc::new(Place::EnumField(e.clone(), 0)))),
            (None, _) => ctx.none(),
        },
        "iter" | "into_iter" | "iter_mut" => iter_from_vec(inner.into_iter().collect()),
        "take" => {
            let Some(p) = place else { return panic("`take` needs a mutable place", pos) };
            p.set(ctx.none());
            this.clone()
        }
        "replace" => {
            let Some(p) = place else { return panic("`replace` needs a mutable place", pos) };
            p.set(ctx.some(a0));
            this.clone()
        }
        "insert" | "get_or_insert" | "get_or_insert_with" => {
            let Some(p) = place else { return panic(format!("`{name}` needs a mutable place"), pos) };
            let cur = p.get();
            let target = match (name, as_option(&cur).flatten()) {
                ("get_or_insert", Some(_)) | ("get_or_insert_with", Some(_)) => cur,
                _ => {
                    let v = if name == "get_or_insert_with" { ctx.call_value(&a0, vec![], pos)? } else { a0 };
                    let nv = ctx.some(v);
                    p.set(nv.clone());
                    nv
                }
            };
            let Value::Enum(ne) = target else { return Ok(None) };
            Value::Ref(Rc::new(Place::EnumField(ne, 0)))
        }
        "unwrap_unchecked" => inner.unwrap_or(Value::Unit),
        "contains" => Value::Bool(inner.is_some_and(|v| eq_values(&v, &a0))),
        _ => return Ok(None),
    }))
}

fn result_method(ctx: &mut Ctx, e: &Rc<EnumVal>, name: &str, args: &[Value], pos: Pos) -> Out {
    let val = e.fields.borrow().first().cloned().unwrap_or(Value::Unit);
    let ok = e.variant == 0;
    let a0 = arg(args, 0);
    let this = Value::Enum(e.clone());
    Ok(Some(match name {
        "is_ok" => Value::Bool(ok),
        "is_err" => Value::Bool(!ok),
        "is_ok_and" => Value::Bool(ok && call1(ctx, &a0, val, pos)?.truthy()),
        "is_err_and" => Value::Bool(!ok && call1(ctx, &a0, val, pos)?.truthy()),
        "unwrap" => {
            if ok {
                val
            } else {
                let d = super::format::debug(ctx, &val, false, 0, pos)?;
                return panic(format!("called `Result::unwrap()` on an `Err` value: {d}"), pos);
            }
        }
        "expect" => {
            if ok {
                val
            } else {
                let d = super::format::debug(ctx, &val, false, 0, pos)?;
                return panic(format!("{}: {d}", str_of(&a0, pos)?), pos);
            }
        }
        "unwrap_err" | "expect_err" => {
            if ok {
                return panic("called `Result::unwrap_err()` on an `Ok` value", pos);
            }
            val
        }
        "unwrap_or" => {
            if ok {
                val
            } else {
                a0
            }
        }
        "unwrap_or_else" => {
            if ok {
                val
            } else {
                call1(ctx, &a0, val, pos)?
            }
        }
        "unwrap_or_default" => {
            if ok {
                val
            } else {
                Value::Int(0, IntKind::Lit)
            }
        }
        "map" => {
            if ok {
                let r = call1(ctx, &a0, val, pos)?;
                ctx.ok(r)
            } else {
                this
            }
        }
        "map_err" => {
            if ok {
                this
            } else {
                let r = call1(ctx, &a0, val, pos)?;
                ctx.err(r)
            }
        }
        "map_or" => {
            if ok {
                call1(ctx, &arg(args, 1), val, pos)?
            } else {
                a0
            }
        }
        "and_then" => {
            if ok {
                call1(ctx, &a0, val, pos)?
            } else {
                this
            }
        }
        "or_else" => {
            if ok {
                this
            } else {
                call1(ctx, &a0, val, pos)?
            }
        }
        "ok" => {
            if ok {
                ctx.some(val)
            } else {
                ctx.none()
            }
        }
        "err" => {
            if ok {
                ctx.none()
            } else {
                ctx.some(val)
            }
        }
        "as_ref" | "as_mut" | "as_deref" => this,
        "iter" | "into_iter" => iter_from_vec(if ok { vec![val] } else { vec![] }),
        _ => return Ok(None),
    }))
}

// ---------------------------------------------------------------- indexing

pub fn index(ctx: &mut Ctx, container: &Value, i: &Value, pos: Pos) -> R<Value> {
    let _ = ctx;
    match (container, i) {
        (Value::Vec(v) | Value::Array(v), Value::Int(n, _)) => {
            let v = v.borrow();
            if *n < 0 || *n as usize >= v.len() {
                return panic(format!("index out of bounds: the len is {} but the index is {n}", v.len()), pos);
            }
            Ok(v[*n as usize].clone())
        }
        (Value::Vec(v) | Value::Array(v), Value::Range(_)) => {
            let len = v.borrow().len();
            let (a, b) = range_bounds(i, len, pos)?;
            if a > b || b > len {
                return panic(format!("range end index {b} out of range for slice of length {len}"), pos);
            }
            Ok(Value::vec(v.borrow()[a..b].to_vec()))
        }
        (Value::Str(s), Value::Range(_)) => {
            let s = s.borrow();
            let (a, b) = range_bounds(i, s.len(), pos)?;
            if a > b || b > s.len() {
                return panic(format!("byte index {b} is out of bounds of the string"), pos);
            }
            if !s.is_char_boundary(a) || !s.is_char_boundary(b) {
                return panic("byte index is not a char boundary", pos);
            }
            Ok(Value::str(&s[a..b]))
        }
        (Value::Map(m), k) => match m.borrow().get(k) {
            Some(v) => Ok(v.clone()),
            None => panic("key not found in map", pos),
        },
        (Value::Str(_), Value::Int(..)) => panic("strings cannot be indexed by an integer; use `.chars().nth(i)` or byte slices", pos),
        (c, _) => {
            let tn = type_name(c);
            match ctx.prog.method(&tn, "index") {
                Some(code) => ctx.call_code(&code, &[], vec![c.clone(), i.clone()], pos),
                None => panic(format!("cannot index into a value of type `{tn}`"), pos),
            }
        }
    }
}

// ---------------------------------------------------------------- associated functions

const ASSOC: &[(&str, &str)] = &[
    ("String", "new"),
    ("String", "from"),
    ("String", "with_capacity"),
    ("String", "from_utf8"),
    ("String", "from_utf8_lossy"),
    ("String", "from_iter"),
    ("Vec", "new"),
    ("Vec", "with_capacity"),
    ("Vec", "from"),
    ("Vec", "from_iter"),
    ("VecDeque", "new"),
    ("VecDeque", "with_capacity"),
    ("VecDeque", "from"),
    ("HashMap", "new"),
    ("HashMap", "with_capacity"),
    ("HashMap", "from"),
    ("HashMap", "from_iter"),
    ("BTreeMap", "new"),
    ("BTreeMap", "from"),
    ("HashSet", "new"),
    ("HashSet", "with_capacity"),
    ("HashSet", "from"),
    ("HashSet", "from_iter"),
    ("BTreeSet", "new"),
    ("BTreeSet", "from"),
    ("Box", "new"),
    ("Rc", "new"),
    ("Arc", "new"),
    ("Rc", "clone"),
    ("Arc", "clone"),
    ("Rc", "ptr_eq"),
    ("Rc", "strong_count"),
    ("RefCell", "new"),
    ("Cell", "new"),
    ("Mutex", "new"),
    ("RwLock", "new"),
    ("char", "from_u32"),
    ("char", "from_digit"),
    ("char", "from"),
    ("cmp", "max"),
    ("cmp", "min"),
    ("mem", "swap"),
    ("mem", "replace"),
    ("mem", "take"),
    ("mem", "drop"),
    ("prelude", "drop"),
    ("iter", "repeat"),
    ("iter", "once"),
    ("iter", "empty"),
    ("iter", "from_fn"),
    ("iter", "successors"),
    ("iter", "zip"),
    ("process", "exit"),
    ("Instant", "now"),
    ("SystemTime", "now"),
    ("Duration", "from_millis"),
    ("Duration", "from_secs"),
    ("Duration", "from_secs_f64"),
];

pub fn has_assoc(ty: &str, name: &str) -> bool {
    if ASSOC.contains(&(ty, name)) {
        return true;
    }
    if name == "default" && matches!(ty, "String" | "Vec" | "VecDeque" | "HashMap" | "BTreeMap" | "HashSet" | "BTreeSet" | "bool" | "char" | "Option") {
        return true;
    }
    // `i32::from_str_radix`, `f64::sqrt(x)`, `u8::try_from(x)`, ...
    (IntKind::from_name(ty).is_some() || ty == "f64" || ty == "f32") && !name.chars().next().is_some_and(|c| c.is_uppercase())
}

pub fn call_assoc(ctx: &mut Ctx, ty: &str, name: &str, generics: &[Type], args: Vec<Value>, pos: Pos) -> R<Value> {
    let a0 = arg(&args, 0);
    let to_vec = |ctx: &mut Ctx, v: &Value| -> R<Vec<Value>> {
        match to_iter(ctx, v, false, pos)? {
            Some(it) => drain(ctx, &it, pos),
            None => Ok(vec![]),
        }
    };
    Ok(match (ty, name) {
        ("String", "new") | ("String", "with_capacity") => Value::str(""),
        ("String", "from") => Value::str(match a0.deref_all() {
            Value::Str(s) => s.borrow().clone(),
            Value::Char(c) => c.to_string(),
            other => super::format::display(ctx, &other, pos)?,
        }),
        ("String", "from_utf8") | ("String", "from_utf8_lossy") => {
            let bytes: Vec<u8> = to_vec(ctx, &a0)?.iter().map(|b| int_of(b, pos).unwrap_or(0) as u8).collect();
            match String::from_utf8(bytes.clone()) {
                Ok(s) => {
                    if name == "from_utf8" {
                        ctx.ok(Value::str(s))
                    } else {
                        Value::str(s)
                    }
                }
                Err(_) => {
                    if name == "from_utf8" {
                        ctx.err(Value::str("invalid utf-8 sequence"))
                    } else {
                        Value::str(String::from_utf8_lossy(&bytes))
                    }
                }
            }
        }
        ("String", "from_iter") => {
            let items = to_vec(ctx, &a0)?;
            collect(ctx, items, Some(&Type::Path(vec!["String".into()], vec![])), pos)?
        }
        ("Vec" | "VecDeque", "new" | "with_capacity") => Value::vec(vec![]),
        ("Vec" | "VecDeque", "from" | "from_iter") => match a0.deref_all() {
            Value::Str(s) => Value::vec(s.borrow().bytes().map(|b| Value::Int(b as i128, IntKind::U8)).collect()),
            _ => Value::vec(to_vec(ctx, &a0)?),
        },
        ("HashMap" | "BTreeMap" | "HashSet" | "BTreeSet", "new" | "with_capacity" | "default") => {
            let mut m = MapVal::new(ty.starts_with("BTree"), ty.ends_with("Set"));
            if let Some(vt) = generics.get(1) {
                m.val_default = default_for_type(ctx, vt, pos)?;
            }
            Value::Map(Rc::new(RefCell::new(m)))
        }
        ("HashMap" | "BTreeMap" | "HashSet" | "BTreeSet", "from" | "from_iter") => {
            let items = to_vec(ctx, &a0)?;
            collect(ctx, items, Some(&Type::Path(vec![ty.into()], generics.to_vec())), pos)?
        }
        ("String" | "Vec" | "VecDeque" | "bool" | "char" | "f64" | "f32" | "Option", "default") => match ty {
            "String" => Value::str(""),
            "Vec" | "VecDeque" => Value::vec(vec![]),
            "bool" => Value::Bool(false),
            "char" => Value::Char('\0'),
            "Option" => ctx.none(),
            _ => Value::Float(0.0, ty == "f32"),
        },
        ("Box", "new") => a0,
        ("Rc" | "Arc", "new") => Value::RcBox(Rc::new(a0)),
        ("Rc" | "Arc", "clone") => match a0 {
            Value::Ref(p) => p.get(),
            v => v,
        },
        ("Rc" | "Arc", "ptr_eq") => Value::Bool(match (a0.clone(), arg(&args, 1)) {
            (Value::RcBox(a), Value::RcBox(b)) => Rc::ptr_eq(&a, &b),
            (a, b) => a.addr() == b.addr() && a.addr() != 0,
        }),
        ("Rc" | "Arc", "strong_count") => Value::usize(match &a0 {
            // discount the copies this call itself holds (the argument list and `a0`)
            Value::RcBox(b) => Rc::strong_count(b).saturating_sub(2).max(1),
            Value::Ref(p) => match p.get() {
                Value::RcBox(b) => Rc::strong_count(&b).saturating_sub(1).max(1),
                _ => 1,
            },
            _ => 1,
        }),
        ("RefCell" | "Cell" | "Mutex" | "RwLock", "new") => Value::Cell(Rc::new(RefCell::new(a0))),
        ("char", "from_u32") => {
            let c = char::from_u32(int_of(&a0, pos)? as u32);
            ctx.opt(c.map(Value::Char))
        }
        ("char", "from_digit") => {
            let c = char::from_digit(int_of(&a0, pos)? as u32, int_of(&arg(&args, 1), pos)? as u32);
            ctx.opt(c.map(Value::Char))
        }
        ("char", "from") => Value::Char(match a0.deref_all() {
            Value::Int(x, _) => char::from_u32(x as u32).unwrap_or('\u{fffd}'),
            Value::Char(c) => c,
            _ => '\u{fffd}',
        }),
        ("cmp", "max") | ("cmp", "min") => {
            let b = arg(&args, 1);
            let o = user_cmp(ctx, &a0.deref_all(), &b.deref_all(), pos)?;
            if (name == "max") == (o == Ordering::Greater) {
                a0
            } else if o == Ordering::Equal {
                if name == "max" { b } else { a0 }
            } else {
                b
            }
        }
        ("mem", "swap") => {
            let (Value::Ref(a), Value::Ref(b)) = (a0, arg(&args, 1)) else { return panic("`mem::swap` needs two `&mut` references", pos) };
            let (va, vb) = (a.get(), b.get());
            a.set(vb);
            b.set(va);
            Value::Unit
        }
        ("mem", "replace") => {
            let Value::Ref(p) = a0 else { return panic("`mem::replace` needs a `&mut` reference", pos) };
            let old = p.get();
            p.set(arg(&args, 1));
            old
        }
        ("mem", "take") => {
            let Value::Ref(p) = a0 else { return panic("`mem::take` needs a `&mut` reference", pos) };
            let old = p.get();
            p.set(default_like(ctx, &old));
            old
        }
        ("mem" | "prelude", "drop") => Value::Unit,
        ("iter", "repeat") => new_iter(move |_| Ok(Some(a0.deep_clone()))),
        ("iter", "once") => iter_from_vec(vec![a0]),
        ("iter", "empty") => iter_from_vec(vec![]),
        ("iter", "from_fn") => {
            let f = a0;
            new_iter(move |c| Ok(as_option(&c.call_value(&f, vec![], pos)?).flatten()))
        }
        ("iter", "successors") => {
            let f = arg(&args, 1);
            let mut cur = as_option(&a0).flatten();
            new_iter(move |c| {
                let Some(v) = cur.take() else { return Ok(None) };
                cur = as_option(&c.call_value(&f, vec![v.clone()], pos)?).flatten();
                Ok(Some(v))
            })
        }
        ("iter", "zip") => {
            let Some(it) = to_iter(ctx, &a0, false, pos)? else { return panic("`zip` needs iterables", pos) };
            iter_method(ctx, it, "zip", &[], &[arg(&args, 1)], pos)?.unwrap_or(Value::Unit)
        }
        ("process", "exit") => return panic(format!("process exited with code {}", int_of(&a0, pos).unwrap_or(0)), pos),
        ("Instant" | "SystemTime", "now") => Value::Float(super::web::now_ms(ctx), false),
        ("Duration", "from_millis") => Value::Float(f64_of(&a0, pos)?, false),
        ("Duration", "from_secs") | ("Duration", "from_secs_f64") => Value::Float(f64_of(&a0, pos)? * 1000.0, false),
        (t, n) => {
            if let Some(k) = IntKind::from_name(t) {
                match n {
                    "from_str_radix" => {
                        let s = str_of(&a0, pos)?;
                        let radix = int_of(&arg(&args, 1), pos)? as u32;
                        return Ok(match i128::from_str_radix(&s, radix) {
                            Ok(v) if v >= k.range().0 && v <= k.range().1 => ctx.ok(Value::Int(v, k)),
                            _ => ctx.err(Value::str("invalid digit found in string")),
                        });
                    }
                    "from" => return Ok(convert_into(ctx, &a0, &Type::Path(vec![t.into()], vec![]), pos)?),
                    "try_from" => {
                        let v = int_of(&a0, pos)?;
                        let (lo, hi) = k.range();
                        return Ok(if v >= lo && v <= hi { ctx.ok(Value::Int(v, k)) } else { ctx.err(Value::str("out of range integral type conversion attempted")) });
                    }
                    "default" => return Ok(Value::Int(0, k)),
                    _ => {}
                }
            }
            if (t == "f64" || t == "f32") && n == "from" {
                return Ok(Value::Float(f64_of(&a0, pos)?, t == "f32"));
            }
            // UFCS on primitives: `f64::sqrt(x)`, `i32::max(a, b)`, `u32::pow(2, 3)`
            if args.is_empty() {
                return panic(format!("no function or associated item named `{n}` found for `{t}`"), pos);
            }
            let mut rest = args.clone();
            let recv = rest.remove(0);
            let recv = match (IntKind::from_name(t), recv.deref_all()) {
                (Some(k), Value::Int(x, IntKind::Lit)) => Value::Int(x, k),
                (_, Value::Int(x, _)) if t == "f64" || t == "f32" => Value::Float(x as f64, t == "f32"),
                (_, v) => v,
            };
            return call_method(ctx, recv, None, n, generics, rest, pos);
        }
    })
}
