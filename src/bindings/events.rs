//! DOM events: Event objects, listeners and dispatch.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::{node_of, ph, wrap, DomProtos};
use crate::dom::NodeId;
use crate::js::interp::{arg, throw, Interp, R};
use crate::js::value::*;
use crate::page::{Listener, Page, Target};

const F_PREVENTED: u8 = 1;
const F_STOP: u8 = 2;
const F_STOP_IMMEDIATE: u8 = 4;

pub struct EventState {
    pub ty: RefCell<String>,
    pub bubbles: Cell<bool>,
    pub cancelable: Cell<bool>,
    pub flags: Cell<u8>,
    pub phase: Cell<u8>,
    pub target: RefCell<Value>,
    pub current: RefCell<Value>,
    pub time: f64,
    pub trusted: bool,
    pub path: RefCell<Vec<Value>>,
}

impl HostObject for EventState {
    fn class_name(&self) -> &'static str {
        "Event"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn get(&self, _it: &mut Interp, key: &PropKey) -> Option<R<Value>> {
        Some(Ok(match key.as_str()? {
            "type" => Value::str(&self.ty.borrow()),
            "bubbles" => Value::Bool(self.bubbles.get()),
            "cancelable" => Value::Bool(self.cancelable.get()),
            "defaultPrevented" => Value::Bool(self.flags.get() & F_PREVENTED != 0),
            "returnValue" => Value::Bool(self.flags.get() & F_PREVENTED == 0),
            "cancelBubble" => Value::Bool(self.flags.get() & F_STOP != 0),
            "eventPhase" => Value::Num(self.phase.get() as f64),
            "target" | "srcElement" => self.target.borrow().clone(),
            "currentTarget" => self.current.borrow().clone(),
            "timeStamp" => Value::Num(self.time),
            "isTrusted" => Value::Bool(self.trusted),
            "composed" => Value::Bool(false),
            _ => return None,
        }))
    }
    fn set(&self, _it: &mut Interp, key: &PropKey, v: &Value) -> Option<R<()>> {
        match key.as_str()? {
            "returnValue" => {
                if !v.truthy() && self.cancelable.get() {
                    self.flags.set(self.flags.get() | F_PREVENTED);
                }
                Some(Ok(()))
            }
            "cancelBubble" => {
                if v.truthy() {
                    self.flags.set(self.flags.get() | F_STOP);
                }
                Some(Ok(()))
            }
            _ => None,
        }
    }
}

pub fn event_state(v: &Value) -> Option<Rc<dyn HostObject>> {
    let h = v.as_obj()?.host()?;
    if h.as_any().downcast_ref::<EventState>().is_some() {
        Some(h)
    } else {
        None
    }
}

fn with_state<T>(v: &Value, f: impl FnOnce(&EventState) -> T) -> Option<T> {
    let h = event_state(v)?;
    let s = h.as_any().downcast_ref::<EventState>()?;
    Some(f(s))
}

#[derive(Default, Clone)]
pub struct EventInit {
    pub bubbles: bool,
    pub cancelable: bool,
    pub props: Vec<(&'static str, Value)>,
}

type EventProtos = RefCell<std::collections::HashMap<&'static str, Obj>>;

fn event_protos(it: &mut Interp) -> std::rc::Rc<EventProtos> {
    if let Some(p) = it.ext.get("event_protos").cloned().and_then(|p| p.downcast::<EventProtos>().ok()) {
        return p;
    }
    let p = std::rc::Rc::new(EventProtos::default());
    it.ext.insert("event_protos", p.clone());
    p
}

fn proto_for(it: &Interp, kind: &str) -> Option<Obj> {
    let k: &'static str = match kind {
        "MouseEvent" | "MouseEvents" | "PointerEvent" | "WheelEvent" | "DragEvent" => "MouseEvent",
        "KeyboardEvent" => "KeyboardEvent",
        "CustomEvent" => "CustomEvent",
        "FocusEvent" => "FocusEvent",
        "InputEvent" => "InputEvent",
        "UIEvent" | "UIEvents" => "UIEvent",
        "SubmitEvent" => "SubmitEvent",
        "MessageEvent" => "MessageEvent",
        "ErrorEvent" => "ErrorEvent",
        "ProgressEvent" => "ProgressEvent",
        _ => "Event",
    };
    let protos = it.ext.get("event_protos")?.clone().downcast::<EventProtos>().ok()?;
    let r = protos.borrow().get(k).cloned();
    r
}

pub fn make_event(it: &mut Interp, ty: &str, kind: &str, init: &EventInit, trusted: bool) -> Value {
    let st = EventState {
        ty: RefCell::new(ty.to_string()),
        bubbles: Cell::new(init.bubbles),
        cancelable: Cell::new(init.cancelable),
        flags: Cell::new(0),
        phase: Cell::new(0),
        target: RefCell::new(Value::Null),
        current: RefCell::new(Value::Null),
        time: super::window::perf_now(),
        trusted,
        path: RefCell::new(vec![]),
    };
    let proto = proto_for(it, kind);
    let o = Obj::new(ObjKind::Host(Rc::new(st)), proto, "Event");
    for (k, v) in &init.props {
        o.set_prop(k, v.clone());
    }
    apply_defaults(it, kind, &o);
    Value::Obj(o)
}

/// Members an event interface always has, when the init dictionary left them out.
fn apply_defaults(it: &mut Interp, kind: &str, o: &Obj) {
    if kind == "MessageEvent" {
        for (k, v) in [("data", Value::Null), ("origin", Value::str("")), ("lastEventId", Value::str("")), ("source", Value::Null)] {
            if o.get_own(k).is_none() {
                o.set_prop(k, v);
            }
        }
        if o.get_own("ports").is_none() {
            let ports = it.new_array(vec![]);
            o.set_prop("ports", ports);
        }
    }
}

/// `new Event(type, init)` and friends.
pub fn new_event(it: &mut Interp, ty: &str, init: &Value, kind: &str) -> R<Value> {
    let mut ei = EventInit::default();
    if let Value::Obj(_) = init {
        ei.bubbles = it.get_str(init, "bubbles")?.truthy();
        ei.cancelable = it.get_str(init, "cancelable")?.truthy();
    }
    let ev = make_event(it, ty, kind, &ei, false);
    if let (Value::Obj(src), Value::Obj(dst)) = (init, &ev) {
        for k in it.own_keys(src, false) {
            if matches!(k.as_str(), Some("bubbles" | "cancelable" | "composed")) {
                continue;
            }
            let v = it.get(init, &k)?;
            dst.borrow_mut().props.insert(k, Prop::data(v));
        }
    }
    if let Value::Obj(dst) = &ev {
        if kind == "CustomEvent" && dst.get_own("detail").is_none() {
            dst.set_prop("detail", Value::Null);
        }
        apply_defaults(it, kind, dst);
    }
    Ok(ev)
}

pub fn target_of(it: &mut Interp, this: &Value) -> Target {
    match this {
        Value::Obj(o) if o.ptr_eq(&it.global) => Target::Window,
        Value::Undefined | Value::Null => Target::Window,
        v => match node_of(v) {
            Some(n) => Target::Node(n),
            None => match v {
                Value::Obj(o) => Target::Object(o.id()),
                _ => Target::Window,
            },
        },
    }
}

fn target_value(it: &mut Interp, t: Target, objs: &[(usize, Value)]) -> Value {
    match t {
        Target::Window => Value::Obj(it.global.clone()),
        Target::Node(n) => wrap(it, n),
        Target::Object(id) => objs.iter().find(|(i, _)| *i == id).map(|(_, v)| v.clone()).unwrap_or(Value::Null),
    }
}

fn parse_options(it: &mut Interp, v: &Value) -> R<(bool, bool, bool)> {
    Ok(match v {
        Value::Obj(_) => (it.get_str(v, "capture")?.truthy(), it.get_str(v, "once")?.truthy(), it.get_str(v, "passive")?.truthy()),
        v => (v.truthy(), false, false),
    })
}

pub fn add_listener(it: &mut Interp, this: &Value, args: &[Value]) -> R<Value> {
    let ty = it.to_str(&arg(args, 0))?.to_string();
    let cb = arg(args, 1);
    if cb.is_nullish() {
        return Ok(Value::Undefined);
    }
    let (capture, once, passive) = parse_options(it, &arg(args, 2))?;
    let t = target_of(it, this);
    if let Target::Object(id) = t {
        ph(it).listener_objects.insert(id, this.clone());
    }
    let list = ph(it).listeners.entry(t).or_default();
    let dup = list.iter().any(|l| l.ty == ty && l.capture == capture && crate::js::interp::strict_equals(&l.callback, &cb));
    if !dup {
        list.push(Listener { ty, callback: cb, capture, once, passive });
    }
    Ok(Value::Undefined)
}

pub fn remove_listener(it: &mut Interp, this: &Value, args: &[Value]) -> R<Value> {
    let ty = it.to_str(&arg(args, 0))?.to_string();
    let cb = arg(args, 1);
    let (capture, _, _) = parse_options(it, &arg(args, 2))?;
    let t = target_of(it, this);
    if let Some(list) = ph(it).listeners.get_mut(&t) {
        list.retain(|l| !(l.ty == ty && l.capture == capture && crate::js::interp::strict_equals(&l.callback, &cb)));
    }
    Ok(Value::Undefined)
}

/// Compile an inline `on*` attribute handler.
fn attr_handler(it: &mut Interp, n: NodeId, ty: &str) -> Option<Value> {
    let code = ph(it).doc.attr(n, &format!("on{ty}"))?.to_string();
    let def = crate::js::parser::parse_function_body(&["event".to_string()], &code).ok()?;
    let env = it.global_env.clone();
    Some(it.make_function(def, &env, None))
}

fn handler_for(it: &mut Interp, t: Target, ty: &str, objs: &[(usize, Value)]) -> Option<Value> {
    let prop = format!("on{ty}");
    let tv = target_value(it, t, objs);
    if let Value::Obj(o) = &tv {
        if let Some(v) = o.get_own(&prop) {
            if v.is_callable() {
                return Some(v);
            }
        }
    }
    match t {
        Target::Node(n) => attr_handler(it, n, ty),
        Target::Window => {
            // <body onload=...> and friends forward to window
            if matches!(ty, "load" | "unload" | "beforeunload" | "resize" | "hashchange" | "popstate" | "message" | "error" | "pageshow" | "online" | "offline") {
                let body = ph(it).doc.body()?;
                return attr_handler(it, body, ty);
            }
            None
        }
        _ => None,
    }
}

fn invoke(it: &mut Interp, ev: &Value, t: Target, ty: &str, phase: u8, objs: &[(usize, Value)]) -> R<()> {
    let tv = target_value(it, t, objs);
    with_state(ev, |s| {
        *s.current.borrow_mut() = tv.clone();
        s.phase.set(phase);
    });
    // inline handler first (target and bubble phases)
    if phase != 1 {
        if let Some(h) = handler_for(it, t, ty, objs) {
            let r = it.call(&h, tv.clone(), std::slice::from_ref(ev));
            match r {
                Ok(Value::Bool(false)) => {
                    with_state(ev, |s| {
                        if s.cancelable.get() {
                            s.flags.set(s.flags.get() | F_PREVENTED)
                        }
                    });
                }
                Ok(_) => {}
                Err(e) => report(it, &e),
            }
            it.run_microtasks();
        }
    }
    let list: Vec<Listener> = ph(it).listeners.get(&t).map(|l| l.iter().filter(|l| l.ty == ty && (phase == 2 || l.capture == (phase == 1))).cloned().collect()).unwrap_or_default();
    for l in list {
        if with_state(ev, |s| s.flags.get() & F_STOP_IMMEDIATE != 0).unwrap_or(false) {
            break;
        }
        // listener may have been removed by an earlier one
        let still = ph(it).listeners.get(&t).is_some_and(|ls| ls.iter().any(|x| x.ty == l.ty && x.capture == l.capture && crate::js::interp::strict_equals(&x.callback, &l.callback)));
        if !still {
            continue;
        }
        if l.once {
            if let Some(ls) = ph(it).listeners.get_mut(&t) {
                ls.retain(|x| !(x.ty == l.ty && x.capture == l.capture && crate::js::interp::strict_equals(&x.callback, &l.callback)));
            }
        }
        let r = if l.callback.is_callable() {
            it.call(&l.callback, tv.clone(), std::slice::from_ref(ev))
        } else {
            match it.get_str(&l.callback, "handleEvent") {
                Ok(f) if f.is_callable() => it.call(&f, l.callback.clone(), std::slice::from_ref(ev)),
                Ok(_) => Ok(Value::Undefined),
                Err(e) => Err(e),
            }
        };
        if let Err(e) = r {
            report(it, &e);
        }
        it.run_microtasks();
    }
    Ok(())
}

fn report(it: &mut Interp, e: &Value) {
    let msg = it.describe_error(e);
    it.console.push(crate::js::interp::ConsoleMsg { level: "error", text: format!("Uncaught {msg}") });
}

/// Dispatch an event object; returns false if the default action was prevented.
pub fn dispatch_value(it: &mut Interp, target: Target, ev: &Value) -> R<bool> {
    let Some(ty) = with_state(ev, |s| s.ty.borrow().clone()) else { throw!(it, "TypeError", "Failed to execute 'dispatchEvent': parameter 1 is not of type 'Event'.") };
    let bubbles = with_state(ev, |s| s.bubbles.get()).unwrap_or(false);
    let objs: Vec<(usize, Value)> = ph(it).listener_objects.iter().map(|(k, v)| (*k, v.clone())).collect();
    let mut path = vec![target];
    if let Target::Node(n) = target {
        let h = ph(it);
        let connected = h.doc.is_connected(n);
        let mut cur = h.doc.parent(n);
        while let Some(p) = cur {
            path.push(Target::Node(p));
            cur = h.doc.parent(p);
        }
        if connected {
            path.push(Target::Window);
        }
    }
    let tv = target_value(it, target, &objs);
    let pathv: Vec<Value> = path.iter().map(|t| target_value(it, *t, &objs)).collect();
    with_state(ev, |s| {
        *s.target.borrow_mut() = tv;
        *s.path.borrow_mut() = pathv;
        s.flags.set(s.flags.get() & F_PREVENTED);
    });
    let stopped = |ev: &Value| with_state(ev, |s| s.flags.get() & (F_STOP | F_STOP_IMMEDIATE) != 0).unwrap_or(false);
    // capture
    for &t in path[1..].iter().rev() {
        if stopped(ev) {
            break;
        }
        invoke(it, ev, t, &ty, 1, &objs)?;
    }
    if !stopped(ev) {
        invoke(it, ev, target, &ty, 2, &objs)?;
    }
    if bubbles {
        for &t in &path[1..] {
            if stopped(ev) {
                break;
            }
            invoke(it, ev, t, &ty, 3, &objs)?;
        }
    }
    with_state(ev, |s| {
        s.phase.set(0);
        *s.current.borrow_mut() = Value::Null;
    });
    Ok(!with_state(ev, |s| s.flags.get() & F_PREVENTED != 0).unwrap_or(false))
}

/// Dispatch a trusted event created by the browser.
pub fn dispatch(page: &mut Page, target: Target, ty: &str, init: EventInit) -> bool {
    fire(&mut page.interp, target, ty, "Event", init)
}

pub fn fire(it: &mut Interp, target: Target, ty: &str, kind: &str, init: EventInit) -> bool {
    let ev = make_event(it, ty, kind, &init, true);
    match dispatch_value(it, target, &ev) {
        Ok(b) => b,
        Err(e) => {
            report(it, &e);
            true
        }
    }
}

pub fn install(it: &mut Interp, p: &DomProtos) {
    let et = p.event_target.clone();
    for target in [et.clone(), it.global.clone()] {
        it.define_method(&target, "addEventListener", 2, |it, this, args| add_listener(it, &this, args));
        it.define_method(&target, "removeEventListener", 2, |it, this, args| remove_listener(it, &this, args));
        it.define_method(&target, "dispatchEvent", 1, |it, this, args| {
            let t = target_of(it, &this);
            if let Target::Object(id) = t {
                ph(it).listener_objects.insert(id, this.clone());
            }
            Ok(Value::Bool(dispatch_value(it, t, &arg(args, 0))?))
        });
    }
    let ep = p.event.clone();
    it.define_method(&ep, "preventDefault", 0, |_, this, _| {
        with_state(&this, |s| {
            if s.cancelable.get() {
                s.flags.set(s.flags.get() | F_PREVENTED)
            }
        });
        Ok(Value::Undefined)
    });
    it.define_method(&ep, "stopPropagation", 0, |_, this, _| {
        with_state(&this, |s| s.flags.set(s.flags.get() | F_STOP));
        Ok(Value::Undefined)
    });
    it.define_method(&ep, "stopImmediatePropagation", 0, |_, this, _| {
        with_state(&this, |s| s.flags.set(s.flags.get() | F_STOP | F_STOP_IMMEDIATE));
        Ok(Value::Undefined)
    });
    it.define_method(&ep, "composedPath", 0, |it, this, _| {
        let p = with_state(&this, |s| s.path.borrow().clone()).unwrap_or_default();
        Ok(it.new_array(p))
    });
    it.define_method(&ep, "initEvent", 3, |it, this, args| {
        let ty = it.to_str(&arg(args, 0))?.to_string();
        let b = arg(args, 1).truthy();
        let c = arg(args, 2).truthy();
        with_state(&this, |s| {
            *s.ty.borrow_mut() = ty;
            s.bubbles.set(b);
            s.cancelable.set(c);
        });
        Ok(Value::Undefined)
    });
    it.define_method(&ep, "getModifierState", 1, |it, this, args| {
        let k = it.to_str(&arg(args, 0))?;
        let prop = match &*k {
            "Shift" => "shiftKey",
            "Control" => "ctrlKey",
            "Alt" => "altKey",
            "Meta" => "metaKey",
            _ => return Ok(Value::Bool(false)),
        };
        Ok(Value::Bool(it.get_str(&this, prop)?.truthy()))
    });
    for (k, v) in [("NONE", 0), ("CAPTURING_PHASE", 1), ("AT_TARGET", 2), ("BUBBLING_PHASE", 3)] {
        ep.set_hidden(k, Value::Num(v as f64));
    }
    let eprotos = event_protos(it);
    eprotos.borrow_mut().insert("Event", ep.clone());
    let ector = it.define_ctor("Event", 1, &ep, |it, _this, args| {
        it.constructing = false;
        let ty = it.to_str(&arg(args, 0))?.to_string();
        new_event(it, &ty, &arg(args, 1), "Event")
    });
    let _ = ector;
    for kind in ["CustomEvent", "UIEvent", "MouseEvent", "KeyboardEvent", "FocusEvent", "InputEvent", "SubmitEvent", "MessageEvent", "ErrorEvent", "ProgressEvent", "PointerEvent", "WheelEvent", "TouchEvent", "AnimationEvent", "TransitionEvent", "PopStateEvent", "HashChangeEvent", "StorageEvent", "ClipboardEvent", "DragEvent", "CompositionEvent", "PageTransitionEvent", "BeforeUnloadEvent", "PromiseRejectionEvent"] {
        let pr = Obj::new(ObjKind::Ordinary, Some(ep.clone()), "Event");
        let k: &'static str = kind;
        eprotos.borrow_mut().insert(k, pr.clone());
        it.define_ctor(k, 1, &pr, move |it, _this, args| {
            it.constructing = false;
            let ty = it.to_str(&arg(args, 0))?.to_string();
            new_event(it, &ty, &arg(args, 1), k)
        });
    }
    // EventTarget is constructible (for `class X extends EventTarget`)
    it.define_ctor("EventTarget", 0, &et, |it, this, _| {
        it.constructing = false;
        Ok(this)
    });
}

pub fn mouse_init(x: f32, y: f32, scroll: (f32, f32), button: i32, detail: i32, mods: Mods) -> Vec<(&'static str, Value)> {
    let n = |v: f32| Value::Num(v as f64);
    vec![
        ("clientX", n(x)),
        ("clientY", n(y)),
        ("x", n(x)),
        ("y", n(y)),
        ("pageX", n(x + scroll.0)),
        ("pageY", n(y + scroll.1)),
        ("screenX", n(x)),
        ("screenY", n(y)),
        ("offsetX", n(x)),
        ("offsetY", n(y)),
        ("movementX", n(0.0)),
        ("movementY", n(0.0)),
        ("button", Value::Num(button as f64)),
        ("buttons", Value::Num(if button >= 0 { 1.0 } else { 0.0 })),
        ("detail", Value::Num(detail as f64)),
        ("which", Value::Num((button + 1) as f64)),
        ("ctrlKey", Value::Bool(mods.ctrl)),
        ("shiftKey", Value::Bool(mods.shift)),
        ("altKey", Value::Bool(mods.alt)),
        ("metaKey", Value::Bool(mods.meta)),
        ("relatedTarget", Value::Null),
        ("pointerId", Value::Num(1.0)),
        ("pointerType", Value::str("mouse")),
        ("isPrimary", Value::Bool(true)),
    ]
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
}

pub fn key_code(key: &str) -> u32 {
    match key {
        "Backspace" => 8,
        "Tab" => 9,
        "Enter" => 13,
        "Shift" => 16,
        "Control" => 17,
        "Alt" => 18,
        "Escape" => 27,
        " " => 32,
        "PageUp" => 33,
        "PageDown" => 34,
        "End" => 35,
        "Home" => 36,
        "ArrowLeft" => 37,
        "ArrowUp" => 38,
        "ArrowRight" => 39,
        "ArrowDown" => 40,
        "Delete" => 46,
        k if k.chars().count() == 1 => {
            let c = k.chars().next().unwrap();
            if c.is_ascii_alphabetic() {
                c.to_ascii_uppercase() as u32
            } else if c.is_ascii_digit() {
                c as u32
            } else {
                match c {
                    ';' => 186,
                    '=' => 187,
                    ',' => 188,
                    '-' => 189,
                    '.' => 190,
                    '/' => 191,
                    '`' => 192,
                    '[' => 219,
                    '\\' => 220,
                    ']' => 221,
                    '\'' => 222,
                    _ => 0,
                }
            }
        }
        _ => 0,
    }
}

pub fn key_init(key: &str, mods: Mods, repeat: bool) -> Vec<(&'static str, Value)> {
    let code = match key {
        k if k.chars().count() == 1 && k.chars().next().unwrap().is_ascii_alphabetic() => format!("Key{}", k.to_ascii_uppercase()),
        k if k.chars().count() == 1 && k.chars().next().unwrap().is_ascii_digit() => format!("Digit{k}"),
        " " => "Space".into(),
        k => k.to_string(),
    };
    let kc = key_code(key) as f64;
    vec![
        ("key", Value::str(key)),
        ("code", Value::str(&code)),
        ("keyCode", Value::Num(kc)),
        ("which", Value::Num(kc)),
        ("charCode", Value::Num(if key.chars().count() == 1 { key.chars().next().unwrap() as u32 as f64 } else { 0.0 })),
        ("repeat", Value::Bool(repeat)),
        ("isComposing", Value::Bool(false)),
        ("location", Value::Num(0.0)),
        ("ctrlKey", Value::Bool(mods.ctrl)),
        ("shiftKey", Value::Bool(mods.shift)),
        ("altKey", Value::Bool(mods.alt)),
        ("metaKey", Value::Bool(mods.meta)),
    ]
}
