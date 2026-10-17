//! Array constructor and prototype.

use std::rc::Rc;

use super::array_like_to_vec;
use crate::js::interp::{arg, same_value_zero, strict_equals, throw, Interp, R};
use crate::js::value::*;

fn items_of(it: &mut Interp, this: &Value) -> R<Vec<Value>> {
    if let Value::Obj(o) = this {
        if let ObjKind::Array(items) = &o.borrow().kind {
            return Ok(items.clone());
        }
    }
    if this.is_nullish() {
        throw!(it, "TypeError", "Array.prototype method called on null or undefined");
    }
    array_like_to_vec(it, this)
}

fn with_vec<T>(it: &mut Interp, this: &Value, f: impl FnOnce(&mut Vec<Value>) -> T) -> R<T> {
    if let Value::Obj(o) = this {
        let mut b = o.borrow_mut();
        let frozen = b.frozen;
        if let ObjKind::Array(items) = &mut b.kind {
            if frozen {
                drop(b);
                throw!(it, "TypeError", "Cannot modify a frozen array");
            }
            return Ok(f(items));
        }
    }
    // generic array-like: materialize, mutate, write back
    let mut v = array_like_to_vec(it, this)?;
    let r = f(&mut v);
    for (i, x) in v.iter().enumerate() {
        it.set(this, PropKey::Str(Rc::from(i.to_string().as_str())), x.clone())?;
    }
    it.set_str(this, "length", Value::Num(v.len() as f64))?;
    Ok(r)
}

fn rel_index(it: &mut Interp, v: &Value, len: usize, default: f64) -> R<usize> {
    let n = if v.is_undefined() { default } else { it.to_integer(v)? };
    let l = len as f64;
    Ok(if n < 0.0 { (l + n).max(0.0) as usize } else { n.min(l) as usize })
}

fn callback(it: &mut Interp, args: &[Value]) -> R<Value> {
    let f = arg(args, 0);
    if !f.is_callable() {
        throw!(it, "TypeError", "{} is not a function", it.short_desc(&f));
    }
    Ok(f)
}

/// Stable merge sort with a fallible comparator.
pub fn sort_values(it: &mut Interp, items: &mut [Value], cmp: &Value) -> R<()> {
    let n = items.len();
    if n < 2 {
        return Ok(());
    }
    // returns true if a should come after b
    let greater = |it: &mut Interp, a: &Value, b: &Value| -> R<bool> {
        if a.is_undefined() {
            return Ok(!b.is_undefined());
        }
        if b.is_undefined() {
            return Ok(false);
        }
        if cmp.is_undefined() {
            let sa = it.to_str(a)?;
            let sb = it.to_str(b)?;
            if sa.is_ascii() && sb.is_ascii() {
                return Ok(sa.as_bytes() > sb.as_bytes());
            }
            let ua: Vec<u16> = sa.encode_utf16().collect();
            let ub: Vec<u16> = sb.encode_utf16().collect();
            Ok(ua > ub)
        } else {
            let r = it.call(cmp, Value::Undefined, &[a.clone(), b.clone()])?;
            let n = it.to_number(&r)?;
            Ok(n > 0.0)
        }
    };
    let mut src: Vec<Value> = items.to_vec();
    let mut dst: Vec<Value> = items.to_vec();
    let mut width = 1;
    while width < n {
        let mut i = 0;
        while i < n {
            let mid = (i + width).min(n);
            let end = (i + 2 * width).min(n);
            let (mut l, mut r, mut k) = (i, mid, i);
            while l < mid && r < end {
                if greater(it, &src[l], &src[r])? {
                    dst[k] = src[r].clone();
                    r += 1;
                } else {
                    dst[k] = src[l].clone();
                    l += 1;
                }
                k += 1;
            }
            while l < mid {
                dst[k] = src[l].clone();
                l += 1;
                k += 1;
            }
            while r < end {
                dst[k] = src[r].clone();
                r += 1;
                k += 1;
            }
            i += 2 * width;
        }
        std::mem::swap(&mut src, &mut dst);
        width *= 2;
    }
    items.clone_from_slice(&src);
    Ok(())
}

fn flatten(it: &mut Interp, items: &[Value], depth: f64, out: &mut Vec<Value>) -> R<()> {
    for v in items {
        if depth >= 1.0 {
            if let Value::Obj(o) = v {
                let inner = if let ObjKind::Array(a) = &o.borrow().kind { Some(a.clone()) } else { None };
                if let Some(inner) = inner {
                    flatten(it, &inner, depth - 1.0, out)?;
                    continue;
                }
            }
        }
        out.push(v.clone());
    }
    Ok(())
}

pub fn join(it: &mut Interp, items: &[Value], sep: &str) -> R<String> {
    let mut s = String::new();
    for (i, v) in items.iter().enumerate() {
        if i > 0 {
            s.push_str(sep);
        }
        if !v.is_nullish() {
            s.push_str(&it.to_str(v)?);
        }
    }
    Ok(s)
}

pub fn setup(it: &mut Interp) {
    let ap = it.p.array.clone();
    let ctor = it.define_ctor("Array", 1, &ap, |it, this, args| {
        let items = if args.len() == 1 {
            if let Value::Num(n) = args[0] {
                if n < 0.0 || n.fract() != 0.0 || n > 4294967295.0 {
                    throw!(it, "RangeError", "Invalid array length");
                }
                vec![Value::Undefined; n as usize]
            } else {
                args.to_vec()
            }
        } else {
            args.to_vec()
        };
        if it.constructing {
            it.constructing = false;
            if let Value::Obj(o) = &this {
                o.borrow_mut().kind = ObjKind::Array(items);
                o.borrow_mut().class = "Array";
                return Ok(this);
            }
        }
        Ok(it.new_array(items))
    });
    it.define_method(&ctor, "isArray", 1, |_, _, args| Ok(Value::Bool(matches!(arg(args, 0), Value::Obj(o) if o.is_array()))));
    it.define_method(&ctor, "of", 0, |it, _, args| Ok(it.new_array(args.to_vec())));
    it.define_method(&ctor, "from", 1, |it, _, args| {
        let src = arg(args, 0);
        let map = arg(args, 1);
        if src.is_nullish() {
            throw!(it, "TypeError", "Array.from requires an array-like or iterable object");
        }
        let has_iter = !matches!(src, Value::Obj(_)) || !it.get(&src, &PropKey::Sym(it.sym_iterator.clone()))?.is_undefined();
        let mut items = if has_iter && !matches!(src, Value::Num(_) | Value::Bool(_)) { it.iterate_to_vec(&src)? } else { array_like_to_vec(it, &src)? };
        if map.is_callable() {
            for (i, v) in items.iter_mut().enumerate() {
                *v = it.call(&map, Value::Undefined, &[v.clone(), Value::Num(i as f64)])?;
            }
        }
        Ok(it.new_array(items))
    });

    it.define_method(&ap, "push", 1, |it, this, args| {
        let a = args.to_vec();
        let n = with_vec(it, &this, |v| {
            v.extend(a);
            v.len()
        })?;
        Ok(Value::Num(n as f64))
    });
    it.define_method(&ap, "pop", 0, |it, this, _| Ok(with_vec(it, &this, |v| v.pop())?.unwrap_or(Value::Undefined)));
    it.define_method(&ap, "shift", 0, |it, this, _| {
        Ok(with_vec(it, &this, |v| if v.is_empty() { None } else { Some(v.remove(0)) })?.unwrap_or(Value::Undefined))
    });
    it.define_method(&ap, "unshift", 1, |it, this, args| {
        let a = args.to_vec();
        let n = with_vec(it, &this, |v| {
            v.splice(0..0, a);
            v.len()
        })?;
        Ok(Value::Num(n as f64))
    });
    it.define_method(&ap, "slice", 2, |it, this, args| {
        let items = items_of(it, &this)?;
        let s = rel_index(it, &arg(args, 0), items.len(), 0.0)?;
        let e = rel_index(it, &arg(args, 1), items.len(), items.len() as f64)?;
        Ok(it.new_array(if s < e { items[s..e].to_vec() } else { vec![] }))
    });
    it.define_method(&ap, "splice", 2, |it, this, args| {
        let len = items_of(it, &this)?.len();
        let s = rel_index(it, &arg(args, 0), len, 0.0)?;
        let dc = if args.is_empty() {
            0
        } else if args.len() < 2 {
            len - s
        } else {
            (it.to_integer(&args[1])?.max(0.0) as usize).min(len - s)
        };
        let ins: Vec<Value> = args.iter().skip(2).cloned().collect();
        let removed = with_vec(it, &this, |v| v.splice(s..s + dc, ins).collect::<Vec<_>>())?;
        Ok(it.new_array(removed))
    });
    it.define_method(&ap, "concat", 1, |it, this, args| {
        let mut out = items_of(it, &this)?;
        for a in args {
            match a {
                Value::Obj(o) if o.is_array() => {
                    if let ObjKind::Array(items) = &o.borrow().kind {
                        out.extend(items.iter().cloned());
                    }
                }
                v => out.push(v.clone()),
            }
        }
        Ok(it.new_array(out))
    });
    it.define_method(&ap, "join", 1, |it, this, args| {
        let items = items_of(it, &this)?;
        let sep = match arg(args, 0) {
            Value::Undefined => ",".to_string(),
            v => it.to_str(&v)?.to_string(),
        };
        Ok(Value::str(&join(it, &items, &sep)?))
    });
    it.define_method(&ap, "toString", 0, |it, this, _| {
        let items = items_of(it, &this)?;
        Ok(Value::str(&join(it, &items, ",")?))
    });
    it.define_method(&ap, "toLocaleString", 0, |it, this, _| {
        let items = items_of(it, &this)?;
        Ok(Value::str(&join(it, &items, ",")?))
    });
    it.define_method(&ap, "reverse", 0, |it, this, _| {
        with_vec(it, &this, |v| v.reverse())?;
        Ok(this)
    });
    it.define_method(&ap, "indexOf", 1, |it, this, args| {
        let items = items_of(it, &this)?;
        let t = arg(args, 0);
        let from = rel_index(it, &arg(args, 1), items.len(), 0.0)?;
        Ok(Value::Num(items.iter().enumerate().skip(from).find(|(_, v)| strict_equals(v, &t)).map(|(i, _)| i as f64).unwrap_or(-1.0)))
    });
    it.define_method(&ap, "lastIndexOf", 1, |it, this, args| {
        let items = items_of(it, &this)?;
        let t = arg(args, 0);
        let from = if args.len() > 1 { rel_index(it, &args[1], items.len(), 0.0)? as isize } else { items.len() as isize - 1 };
        let mut i = from.min(items.len() as isize - 1);
        while i >= 0 {
            if strict_equals(&items[i as usize], &t) {
                return Ok(Value::Num(i as f64));
            }
            i -= 1;
        }
        Ok(Value::Num(-1.0))
    });
    it.define_method(&ap, "includes", 1, |it, this, args| {
        let items = items_of(it, &this)?;
        let t = arg(args, 0);
        let from = rel_index(it, &arg(args, 1), items.len(), 0.0)?;
        Ok(Value::Bool(items.iter().skip(from).any(|v| same_value_zero(v, &t))))
    });
    for (name, from_end, want_index) in [("find", false, false), ("findIndex", false, true), ("findLast", true, false), ("findLastIndex", true, true)] {
        it.define_method(&ap, name, 1, move |it, this, args| {
            let items = items_of(it, &this)?;
            let f = callback(it, args)?;
            let t = arg(args, 1);
            let order: Vec<usize> = if from_end { (0..items.len()).rev().collect() } else { (0..items.len()).collect() };
            for i in order {
                let r = it.call(&f, t.clone(), &[items[i].clone(), Value::Num(i as f64), this.clone()])?;
                if r.truthy() {
                    return Ok(if want_index { Value::Num(i as f64) } else { items[i].clone() });
                }
            }
            Ok(if want_index { Value::Num(-1.0) } else { Value::Undefined })
        });
    }
    it.define_method(&ap, "filter", 1, |it, this, args| {
        let items = items_of(it, &this)?;
        let f = callback(it, args)?;
        let t = arg(args, 1);
        let mut out = Vec::new();
        for (i, v) in items.iter().enumerate() {
            if it.call(&f, t.clone(), &[v.clone(), Value::Num(i as f64), this.clone()])?.truthy() {
                out.push(v.clone());
            }
        }
        Ok(it.new_array(out))
    });
    it.define_method(&ap, "map", 1, |it, this, args| {
        let items = items_of(it, &this)?;
        let f = callback(it, args)?;
        let t = arg(args, 1);
        let mut out = Vec::with_capacity(items.len());
        for (i, v) in items.iter().enumerate() {
            out.push(it.call(&f, t.clone(), &[v.clone(), Value::Num(i as f64), this.clone()])?);
        }
        Ok(it.new_array(out))
    });
    it.define_method(&ap, "forEach", 1, |it, this, args| {
        let items = items_of(it, &this)?;
        let f = callback(it, args)?;
        let t = arg(args, 1);
        for (i, v) in items.iter().enumerate() {
            it.call(&f, t.clone(), &[v.clone(), Value::Num(i as f64), this.clone()])?;
        }
        Ok(Value::Undefined)
    });
    for (name, rev) in [("reduce", false), ("reduceRight", true)] {
        it.define_method(&ap, name, 1, move |it, this, args| {
            let items = items_of(it, &this)?;
            let f = callback(it, args)?;
            let order: Vec<usize> = if rev { (0..items.len()).rev().collect() } else { (0..items.len()).collect() };
            let mut iter = order.into_iter();
            let mut acc = if args.len() > 1 {
                args[1].clone()
            } else {
                match iter.next() {
                    Some(i) => items[i].clone(),
                    None => throw!(it, "TypeError", "Reduce of empty array with no initial value"),
                }
            };
            for i in iter {
                acc = it.call(&f, Value::Undefined, &[acc, items[i].clone(), Value::Num(i as f64), this.clone()])?;
            }
            Ok(acc)
        });
    }
    it.define_method(&ap, "some", 1, |it, this, args| {
        let items = items_of(it, &this)?;
        let f = callback(it, args)?;
        let t = arg(args, 1);
        for (i, v) in items.iter().enumerate() {
            if it.call(&f, t.clone(), &[v.clone(), Value::Num(i as f64), this.clone()])?.truthy() {
                return Ok(Value::Bool(true));
            }
        }
        Ok(Value::Bool(false))
    });
    it.define_method(&ap, "every", 1, |it, this, args| {
        let items = items_of(it, &this)?;
        let f = callback(it, args)?;
        let t = arg(args, 1);
        for (i, v) in items.iter().enumerate() {
            if !it.call(&f, t.clone(), &[v.clone(), Value::Num(i as f64), this.clone()])?.truthy() {
                return Ok(Value::Bool(false));
            }
        }
        Ok(Value::Bool(true))
    });
    it.define_method(&ap, "sort", 1, |it, this, args| {
        let cmp = arg(args, 0);
        if !cmp.is_undefined() && !cmp.is_callable() {
            throw!(it, "TypeError", "The comparison function must be either a function or undefined");
        }
        let mut items = items_of(it, &this)?;
        sort_values(it, &mut items, &cmp)?;
        with_vec(it, &this, |v| *v = items)?;
        Ok(this)
    });
    it.define_method(&ap, "toSorted", 1, |it, this, args| {
        let mut items = items_of(it, &this)?;
        sort_values(it, &mut items, &arg(args, 0))?;
        Ok(it.new_array(items))
    });
    it.define_method(&ap, "toReversed", 0, |it, this, _| {
        let mut items = items_of(it, &this)?;
        items.reverse();
        Ok(it.new_array(items))
    });
    it.define_method(&ap, "toSpliced", 2, |it, this, args| {
        let mut items = items_of(it, &this)?;
        let len = items.len();
        let s = rel_index(it, &arg(args, 0), len, 0.0)?;
        let dc = if args.len() < 2 { len - s } else { (it.to_integer(&args[1])?.max(0.0) as usize).min(len - s) };
        items.splice(s..s + dc, args.iter().skip(2).cloned());
        Ok(it.new_array(items))
    });
    it.define_method(&ap, "with", 2, |it, this, args| {
        let mut items = items_of(it, &this)?;
        let i = it.to_integer(&arg(args, 0))?;
        let idx = if i < 0.0 { items.len() as f64 + i } else { i };
        if idx < 0.0 || idx as usize >= items.len() {
            throw!(it, "RangeError", "Invalid index");
        }
        items[idx as usize] = arg(args, 1);
        Ok(it.new_array(items))
    });
    it.define_method(&ap, "flat", 0, |it, this, args| {
        let items = items_of(it, &this)?;
        let d = if arg(args, 0).is_undefined() { 1.0 } else { it.to_integer(&arg(args, 0))? };
        let mut out = Vec::new();
        flatten(it, &items, d, &mut out)?;
        Ok(it.new_array(out))
    });
    it.define_method(&ap, "flatMap", 1, |it, this, args| {
        let items = items_of(it, &this)?;
        let f = callback(it, args)?;
        let mut mapped = Vec::new();
        for (i, v) in items.iter().enumerate() {
            mapped.push(it.call(&f, arg(args, 1), &[v.clone(), Value::Num(i as f64), this.clone()])?);
        }
        let mut out = Vec::new();
        flatten(it, &mapped, 1.0, &mut out)?;
        Ok(it.new_array(out))
    });
    it.define_method(&ap, "fill", 1, |it, this, args| {
        let v = arg(args, 0);
        let len = items_of(it, &this)?.len();
        let s = rel_index(it, &arg(args, 1), len, 0.0)?;
        let e = rel_index(it, &arg(args, 2), len, len as f64)?;
        with_vec(it, &this, |items| {
            for x in items.iter_mut().take(e).skip(s) {
                *x = v.clone();
            }
        })?;
        Ok(this)
    });
    it.define_method(&ap, "copyWithin", 2, |it, this, args| {
        let len = items_of(it, &this)?.len();
        let t = rel_index(it, &arg(args, 0), len, 0.0)?;
        let s = rel_index(it, &arg(args, 1), len, 0.0)?;
        let e = rel_index(it, &arg(args, 2), len, len as f64)?;
        with_vec(it, &this, |items| {
            if s < e {
                let chunk: Vec<Value> = items[s..e].to_vec();
                for (i, v) in chunk.into_iter().enumerate() {
                    if t + i < items.len() {
                        items[t + i] = v;
                    }
                }
            }
        })?;
        Ok(this)
    });
    it.define_method(&ap, "at", 1, |it, this, args| {
        let items = items_of(it, &this)?;
        let i = it.to_integer(&arg(args, 0))?;
        let idx = if i < 0.0 { items.len() as f64 + i } else { i };
        Ok(if idx >= 0.0 { items.get(idx as usize).cloned().unwrap_or(Value::Undefined) } else { Value::Undefined })
    });
    for (name, kind) in [("keys", 0u8), ("values", 1u8), ("entries", 2u8)] {
        it.define_method(&ap, name, 0, move |it, this, _| {
            Ok(Value::Obj(Obj::new(ObjKind::Iterator(IterKind::Array { target: this, index: 0, kind }), Some(it.p.iterator.clone()), "Array Iterator")))
        });
    }
    let values = ap.get_own("values").unwrap();
    let si = it.sym_iterator.clone();
    ap.borrow_mut().props.insert(PropKey::Sym(si), Prop::hidden(values));
}
