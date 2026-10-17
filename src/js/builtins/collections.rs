//! Map, Set, WeakMap, WeakSet, WeakRef.

use crate::js::interp::{arg, throw, Interp, R};
use crate::js::value::*;

fn with_map<T>(it: &mut Interp, this: &Value, set: bool, f: impl FnOnce(&mut MapData) -> T) -> R<T> {
    if let Value::Obj(o) = this {
        let mut b = o.borrow_mut();
        match (&mut b.kind, set) {
            (ObjKind::Map(m), false) | (ObjKind::Set(m), true) => return Ok(f(m)),
            _ => {}
        }
    }
    throw!(it, "TypeError", "Method {}.prototype called on incompatible receiver", if set { "Set" } else { "Map" })
}

fn setup_one(it: &mut Interp, name: &'static str, proto: Obj, is_set: bool, weak: bool) {
    let ctor = it.define_ctor(name, 0, &proto, move |it, this, args| {
        if !it.constructing {
            throw!(it, "TypeError", "Constructor {name} requires 'new'");
        }
        it.constructing = false;
        let Value::Obj(o) = &this else { unreachable!() };
        o.borrow_mut().kind = if is_set { ObjKind::Set(MapData::new()) } else { ObjKind::Map(MapData::new()) };
        o.borrow_mut().class = name;
        let init = arg(args, 0);
        if !init.is_nullish() {
            let adder = it.get_str(&this, if is_set { "add" } else { "set" })?;
            for item in it.iterate_to_vec(&init)? {
                if is_set {
                    it.call(&adder, this.clone(), &[item])?;
                } else {
                    let k = it.get_str(&item, "0")?;
                    let v = it.get_str(&item, "1")?;
                    it.call(&adder, this.clone(), &[k, v])?;
                }
            }
        }
        Ok(this)
    });
    if !is_set && !weak {
        it.define_method(&ctor, "groupBy", 2, |it, _, args| {
            let items = it.iterate_to_vec(&arg(args, 0))?;
            let f = arg(args, 1);
            let mut m = MapData::new();
            for (i, v) in items.into_iter().enumerate() {
                let k = it.call(&f, Value::Undefined, &[v.clone(), Value::Num(i as f64)])?;
                match m.get(&k).cloned() {
                    Some(Value::Obj(arr)) => {
                        if let ObjKind::Array(a) = &mut arr.borrow_mut().kind {
                            a.push(v);
                        }
                    }
                    _ => {
                        let arr = it.new_array(vec![v]);
                        m.set(k, arr);
                    }
                }
            }
            Ok(Value::Obj(Obj::new(ObjKind::Map(m), Some(it.p.map.clone()), "Map")))
        });
    }
    let check_weak = move |it: &mut Interp, k: &Value| -> R<()> {
        if weak && !matches!(k, Value::Obj(_) | Value::Sym(_)) {
            throw!(it, "TypeError", "Invalid value used as weak {} key", if is_set { "set" } else { "map" });
        }
        Ok(())
    };
    if is_set {
        it.define_method(&proto, "add", 1, move |it, this, args| {
            let k = arg(args, 0);
            check_weak(it, &k)?;
            with_map(it, &this, true, |m| {
                if !m.has(&k) {
                    m.set(k, Value::Undefined)
                }
            })?;
            Ok(this)
        });
    } else {
        it.define_method(&proto, "get", 1, |it, this, args| {
            let k = arg(args, 0);
            Ok(with_map(it, &this, false, |m| m.get(&k).cloned())?.unwrap_or(Value::Undefined))
        });
        it.define_method(&proto, "set", 2, move |it, this, args| {
            let k = arg(args, 0);
            check_weak(it, &k)?;
            let k = if let Value::Num(n) = &k { if *n == 0.0 { Value::Num(0.0) } else { k } } else { k };
            let v = arg(args, 1);
            with_map(it, &this, false, |m| m.set(k, v))?;
            Ok(this)
        });
    }
    it.define_method(&proto, "has", 1, move |it, this, args| {
        let k = arg(args, 0);
        Ok(Value::Bool(with_map(it, &this, is_set, |m| m.has(&k))?))
    });
    it.define_method(&proto, "delete", 1, move |it, this, args| {
        let k = arg(args, 0);
        Ok(Value::Bool(with_map(it, &this, is_set, |m| m.delete(&k))?))
    });
    if weak {
        return;
    }
    it.define_method(&proto, "clear", 0, move |it, this, _| {
        with_map(it, &this, is_set, |m| m.clear())?;
        Ok(Value::Undefined)
    });
    it.define_getter(&proto, "size", move |it, this, _| Ok(Value::Num(with_map(it, &this, is_set, |m| m.len())? as f64)));
    it.define_method(&proto, "forEach", 1, move |it, this, args| {
        let f = arg(args, 0);
        let t = arg(args, 1);
        let mut i = 0;
        loop {
            let entry = with_map(it, &this, is_set, |m| {
                while i < m.entries.len() && m.entries[i].is_none() {
                    i += 1;
                }
                m.entries.get(i).cloned().flatten()
            })?;
            let Some((k, v)) = entry else { break };
            i += 1;
            if is_set {
                it.call(&f, t.clone(), &[k.clone(), k, this.clone()])?;
            } else {
                it.call(&f, t.clone(), &[v, k, this.clone()])?;
            }
        }
        Ok(Value::Undefined)
    });
    let kinds: &[(&str, u8)] = if is_set { &[("values", 0), ("keys", 0), ("entries", 3)] } else { &[("keys", 0), ("values", 1), ("entries", 2)] };
    for &(n, kind) in kinds {
        it.define_method(&proto, n, 0, move |it, this, _| {
            with_map(it, &this, is_set, |_| ())?;
            let Value::Obj(o) = &this else { unreachable!() };
            // kind 3 for sets: [v, v]
            let k = if kind == 3 { 2 } else { kind };
            let iter = Obj::new(ObjKind::Iterator(IterKind::Map { map: o.clone(), index: 0, kind: k }), Some(it.p.iterator.clone()), if is_set { "Set Iterator" } else { "Map Iterator" });
            if kind == 3 {
                // set entries produce [value, value]; emulate via a list snapshot
                let entries: Vec<Value> = match &o.borrow().kind {
                    ObjKind::Set(m) => m.entries.iter().flatten().map(|(k, _)| k.clone()).collect(),
                    _ => vec![],
                };
                let pairs: Vec<Value> = entries.into_iter().map(|k| it.new_array(vec![k.clone(), k])).collect();
                return Ok(super::make_list_iterator(it, pairs));
            }
            Ok(Value::Obj(iter))
        });
    }
    let si = it.sym_iterator.clone();
    let main = proto.get_own(if is_set { "values" } else { "entries" }).unwrap();
    proto.borrow_mut().props.insert(PropKey::Sym(si), Prop::hidden(main));
    let tag = it.sym_to_string_tag.clone();
    proto.borrow_mut().props.insert(PropKey::Sym(tag), Prop::hidden(Value::str(name)));
}

pub fn setup(it: &mut Interp) {
    let (m, s, wm, ws) = (it.p.map.clone(), it.p.set.clone(), it.p.weakmap.clone(), it.p.weakset.clone());
    setup_one(it, "Map", m, false, false);
    setup_one(it, "Set", s, true, false);
    setup_one(it, "WeakMap", wm, false, true);
    setup_one(it, "WeakSet", ws, true, true);

    let wrp = it.new_object();
    it.define_ctor("WeakRef", 1, &wrp, |it, this, args| {
        it.constructing = false;
        if let Value::Obj(o) = &this {
            o.set_hidden("__target", arg(args, 0));
        }
        Ok(this)
    });
    it.define_method(&wrp, "deref", 0, |it, this, _| it.get_str(&this, "__target"));
    let frp = it.new_object();
    it.define_ctor("FinalizationRegistry", 1, &frp, |it, this, _| {
        it.constructing = false;
        Ok(this)
    });
    it.define_method(&frp, "register", 2, |_, _, _| Ok(Value::Undefined));
    it.define_method(&frp, "unregister", 1, |_, _, _| Ok(Value::Bool(false)));
}
