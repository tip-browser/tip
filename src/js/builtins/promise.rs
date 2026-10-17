//! Promises.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::js::interp::{arg, throw, Interp, Job, R};
use crate::js::value::*;

fn new_promise_obj(it: &Interp, proto: Option<Obj>) -> Obj {
    Obj::new(
        ObjKind::Promise(PromiseState { status: PromiseStatus::Pending, value: Value::Undefined, reactions: vec![], handled: false }),
        Some(proto.unwrap_or_else(|| it.p.promise.clone())),
        "Promise",
    )
}

pub fn state_of(p: &Value) -> Option<(PromiseStatus, Value)> {
    match p {
        Value::Obj(o) => match &o.borrow().kind {
            ObjKind::Promise(s) => Some((s.status, s.value.clone())),
            _ => None,
        },
        _ => None,
    }
}

pub fn is_handled(p: &Value) -> bool {
    match p {
        Value::Obj(o) => match &o.borrow().kind {
            ObjKind::Promise(s) => s.handled,
            _ => true,
        },
        _ => true,
    }
}

pub fn mark_handled(p: &Value) {
    if let Value::Obj(o) = p {
        if let ObjKind::Promise(s) = &mut o.borrow_mut().kind {
            s.handled = true;
        }
    }
}

fn settle(it: &mut Interp, p: &Obj, status: PromiseStatus, value: Value) {
    let reactions = {
        let mut b = p.borrow_mut();
        let ObjKind::Promise(s) = &mut b.kind else { return };
        if s.status != PromiseStatus::Pending {
            return;
        }
        s.status = status;
        s.value = value.clone();
        std::mem::take(&mut s.reactions)
    };
    if status == PromiseStatus::Rejected && reactions.is_empty() {
        let handled = matches!(&p.borrow().kind, ObjKind::Promise(s) if s.handled);
        if !handled {
            it.unhandled.push(Value::Obj(p.clone()));
        }
    }
    for (onf, onr, derived) in reactions {
        enqueue_reaction(it, status, value.clone(), onf, onr, derived);
    }
}

fn enqueue_reaction(it: &mut Interp, status: PromiseStatus, value: Value, onf: Option<Value>, onr: Option<Value>, derived: Obj) {
    let handler = if status == PromiseStatus::Fulfilled { onf } else { onr };
    it.enqueue(Job::Native(Box::new(move |it: &mut Interp| {
        match handler {
            Some(h) if h.is_callable() => match it.call(&h, Value::Undefined, &[value]) {
                Ok(v) => resolve_promise(it, &derived, v),
                Err(e) => settle(it, &derived, PromiseStatus::Rejected, e),
            },
            _ => {
                if status == PromiseStatus::Fulfilled {
                    resolve_promise(it, &derived, value)
                } else {
                    settle(it, &derived, PromiseStatus::Rejected, value)
                }
            }
        }
        Ok(())
    })));
}

/// The [[Resolve]] algorithm: adopt thenables, otherwise fulfill.
pub fn resolve_promise(it: &mut Interp, p: &Obj, v: Value) {
    if let Value::Obj(o) = &v {
        if o.ptr_eq(p) {
            let e = it.make_error("TypeError", "Chaining cycle detected for promise");
            return settle(it, p, PromiseStatus::Rejected, e);
        }
        // fast path for native promises
        let native_state = match &o.borrow().kind {
            ObjKind::Promise(_) => true,
            _ => false,
        };
        let then = match it.get_str(&v, "then") {
            Ok(t) => t,
            Err(e) => return settle(it, p, PromiseStatus::Rejected, e),
        };
        if then.is_callable() {
            let _ = native_state;
            let target = p.clone();
            let thenable = v.clone();
            it.enqueue(Job::Native(Box::new(move |it: &mut Interp| {
                let (res, rej) = resolving_functions(it, &target);
                if let Err(e) = it.call(&then, thenable, &[res, rej.clone()]) {
                    it.call(&rej, Value::Undefined, &[e])?;
                }
                Ok(())
            })));
            return;
        }
    }
    settle(it, p, PromiseStatus::Fulfilled, v)
}

fn resolving_functions(it: &mut Interp, p: &Obj) -> (Value, Value) {
    let done = Rc::new(Cell::new(false));
    let (p1, d1) = (p.clone(), done.clone());
    let res = it.native("", 1, move |it, _, args| {
        if !d1.replace(true) {
            resolve_promise(it, &p1, arg(args, 0));
        }
        Ok(Value::Undefined)
    });
    let (p2, d2) = (p.clone(), done);
    let rej = it.native("", 1, move |it, _, args| {
        if !d2.replace(true) {
            settle(it, &p2, PromiseStatus::Rejected, arg(args, 0));
        }
        Ok(Value::Undefined)
    });
    (res, rej)
}

/// A new pending promise with its resolve/reject functions.
pub fn new_capability(it: &mut Interp) -> (Value, Value, Value) {
    let p = new_promise_obj(it, None);
    let (res, rej) = resolving_functions(it, &p);
    (Value::Obj(p), res, rej)
}

pub fn promise_resolve(it: &mut Interp, v: Value) -> R<Value> {
    if let Value::Obj(o) = &v {
        if matches!(o.borrow().kind, ObjKind::Promise(_)) {
            return Ok(v);
        }
    }
    let p = new_promise_obj(it, None);
    resolve_promise(it, &p, v);
    Ok(Value::Obj(p))
}

pub fn then_internal(it: &mut Interp, p: &Value, onf: Option<Value>, onr: Option<Value>) -> Value {
    let derived = new_promise_obj(it, None);
    let Value::Obj(po) = p else { return Value::Obj(derived) };
    let now = {
        let mut b = po.borrow_mut();
        let ObjKind::Promise(s) = &mut b.kind else { return Value::Obj(derived) };
        s.handled = true;
        if s.status == PromiseStatus::Pending {
            s.reactions.push((onf.clone(), onr.clone(), derived.clone()));
            None
        } else {
            Some((s.status, s.value.clone()))
        }
    };
    if let Some((status, value)) = now {
        enqueue_reaction(it, status, value, onf, onr, derived.clone());
    }
    Value::Obj(derived)
}

fn this_promise(it: &mut Interp, v: &Value) -> R<Value> {
    if let Value::Obj(o) = v {
        if matches!(o.borrow().kind, ObjKind::Promise(_)) {
            return Ok(v.clone());
        }
    }
    throw!(it, "TypeError", "Method Promise.prototype.then called on incompatible receiver")
}

pub fn setup(it: &mut Interp) {
    let pp = it.p.promise.clone();
    let tag = it.sym_to_string_tag.clone();
    pp.borrow_mut().props.insert(PropKey::Sym(tag), Prop::hidden(Value::str("Promise")));
    let ctor = it.define_ctor("Promise", 1, &pp, |it, this, args| {
        if !it.constructing {
            throw!(it, "TypeError", "Promise constructor cannot be invoked without 'new'");
        }
        it.constructing = false;
        let exec = arg(args, 0);
        if !exec.is_callable() {
            throw!(it, "TypeError", "Promise resolver {} is not a function", it.short_desc(&exec));
        }
        let Value::Obj(o) = &this else { unreachable!() };
        o.borrow_mut().kind = ObjKind::Promise(PromiseState { status: PromiseStatus::Pending, value: Value::Undefined, reactions: vec![], handled: false });
        o.borrow_mut().class = "Promise";
        let (res, rej) = resolving_functions(it, o);
        if let Err(e) = it.call(&exec, Value::Undefined, &[res, rej.clone()]) {
            it.call(&rej, Value::Undefined, &[e])?;
        }
        Ok(this)
    });
    it.define_method(&pp, "then", 2, |it, this, args| {
        let p = this_promise(it, &this)?;
        let onf = arg(args, 0);
        let onr = arg(args, 1);
        Ok(then_internal(it, &p, if onf.is_callable() { Some(onf) } else { None }, if onr.is_callable() { Some(onr) } else { None }))
    });
    it.define_method(&pp, "catch", 1, |it, this, args| {
        let then = it.get_str(&this, "then")?;
        it.call(&then, this, &[Value::Undefined, arg(args, 0)])
    });
    it.define_method(&pp, "finally", 1, |it, this, args| {
        let f = arg(args, 0);
        if !f.is_callable() {
            let then = it.get_str(&this, "then")?;
            return it.call(&then, this, &[f.clone(), f]);
        }
        let f1 = f.clone();
        let onf = it.native("", 1, move |it, _, a| {
            let v = arg(a, 0);
            let r = it.call(&f1, Value::Undefined, &[])?;
            let p = promise_resolve(it, r)?;
            let ret = it.native("", 0, move |_, _, _| Ok(v.clone()));
            Ok(then_internal(it, &p, Some(ret), None))
        });
        let f2 = f;
        let onr = it.native("", 1, move |it, _, a| {
            let e = arg(a, 0);
            let r = it.call(&f2, Value::Undefined, &[])?;
            let p = promise_resolve(it, r)?;
            let thrower = it.native("", 0, move |_, _, _| Err(e.clone()));
            Ok(then_internal(it, &p, Some(thrower), None))
        });
        let then = it.get_str(&this, "then")?;
        it.call(&then, this, &[onf, onr])
    });
    it.define_method(&ctor, "resolve", 1, |it, _, args| promise_resolve(it, arg(args, 0)));
    it.define_method(&ctor, "reject", 1, |it, _, args| {
        let p = new_promise_obj(it, None);
        settle(it, &p, PromiseStatus::Rejected, arg(args, 0));
        Ok(Value::Obj(p))
    });
    it.define_method(&ctor, "withResolvers", 0, |it, _, _| {
        let (p, res, rej) = new_capability(it);
        let o = it.new_object();
        o.set_prop("promise", p);
        o.set_prop("resolve", res);
        o.set_prop("reject", rej);
        Ok(Value::Obj(o))
    });
    // combinators
    for kind in ["all", "allSettled", "race", "any"] {
        it.define_method(&ctor, kind, 1, move |it, _, args| {
            let items = it.iterate_to_vec(&arg(args, 0))?;
            let (result, res, rej) = new_capability(it);
            let n = items.len();
            let values = Rc::new(RefCell::new(vec![Value::Undefined; n]));
            let remaining = Rc::new(Cell::new(n));
            if n == 0 {
                match kind {
                    "all" | "allSettled" => {
                        let a = it.new_array(vec![]);
                        it.call(&res, Value::Undefined, &[a])?;
                    }
                    "any" => {
                        let errs = it.new_array(vec![]);
                        let e = it.make_error("AggregateError", "All promises were rejected");
                        if let Value::Obj(o) = &e {
                            o.set_hidden("errors", errs);
                        }
                        it.call(&rej, Value::Undefined, &[e])?;
                    }
                    _ => {}
                }
                return Ok(result);
            }
            for (i, item) in items.into_iter().enumerate() {
                let p = promise_resolve(it, item)?;
                let (vals, rem, res1, _rej1) = (values.clone(), remaining.clone(), res.clone(), rej.clone());
                let onf = it.native("", 1, move |it, _, a| {
                    let v = arg(a, 0);
                    match kind {
                        "race" => {
                            it.call(&res1, Value::Undefined, &[v])?;
                        }
                        "any" => {
                            it.call(&res1, Value::Undefined, &[v])?;
                        }
                        _ => {
                            let stored = if kind == "allSettled" {
                                let o = it.new_object();
                                o.set_prop("status", Value::str("fulfilled"));
                                o.set_prop("value", v);
                                Value::Obj(o)
                            } else {
                                v
                            };
                            vals.borrow_mut()[i] = stored;
                            rem.set(rem.get() - 1);
                            if rem.get() == 0 {
                                let a = it.new_array(vals.borrow().clone());
                                it.call(&res1, Value::Undefined, &[a])?;
                            }
                        }
                    }
                    Ok(Value::Undefined)
                });
                let (vals, rem, res2, rej2) = (values.clone(), remaining.clone(), res.clone(), rej.clone());
                let onr = it.native("", 1, move |it, _, a| {
                    let e = arg(a, 0);
                    match kind {
                        "all" | "race" => {
                            it.call(&rej2, Value::Undefined, &[e])?;
                        }
                        "allSettled" => {
                            let o = it.new_object();
                            o.set_prop("status", Value::str("rejected"));
                            o.set_prop("reason", e);
                            vals.borrow_mut()[i] = Value::Obj(o);
                            rem.set(rem.get() - 1);
                            if rem.get() == 0 {
                                let a = it.new_array(vals.borrow().clone());
                                it.call(&res2, Value::Undefined, &[a])?;
                            }
                        }
                        _ => {
                            vals.borrow_mut()[i] = e;
                            rem.set(rem.get() - 1);
                            if rem.get() == 0 {
                                let errs = it.new_array(vals.borrow().clone());
                                let err = it.make_error("AggregateError", "All promises were rejected");
                                if let Value::Obj(o) = &err {
                                    o.set_hidden("errors", errs);
                                }
                                it.call(&rej2, Value::Undefined, &[err])?;
                            }
                        }
                    }
                    Ok(Value::Undefined)
                });
                let then = it.get_str(&p, "then")?;
                it.call(&then, p, &[onf, onr])?;
            }
            Ok(result)
        });
    }
}
