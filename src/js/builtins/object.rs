//! Object constructor and Object.prototype.

use std::rc::Rc;

use crate::js::interp::{arg, same_value_zero, strict_equals, throw, Interp, R};
use crate::js::value::*;

pub fn define_property(it: &mut Interp, target: &Value, key: &Value, desc: &Value) -> R<()> {
    let Value::Obj(o) = target else { throw!(it, "TypeError", "Object.defineProperty called on non-object") };
    if let Some(p) = o.proxy() {
        let (h, trap) = it.proxy_trap(&p, "defineProperty")?;
        return match trap {
            Some(t) => it.call(&t, Value::Obj(h), &[Value::Obj(p.target.clone()), key.clone(), desc.clone()]).map(|_| ()),
            None => define_property(it, &Value::Obj(p.target.clone()), key, desc),
        };
    }
    let Value::Obj(_) = desc else { throw!(it, "TypeError", "Property description must be an object") };
    let k = it.to_key(key)?;
    let get = it.get_str(desc, "get")?;
    let set = it.get_str(desc, "set")?;
    let has = |it: &mut Interp, n: &str| -> bool {
        let Value::Obj(d) = desc else { return false };
        it.has_property(d, &PropKey::from(n))
    };
    let existing = o.borrow().props.get(&k).cloned();
    let enumerable = if has(it, "enumerable") { it.get_str(desc, "enumerable")?.truthy() } else { existing.as_ref().is_some_and(|p| p.enumerable) };
    let configurable = if has(it, "configurable") { it.get_str(desc, "configurable")?.truthy() } else { existing.as_ref().is_some_and(|p| p.configurable) };
    if !get.is_undefined() || !set.is_undefined() {
        let prev_g = existing.as_ref().and_then(|p| p.getter.clone());
        let prev_s = existing.as_ref().and_then(|p| p.setter.clone());
        let p = Prop {
            value: Value::Undefined,
            getter: if get.is_undefined() { prev_g } else { Some(get) },
            setter: if set.is_undefined() { prev_s } else { Some(set) },
            enumerable,
            writable: true,
            configurable,
        };
        o.borrow_mut().props.insert(k, p);
        return Ok(());
    }
    let value = if has(it, "value") { it.get_str(desc, "value")? } else { existing.as_ref().map(|p| p.value.clone()).unwrap_or(Value::Undefined) };
    let writable = if has(it, "writable") { it.get_str(desc, "writable")?.truthy() } else { existing.as_ref().is_some_and(|p| p.writable) };
    // array index/length writes go through the exotic path
    let is_array = o.is_array();
    if is_array && (k.array_index().is_some() || k.as_str() == Some("length")) {
        return it.set(target, k, value);
    }
    o.borrow_mut().props.insert(k, Prop { value, getter: None, setter: None, enumerable, writable, configurable });
    Ok(())
}

fn descriptor(it: &mut Interp, o: &Obj, k: &PropKey) -> R<Value> {
    if let Some(p) = o.proxy() {
        let (h, trap) = it.proxy_trap(&p, "getOwnPropertyDescriptor")?;
        return match trap {
            Some(t) => it.call(&t, Value::Obj(h), &[Value::Obj(p.target.clone()), k.to_value()]),
            None => descriptor(it, &p.target, k),
        };
    }
    let p = o.borrow().props.get(k).cloned();
    let d = it.new_object();
    match p {
        Some(p) => {
            if p.is_accessor() {
                d.set_prop("get", p.getter.unwrap_or(Value::Undefined));
                d.set_prop("set", p.setter.unwrap_or(Value::Undefined));
            } else {
                d.set_prop("value", p.value);
                d.set_prop("writable", Value::Bool(p.writable));
            }
            d.set_prop("enumerable", Value::Bool(p.enumerable));
            d.set_prop("configurable", Value::Bool(p.configurable));
        }
        None => {
            if !it.has_own(o, k) {
                return Ok(Value::Undefined);
            }
            let v = it.get_from(o, k, &Value::Obj(o.clone()))?;
            d.set_prop("value", v);
            d.set_prop("writable", Value::Bool(true));
            d.set_prop("enumerable", Value::Bool(true));
            d.set_prop("configurable", Value::Bool(true));
        }
    }
    Ok(Value::Obj(d))
}

pub fn setup(it: &mut Interp) {
    let op = it.p.object.clone();
    let ctor = it.define_ctor("Object", 1, &op, |it, this, args| {
        let v = arg(args, 0);
        if v.is_nullish() {
            if it.constructing {
                it.constructing = false;
                return Ok(this);
            }
            return Ok(Value::Obj(it.new_object()));
        }
        Ok(Value::Obj(it.to_object(&v)?))
    });
    it.define_method(&ctor, "keys", 1, |it, _, args| {
        let o = it.to_object(&arg(args, 0))?;
        let keys: Vec<Value> = it.own_keys(&o, false).into_iter().map(|k| k.to_value()).collect();
        Ok(it.new_array(keys))
    });
    it.define_method(&ctor, "values", 1, |it, _, args| {
        let v = arg(args, 0);
        let o = it.to_object(&v)?;
        let ov = Value::Obj(o.clone());
        let mut out = Vec::new();
        for k in it.own_keys(&o, false) {
            out.push(it.get(&ov, &k)?);
        }
        Ok(it.new_array(out))
    });
    it.define_method(&ctor, "entries", 1, |it, _, args| {
        let o = it.to_object(&arg(args, 0))?;
        let ov = Value::Obj(o.clone());
        let mut out = Vec::new();
        for k in it.own_keys(&o, false) {
            let v = it.get(&ov, &k)?;
            out.push(it.new_array(vec![k.to_value(), v]));
        }
        Ok(it.new_array(out))
    });
    it.define_method(&ctor, "assign", 2, |it, _, args| {
        let target = arg(args, 0);
        let t = it.to_object(&target)?;
        let tv = Value::Obj(t);
        for src in args.iter().skip(1) {
            if src.is_nullish() {
                continue;
            }
            let so = it.to_object(src)?;
            let sv = Value::Obj(so.clone());
            for k in it.own_keys(&so, false) {
                let v = it.get(&sv, &k)?;
                it.set(&tv, k, v)?;
            }
            let syms: Vec<PropKey> = so.borrow().props.iter().filter(|(k, p)| matches!(k, PropKey::Sym(_)) && p.enumerable).map(|(k, _)| k.clone()).collect();
            for k in syms {
                let v = it.get(&sv, &k)?;
                it.set(&tv, k, v)?;
            }
        }
        Ok(tv)
    });
    it.define_method(&ctor, "create", 2, |it, _, args| {
        let proto = match arg(args, 0) {
            Value::Obj(p) => Some(p),
            Value::Null => None,
            _ => throw!(it, "TypeError", "Object prototype may only be an Object or null"),
        };
        let o = Obj::new(ObjKind::Ordinary, proto, "Object");
        let props = arg(args, 1);
        if let Value::Obj(po) = &props {
            for k in it.own_keys(po, false) {
                let d = it.get(&props, &k)?;
                define_property(it, &Value::Obj(o.clone()), &k.to_value(), &d)?;
            }
        }
        Ok(Value::Obj(o))
    });
    it.define_method(&ctor, "getPrototypeOf", 1, |it, _, args| {
        let v = arg(args, 0);
        let o = it.to_object(&v)?;
        Ok(o.proto().map(Value::Obj).unwrap_or(Value::Null))
    });
    it.define_method(&ctor, "setPrototypeOf", 2, |_, _, args| {
        let v = arg(args, 0);
        if let Value::Obj(o) = &v {
            o.borrow_mut().proto = arg(args, 1).as_obj().cloned();
        }
        Ok(v)
    });
    it.define_method(&ctor, "defineProperty", 3, |it, _, args| {
        let t = arg(args, 0);
        define_property(it, &t, &arg(args, 1), &arg(args, 2))?;
        Ok(t)
    });
    it.define_method(&ctor, "defineProperties", 2, |it, _, args| {
        let t = arg(args, 0);
        let props = arg(args, 1);
        let po = it.to_object(&props)?;
        for k in it.own_keys(&po, false) {
            let d = it.get(&props, &k)?;
            define_property(it, &t, &k.to_value(), &d)?;
        }
        Ok(t)
    });
    it.define_method(&ctor, "getOwnPropertyNames", 1, |it, _, args| {
        let o = it.to_object(&arg(args, 0))?;
        let mut keys: Vec<Value> = it.own_keys(&o, true).into_iter().map(|k| k.to_value()).collect();
        if o.is_array() {
            keys.push(Value::str("length"));
        }
        Ok(it.new_array(keys))
    });
    it.define_method(&ctor, "getOwnPropertySymbols", 1, |it, _, args| {
        let o = it.to_object(&arg(args, 0))?;
        let keys: Vec<Value> = o.borrow().props.iter().filter(|(k, _)| matches!(k, PropKey::Sym(_))).map(|(k, _)| k.to_value()).collect();
        Ok(it.new_array(keys))
    });
    it.define_method(&ctor, "getOwnPropertyDescriptor", 2, |it, _, args| {
        let o = it.to_object(&arg(args, 0))?;
        let k = it.to_key(&arg(args, 1))?;
        descriptor(it, &o, &k)
    });
    it.define_method(&ctor, "getOwnPropertyDescriptors", 1, |it, _, args| {
        let o = it.to_object(&arg(args, 0))?;
        let out = it.new_object();
        for k in it.own_keys(&o, true) {
            let d = descriptor(it, &o, &k)?;
            out.borrow_mut().props.insert(k, Prop::data(d));
        }
        Ok(Value::Obj(out))
    });
    for (name, frozen) in [("freeze", true), ("seal", false), ("preventExtensions", false)] {
        it.define_method(&ctor, name, 1, move |_, _, args| {
            let v = arg(args, 0);
            if let Value::Obj(o) = &v {
                let mut b = o.borrow_mut();
                b.extensible = false;
                if frozen {
                    b.frozen = true;
                    let keys = b.props.keys();
                    for k in keys {
                        if let Some(p) = b.props.get_mut(&k) {
                            p.writable = false;
                            p.configurable = false;
                        }
                    }
                }
            }
            Ok(v)
        });
    }
    it.define_method(&ctor, "isFrozen", 1, |_, _, args| Ok(Value::Bool(match arg(args, 0) {
        Value::Obj(o) => o.borrow().frozen,
        _ => true,
    })));
    it.define_method(&ctor, "isSealed", 1, |_, _, args| Ok(Value::Bool(match arg(args, 0) {
        Value::Obj(o) => !o.borrow().extensible,
        _ => true,
    })));
    it.define_method(&ctor, "isExtensible", 1, |_, _, args| Ok(Value::Bool(match arg(args, 0) {
        Value::Obj(o) => o.borrow().extensible,
        _ => false,
    })));
    it.define_method(&ctor, "is", 2, |_, _, args| {
        let (a, b) = (arg(args, 0), arg(args, 1));
        Ok(Value::Bool(match (&a, &b) {
            (Value::Num(x), Value::Num(y)) => {
                if x.is_nan() && y.is_nan() {
                    true
                } else {
                    x == y && x.is_sign_negative() == y.is_sign_negative()
                }
            }
            _ => strict_equals(&a, &b),
        }))
    });
    it.define_method(&ctor, "fromEntries", 1, |it, _, args| {
        let o = it.new_object();
        let ov = Value::Obj(o.clone());
        for e in it.iterate_to_vec(&arg(args, 0))? {
            let k = it.get_str(&e, "0")?;
            let v = it.get_str(&e, "1")?;
            let k = it.to_key(&k)?;
            it.set(&ov, k, v)?;
        }
        Ok(ov)
    });
    it.define_method(&ctor, "hasOwn", 2, |it, _, args| {
        let o = it.to_object(&arg(args, 0))?;
        let k = it.to_key(&arg(args, 1))?;
        Ok(Value::Bool(it.has_own(&o, &k)))
    });
    it.define_method(&ctor, "groupBy", 2, |it, _, args| {
        let items = it.iterate_to_vec(&arg(args, 0))?;
        let f = arg(args, 1);
        let out = it.new_object();
        for (i, v) in items.into_iter().enumerate() {
            let k = it.call(&f, Value::Undefined, &[v.clone(), Value::Num(i as f64)])?;
            let k = it.to_key(&k)?;
            let existing = out.borrow().props.get(&k).map(|p| p.value.clone());
            match existing {
                Some(Value::Obj(arr)) => {
                    if let ObjKind::Array(a) = &mut arr.borrow_mut().kind {
                        a.push(v);
                    }
                }
                _ => {
                    let arr = it.new_array(vec![v]);
                    out.borrow_mut().props.insert(k, Prop::data(arr));
                }
            }
        }
        Ok(Value::Obj(out))
    });

    // prototype
    it.define_method(&op, "hasOwnProperty", 1, |it, this, args| {
        let o = it.to_object(&this)?;
        let k = it.to_key(&arg(args, 0))?;
        Ok(Value::Bool(it.has_own(&o, &k)))
    });
    it.define_method(&op, "isPrototypeOf", 1, |_, this, args| {
        let (Value::Obj(p), Value::Obj(o)) = (&this, &arg(args, 0)) else { return Ok(Value::Bool(false)) };
        let mut cur = o.proto();
        while let Some(c) = cur {
            if c.ptr_eq(p) {
                return Ok(Value::Bool(true));
            }
            cur = c.proto();
        }
        Ok(Value::Bool(false))
    });
    it.define_method(&op, "propertyIsEnumerable", 1, |it, this, args| {
        let o = it.to_object(&this)?;
        let k = it.to_key(&arg(args, 0))?;
        let e = o.borrow().props.get(&k).map(|p| p.enumerable);
        Ok(Value::Bool(e.unwrap_or_else(|| k.array_index().is_some() && it.has_own(&o, &k))))
    });
    it.define_method(&op, "toString", 0, |it, this, _| {
        let tag = match &this {
            Value::Undefined => "Undefined".to_string(),
            Value::Null => "Null".to_string(),
            v => {
                let o = it.to_object(v)?;
                let t = it.get(v, &PropKey::Sym(it.sym_to_string_tag.clone()))?;
                if let Value::Str(s) = t {
                    s.to_string()
                } else {
                    let b = o.borrow();
                    match &b.kind {
                        ObjKind::Array(_) => "Array".into(),
                        ObjKind::Function(_) => "Function".into(),
                        ObjKind::Error => "Error".into(),
                        ObjKind::Bool(_) => "Boolean".into(),
                        ObjKind::Num(_) => "Number".into(),
                        ObjKind::Str(_) => "String".into(),
                        ObjKind::Date(_) => "Date".into(),
                        ObjKind::RegExp(_) => "RegExp".into(),
                        ObjKind::Arguments => "Arguments".into(),
                        ObjKind::Host(h) => h.class_name().into(),
                        _ => {
                            if b.class == "Arguments" {
                                "Arguments".into()
                            } else {
                                "Object".into()
                            }
                        }
                    }
                }
            }
        };
        Ok(Value::str(&format!("[object {tag}]")))
    });
    it.define_method(&op, "toLocaleString", 0, |it, this, _| {
        let f = it.get_str(&this, "toString")?;
        it.call(&f, this, &[])
    });
    it.define_method(&op, "valueOf", 0, |it, this, _| Ok(Value::Obj(it.to_object(&this)?)));
    it.define_accessor(
        &op,
        "__proto__",
        |it, this, _| {
            let o = it.to_object(&this)?;
            Ok(o.proto().map(Value::Obj).unwrap_or(Value::Null))
        },
        |_, this, args| {
            if let Value::Obj(o) = &this {
                match arg(args, 0) {
                    Value::Obj(p) => o.borrow_mut().proto = Some(p),
                    Value::Null => o.borrow_mut().proto = None,
                    _ => {}
                }
            }
            Ok(Value::Undefined)
        },
    );
    if let Some(p) = op.borrow_mut().props.get_mut(&PropKey::from("__proto__")) {
        p.enumerable = false;
    }
    let _ = (same_value_zero, Rc::<str>::from(""));
}
