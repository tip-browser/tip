//! ArrayBuffer, typed arrays, TextEncoder/TextDecoder.

use std::cell::RefCell;
use std::rc::Rc;

use crate::js::interp::{arg, throw, Interp, R};
use crate::js::value::*;

fn typed_of(v: &Value) -> Option<TypedKind> {
    match v {
        Value::Obj(o) => match &o.borrow().kind {
            ObjKind::TypedArray(t) => Some(t.clone()),
            _ => None,
        },
        _ => None,
    }
}

pub fn new_typed(it: &mut Interp, ty: TypedTy, data: Vec<u8>) -> Value {
    let len = data.len() / ty.size();
    let proto = it.p.typed.get(ty.name()).cloned();
    Value::Obj(Obj::new(ObjKind::TypedArray(TypedKind { buf: Rc::new(RefCell::new(data)), offset: 0, len, ty }), proto, ty.name()))
}

pub fn bytes_of(v: &Value) -> Option<Vec<u8>> {
    match v {
        Value::Obj(o) => match &o.borrow().kind {
            ObjKind::TypedArray(t) => {
                let b = t.buf.borrow();
                Some(b[t.offset..t.offset + t.len * t.ty.size()].to_vec())
            }
            ObjKind::ArrayBuffer(b) => Some(b.borrow().clone()),
            _ => None,
        },
        _ => None,
    }
}

pub fn setup(it: &mut Interp) {
    let abp = it.p.arraybuffer.clone();
    it.define_ctor("ArrayBuffer", 1, &abp, |it, this, args| {
        it.constructing = false;
        let n = it.to_length(&arg(args, 0))?;
        if n > 1 << 30 {
            throw!(it, "RangeError", "Array buffer allocation failed");
        }
        if let Value::Obj(o) = &this {
            o.borrow_mut().kind = ObjKind::ArrayBuffer(Rc::new(RefCell::new(vec![0; n])));
            o.borrow_mut().class = "ArrayBuffer";
        }
        Ok(this)
    });
    it.define_getter(&abp, "byteLength", |_, this, _| Ok(Value::Num(bytes_of(&this).map(|b| b.len()).unwrap_or(0) as f64)));
    it.define_method(&abp, "slice", 2, |it, this, args| {
        let b = bytes_of(&this).unwrap_or_default();
        let s = (it.to_integer(&arg(args, 0))?.max(0.0) as usize).min(b.len());
        let e = if arg(args, 1).is_undefined() { b.len() } else { (it.to_integer(&arg(args, 1))?.max(0.0) as usize).min(b.len()) };
        let o = Obj::new(ObjKind::ArrayBuffer(Rc::new(RefCell::new(if s < e { b[s..e].to_vec() } else { vec![] }))), Some(it.p.arraybuffer.clone()), "ArrayBuffer");
        Ok(Value::Obj(o))
    });

    let base = it.new_object();
    for ty in [TypedTy::U8, TypedTy::U8C, TypedTy::I8, TypedTy::U16, TypedTy::I16, TypedTy::U32, TypedTy::I32, TypedTy::F32, TypedTy::F64] {
        let proto = Obj::new(ObjKind::Ordinary, Some(base.clone()), ty.name());
        it.p.typed.insert(ty.name(), proto.clone());
        let c = it.define_ctor(ty.name(), 3, &proto, move |it, this, args| {
            it.constructing = false;
            let a0 = arg(args, 0);
            let kind = match &a0 {
                Value::Obj(o) if matches!(o.borrow().kind, ObjKind::ArrayBuffer(_)) => {
                    let buf = match &o.borrow().kind {
                        ObjKind::ArrayBuffer(b) => b.clone(),
                        _ => unreachable!(),
                    };
                    let off = it.to_length(&arg(args, 1))?;
                    let total = buf.borrow().len();
                    let len = if arg(args, 2).is_undefined() { total.saturating_sub(off) / ty.size() } else { it.to_length(&arg(args, 2))? };
                    if off + len * ty.size() > total {
                        throw!(it, "RangeError", "Invalid typed array length");
                    }
                    TypedKind { buf, offset: off, len, ty }
                }
                Value::Obj(_) => {
                    let items = if typed_of(&a0).is_some() { super::array_like_to_vec(it, &a0)? } else { it.iterate_to_vec(&a0).or_else(|_| super::array_like_to_vec(it, &a0))? };
                    let t = TypedKind { buf: Rc::new(RefCell::new(vec![0; items.len() * ty.size()])), offset: 0, len: items.len(), ty };
                    for (i, v) in items.iter().enumerate() {
                        let n = it.to_number(v)?;
                        t.set(i, n);
                    }
                    t
                }
                v => {
                    let n = it.to_length(v)?;
                    if n > 1 << 28 {
                        throw!(it, "RangeError", "Invalid typed array length");
                    }
                    TypedKind { buf: Rc::new(RefCell::new(vec![0; n * ty.size()])), offset: 0, len: n, ty }
                }
            };
            if let Value::Obj(o) = &this {
                o.borrow_mut().kind = ObjKind::TypedArray(kind);
                o.borrow_mut().class = ty.name();
            }
            Ok(this)
        });
        c.set_hidden("BYTES_PER_ELEMENT", Value::Num(ty.size() as f64));
        it.define_method(&c, "from", 1, move |it, _, args| {
            let items = it.iterate_to_vec(&arg(args, 0))?;
            let t = TypedKind { buf: Rc::new(RefCell::new(vec![0; items.len() * ty.size()])), offset: 0, len: items.len(), ty };
            for (i, v) in items.iter().enumerate() {
                let n = it.to_number(v)?;
                t.set(i, n);
            }
            Ok(Value::Obj(Obj::new(ObjKind::TypedArray(t), it.p.typed.get(ty.name()).cloned(), ty.name())))
        });
    }
    it.define_getter(&base, "length", |_, this, _| Ok(Value::Num(typed_of(&this).map(|t| t.len).unwrap_or(0) as f64)));
    it.define_getter(&base, "byteLength", |_, this, _| Ok(Value::Num(typed_of(&this).map(|t| t.len * t.ty.size()).unwrap_or(0) as f64)));
    it.define_getter(&base, "byteOffset", |_, this, _| Ok(Value::Num(typed_of(&this).map(|t| t.offset).unwrap_or(0) as f64)));
    it.define_getter(&base, "buffer", |it, this, _| {
        let t = typed_of(&this).ok_or_else(|| it.make_error("TypeError", "not a typed array"))?;
        Ok(Value::Obj(Obj::new(ObjKind::ArrayBuffer(t.buf.clone()), Some(it.p.arraybuffer.clone()), "ArrayBuffer")))
    });
    it.define_method(&base, "set", 2, |it, this, args| {
        let t = typed_of(&this).ok_or_else(|| it.make_error("TypeError", "not a typed array"))?;
        let items = super::array_like_to_vec(it, &arg(args, 0))?;
        let off = it.to_length(&arg(args, 1))?;
        if off + items.len() > t.len {
            throw!(it, "RangeError", "offset is out of bounds");
        }
        for (i, v) in items.iter().enumerate() {
            let n = it.to_number(v)?;
            t.set(off + i, n);
        }
        Ok(Value::Undefined)
    });
    it.define_method(&base, "subarray", 2, |it, this, args| {
        let t = typed_of(&this).ok_or_else(|| it.make_error("TypeError", "not a typed array"))?;
        let rel = |it: &mut Interp, v: &Value, d: f64| -> R<usize> {
            let n = if v.is_undefined() { d } else { it.to_integer(v)? };
            Ok(if n < 0.0 { (t.len as f64 + n).max(0.0) as usize } else { (n as usize).min(t.len) })
        };
        let s = rel(it, &arg(args, 0), 0.0)?;
        let e = rel(it, &arg(args, 1), t.len as f64)?;
        let nk = TypedKind { buf: t.buf.clone(), offset: t.offset + s * t.ty.size(), len: e.saturating_sub(s), ty: t.ty };
        Ok(Value::Obj(Obj::new(ObjKind::TypedArray(nk), it.p.typed.get(t.ty.name()).cloned(), t.ty.name())))
    });
    // Borrow the generic Array methods that work on array-likes.
    let ap = it.p.array.clone();
    for name in ["join", "indexOf", "includes", "forEach", "map", "filter", "reduce", "some", "every", "find", "findIndex", "slice", "at", "keys", "values", "entries", "fill", "reverse", "sort", "lastIndexOf"] {
        if let Some(f) = ap.get_own(name) {
            base.set_hidden(name, f);
        }
    }
    let si = it.sym_iterator.clone();
    if let Some(v) = ap.get_own("values") {
        base.borrow_mut().props.insert(PropKey::Sym(si), Prop::hidden(v));
    }

    // TextEncoder / TextDecoder
    let tep = it.new_object();
    it.define_ctor("TextEncoder", 0, &tep, |it, this, _| {
        it.constructing = false;
        Ok(this)
    });
    tep.set_hidden("encoding", Value::str("utf-8"));
    it.define_method(&tep, "encode", 1, |it, _, args| {
        let s = if arg(args, 0).is_undefined() { std::rc::Rc::from("") } else { it.to_str(&arg(args, 0))? };
        Ok(new_typed(it, TypedTy::U8, s.as_bytes().to_vec()))
    });
    let tdp = it.new_object();
    it.define_ctor("TextDecoder", 0, &tdp, |it, this, _| {
        it.constructing = false;
        Ok(this)
    });
    tdp.set_hidden("encoding", Value::str("utf-8"));
    it.define_method(&tdp, "decode", 1, |_, _, args| {
        let b = bytes_of(&arg(args, 0)).unwrap_or_default();
        Ok(Value::str(&String::from_utf8_lossy(&b)))
    });
}
