//! Tree-walking evaluator.

use std::any::Any;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::Instant;

use super::ast::*;
use super::coro::{CoSide, Coroutine, Resume, Yield};
use super::value::*;

pub type R<T> = Result<T, Value>;

pub struct Binding {
    pub value: Value,
    pub mutable: bool,
    /// Live import: reads go to this binding of another module's environment.
    pub link: Option<Rc<(Env, JsStr)>>,
}

pub struct FuncScope {
    /// `None` until `super()` is called in a derived constructor.
    pub this: RefCell<Option<Value>>,
    pub home: Option<Obj>,
    pub new_target: Value,
    pub func: Option<Obj>,
}

pub struct Scope {
    pub vars: RefCell<HashMap<JsStr, Binding>>,
    pub parent: Option<Env>,
    pub func: Option<FuncScope>,
}

pub type Env = Rc<Scope>;

impl Scope {
    pub fn new(parent: Option<Env>) -> Env {
        Rc::new(Scope { vars: RefCell::new(HashMap::new()), parent, func: None })
    }
    pub fn new_func(parent: Option<Env>, fs: FuncScope) -> Env {
        Rc::new(Scope { vars: RefCell::new(HashMap::new()), parent, func: Some(fs) })
    }
    pub fn declare(&self, name: JsStr, value: Value, mutable: bool) {
        self.vars.borrow_mut().insert(name, Binding { value, mutable, link: None });
    }
    /// Declare an immutable binding that reads through to `target` in `env`.
    pub fn declare_link(&self, name: JsStr, env: Env, target: JsStr) {
        self.vars.borrow_mut().insert(name, Binding { value: Value::Undefined, mutable: false, link: Some(Rc::new((env, target))) });
    }
}

/// Read a binding by name from one scope, following import links.
pub fn read_binding(env: &Env, name: &str) -> Option<Value> {
    let mut link = {
        let vars = env.vars.borrow();
        let b = vars.get(name)?;
        match &b.link {
            None => return Some(b.value.clone()),
            Some(l) => l.clone(),
        }
    };
    for _ in 0..64 {
        let next = {
            let vars = link.0.vars.borrow();
            let b = vars.get(&link.1)?;
            match &b.link {
                None => return Some(b.value.clone()),
                Some(l) => l.clone(),
            }
        };
        link = next;
    }
    None
}

pub enum Flow {
    Normal,
    Return(Value),
    Break(Option<Name>),
    Continue(Option<Name>),
}

pub enum Job {
    Call(Value, Vec<Value>),
    Native(Box<dyn FnOnce(&mut Interp) -> R<()>>),
}

pub struct Protos {
    pub object: Obj,
    pub function: Obj,
    pub array: Obj,
    pub string: Obj,
    pub number: Obj,
    pub boolean: Obj,
    pub symbol: Obj,
    pub bigint: Obj,
    pub error: Obj,
    pub errors: HashMap<&'static str, Obj>,
    pub promise: Obj,
    pub regexp: Obj,
    pub date: Obj,
    pub map: Obj,
    pub set: Obj,
    pub weakmap: Obj,
    pub weakset: Obj,
    pub iterator: Obj,
    pub generator: Obj,
    pub typed: HashMap<&'static str, Obj>,
    pub arraybuffer: Obj,
}

pub struct ConsoleMsg {
    pub level: &'static str,
    pub text: String,
}

/// Stack budget for JS execution on the main thread (bytes). The binary raises this when it
/// is linked with a larger main-thread stack.
pub static MAIN_STACK_LIMIT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(6 * 1024 * 1024);

#[inline(never)]
pub fn stack_pointer() -> usize {
    let x = 0u8;
    std::hint::black_box(&x) as *const u8 as usize
}

pub struct Interp {
    /// Completion value of the last expression statement (for eval / DevTools console).
    pub completion: Value,
    pub stack_base: usize,
    pub stack_limit: usize,
    pub global: Obj,
    pub global_env: Env,
    pub p: Protos,
    pub microtasks: VecDeque<Job>,
    pub host: Option<Box<dyn Any>>,
    pub call_depth: usize,
    pub max_depth: usize,
    pub current_co: Option<*const CoSide>,
    pub live_coroutines: usize,
    pub pending_return: Option<Value>,
    pub constructing: bool,
    short_circuit: Obj,
    gen_return: Obj,
    abort: Obj,
    pub sym_iterator: Sym,
    pub sym_async_iterator: Sym,
    pub sym_has_instance: Sym,
    pub sym_to_primitive: Sym,
    pub sym_to_string_tag: Sym,
    pub symbol_registry: HashMap<JsStr, Sym>,
    pub cur_line: u32,
    pub script_name: String,
    pub console: Vec<ConsoleMsg>,
    pub deadline: Option<Instant>,
    steps: u64,
    pub unhandled: Vec<Value>,
    pub coroutines: Vec<std::rc::Weak<Coroutine>>,
    /// ES module map keyed by URL.
    pub modules: HashMap<String, super::module::ModRef>,
    pub module_loader: Option<Rc<dyn super::module::ModuleLoader>>,
}

macro_rules! throw {
    ($it:expr, $kind:expr, $($arg:tt)*) => {{
        let __msg = format!($($arg)*);
        return Err($it.make_error($kind, &__msg));
    }};
}
pub(crate) use throw;

pub fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).cloned().unwrap_or(Value::Undefined)
}

impl Interp {
    pub fn new() -> Box<Interp> {
        let object = Obj::new(ObjKind::Ordinary, None, "Object");
        let mk = |class: &'static str, kind: ObjKind| Obj::new(kind, Some(object.clone()), class);
        let function = Obj::new(ObjKind::Function(Func::Native { f: Rc::new(|_, _, _| Ok(Value::Undefined)), ctor: false }), Some(object.clone()), "Function");
        let error = mk("Error", ObjKind::Ordinary);
        let mut errors = HashMap::new();
        for e in ["TypeError", "RangeError", "SyntaxError", "ReferenceError", "EvalError", "URIError", "AggregateError"] {
            errors.insert(e, Obj::new(ObjKind::Ordinary, Some(error.clone()), "Error"));
        }
        let iterator = mk("Iterator", ObjKind::Ordinary);
        let p = Protos {
            function,
            array: mk("Array", ObjKind::Array(vec![])),
            string: mk("String", ObjKind::Str(Rc::from(""))),
            number: mk("Number", ObjKind::Num(0.0)),
            boolean: mk("Boolean", ObjKind::Bool(false)),
            symbol: mk("Symbol", ObjKind::Ordinary),
            bigint: mk("BigInt", ObjKind::Ordinary),
            error,
            errors,
            promise: mk("Promise", ObjKind::Ordinary),
            regexp: mk("RegExp", ObjKind::Ordinary),
            date: mk("Date", ObjKind::Ordinary),
            map: mk("Map", ObjKind::Ordinary),
            set: mk("Set", ObjKind::Ordinary),
            weakmap: mk("WeakMap", ObjKind::Ordinary),
            weakset: mk("WeakSet", ObjKind::Ordinary),
            generator: Obj::new(ObjKind::Ordinary, Some(iterator.clone()), "Generator"),
            iterator,
            typed: HashMap::new(),
            arraybuffer: mk("ArrayBuffer", ObjKind::Ordinary),
            object: object.clone(),
        };
        let global = Obj::new(ObjKind::Ordinary, Some(object.clone()), "Window");
        let global_env = Scope::new_func(None, FuncScope { this: RefCell::new(Some(Value::Obj(global.clone()))), home: None, new_target: Value::Undefined, func: None });
        let sym = |d: &str| Sym(Rc::new(SymbolData { description: Some(Rc::from(d)) }));
        let mut it = Box::new(Interp {
            completion: Value::Undefined,
            stack_base: 0,
            stack_limit: MAIN_STACK_LIMIT.load(std::sync::atomic::Ordering::Relaxed),
            global,
            global_env,
            p,
            microtasks: VecDeque::new(),
            host: None,
            call_depth: 0,
            max_depth: 100_000,
            current_co: None,
            live_coroutines: 0,
            pending_return: None,
            constructing: false,
            short_circuit: Obj::new(ObjKind::Ordinary, None, "ShortCircuit"),
            gen_return: Obj::new(ObjKind::Ordinary, None, "GeneratorReturn"),
            abort: Obj::new(ObjKind::Ordinary, None, "Abort"),
            sym_iterator: sym("Symbol.iterator"),
            sym_async_iterator: sym("Symbol.asyncIterator"),
            sym_has_instance: sym("Symbol.hasInstance"),
            sym_to_primitive: sym("Symbol.toPrimitive"),
            sym_to_string_tag: sym("Symbol.toStringTag"),
            symbol_registry: HashMap::new(),
            cur_line: 0,
            script_name: "script".into(),
            console: vec![],
            deadline: None,
            modules: HashMap::new(),
            module_loader: None,
            steps: 0,
            unhandled: vec![],
            coroutines: vec![],
        });
        super::builtins::setup(&mut it);
        it
    }

    pub fn host<T: 'static>(&self) -> Option<&T> {
        self.host.as_ref()?.downcast_ref::<T>()
    }

    pub fn host_mut<T: 'static>(&mut self) -> Option<&mut T> {
        self.host.as_mut()?.downcast_mut::<T>()
    }

    pub fn gen_return_marker(&self) -> Value {
        Value::Obj(self.gen_return.clone())
    }
    pub fn abort_marker(&self) -> Value {
        Value::Obj(self.abort.clone())
    }
    fn is_marker(&self, v: &Value) -> bool {
        match v {
            Value::Obj(o) => o.ptr_eq(&self.gen_return) || o.ptr_eq(&self.abort) || o.ptr_eq(&self.short_circuit),
            _ => false,
        }
    }

    pub fn normalize_coroutine_result(&mut self, r: R<Value>) -> R<Value> {
        match r {
            Err(Value::Obj(o)) if o.ptr_eq(&self.gen_return) => Ok(self.pending_return.take().unwrap_or(Value::Undefined)),
            Err(Value::Obj(o)) if o.ptr_eq(&self.abort) => Ok(Value::Undefined),
            r => r,
        }
    }

    // ------------------------------------------------------------ errors

    pub fn make_error(&mut self, kind: &str, msg: &str) -> Value {
        let proto = self.p.errors.get(kind).cloned().unwrap_or_else(|| self.p.error.clone());
        let o = Obj::new(ObjKind::Error, Some(proto), "Error");
        o.set_hidden("message", Value::str(msg));
        o.set_hidden("stack", Value::str(&format!("{kind}: {msg}\n    at {}:{}", self.script_name, self.cur_line + 1)));
        Value::Obj(o)
    }

    /// Human-readable description of a thrown value.
    pub fn describe_error(&mut self, v: &Value) -> String {
        if let Value::Obj(o) = v {
            if matches!(o.borrow().kind, ObjKind::Error) || o.get_own("message").is_some() {
                let name = self.get(v, &PropKey::from("name")).ok().map(|n| self.to_string_lossy(&n)).unwrap_or_else(|| "Error".into());
                let msg = self.get(v, &PropKey::from("message")).ok().map(|n| self.to_string_lossy(&n)).unwrap_or_default();
                return if msg.is_empty() { name } else { format!("{name}: {msg}") };
            }
        }
        format!("Uncaught {}", self.inspect(v, 1))
    }

    /// Guard against native stack exhaustion from deep JS recursion.
    pub fn check_stack(&mut self) -> R<()> {
        let sp = stack_pointer();
        if self.call_depth == 0 || self.stack_base == 0 {
            self.stack_base = sp;
        }
        if self.stack_base.saturating_sub(sp) > self.stack_limit || self.call_depth > self.max_depth {
            throw!(self, "RangeError", "Maximum call stack size exceeded");
        }
        Ok(())
    }

    pub fn take_completion(&mut self) -> Value {
        std::mem::replace(&mut self.completion, Value::Undefined)
    }

    pub fn check_deadline(&mut self) -> R<()> {
        self.steps += 1;
        if self.steps % 4096 == 0 {
            if let Some(d) = self.deadline {
                if Instant::now() > d {
                    self.deadline = None;
                    throw!(self, "Error", "Script execution timed out");
                }
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ object helpers

    pub fn new_object(&self) -> Obj {
        Obj::new(ObjKind::Ordinary, Some(self.p.object.clone()), "Object")
    }

    pub fn new_array(&self, items: Vec<Value>) -> Value {
        Value::Obj(Obj::new(ObjKind::Array(items), Some(self.p.array.clone()), "Array"))
    }

    pub fn new_native(&self, name: &str, len: u32, f: NativeFn, ctor: bool) -> Obj {
        let o = Obj::new(ObjKind::Function(Func::Native { f, ctor }), Some(self.p.function.clone()), "Function");
        {
            let mut b = o.borrow_mut();
            b.props.insert(PropKey::from("name"), Prop { value: Value::str(name), getter: None, setter: None, enumerable: false, writable: false, configurable: true });
            b.props.insert(PropKey::from("length"), Prop { value: Value::Num(len as f64), getter: None, setter: None, enumerable: false, writable: false, configurable: true });
        }
        o
    }

    pub fn native(&self, name: &str, len: u32, f: impl Fn(&mut Interp, Value, &[Value]) -> R<Value> + 'static) -> Value {
        Value::Obj(self.new_native(name, len, Rc::new(f), false))
    }

    pub fn define_method(&self, obj: &Obj, name: &str, len: u32, f: impl Fn(&mut Interp, Value, &[Value]) -> R<Value> + 'static) {
        let fun = self.new_native(name, len, Rc::new(f), false);
        obj.borrow_mut().props.insert(PropKey::from(name), Prop::hidden(Value::Obj(fun)));
    }

    pub fn define_method_sym(&self, obj: &Obj, sym: &Sym, name: &str, f: impl Fn(&mut Interp, Value, &[Value]) -> R<Value> + 'static) {
        let fun = self.new_native(name, 0, Rc::new(f), false);
        obj.borrow_mut().props.insert(PropKey::Sym(sym.clone()), Prop::hidden(Value::Obj(fun)));
    }

    pub fn define_getter(&self, obj: &Obj, name: &str, f: impl Fn(&mut Interp, Value, &[Value]) -> R<Value> + 'static) {
        let fun = self.new_native(&format!("get {name}"), 0, Rc::new(f), false);
        obj.borrow_mut().props.insert(PropKey::from(name), Prop { value: Value::Undefined, getter: Some(Value::Obj(fun)), setter: None, enumerable: false, writable: true, configurable: true });
    }

    pub fn define_accessor(&self, obj: &Obj, name: &str, g: impl Fn(&mut Interp, Value, &[Value]) -> R<Value> + 'static, s: impl Fn(&mut Interp, Value, &[Value]) -> R<Value> + 'static) {
        let gf = self.new_native(&format!("get {name}"), 0, Rc::new(g), false);
        let sf = self.new_native(&format!("set {name}"), 1, Rc::new(s), false);
        obj.borrow_mut().props.insert(PropKey::from(name), Prop { value: Value::Undefined, getter: Some(Value::Obj(gf)), setter: Some(Value::Obj(sf)), enumerable: true, writable: true, configurable: true });
    }

    /// Create a constructor function with a prototype object.
    pub fn define_ctor(&mut self, name: &'static str, len: u32, proto: &Obj, f: impl Fn(&mut Interp, Value, &[Value]) -> R<Value> + 'static) -> Obj {
        let c = self.new_native(name, len, Rc::new(f), true);
        c.borrow_mut().props.insert(PropKey::from("prototype"), Prop { value: Value::Obj(proto.clone()), getter: None, setter: None, enumerable: false, writable: false, configurable: false });
        proto.borrow_mut().props.insert(PropKey::from("constructor"), Prop::hidden(Value::Obj(c.clone())));
        self.global.borrow_mut().props.insert(PropKey::from(name), Prop::hidden(Value::Obj(c.clone())));
        c
    }

    pub fn symbol_key(&self, s: &Sym) -> PropKey {
        PropKey::Sym(s.clone())
    }

    // ------------------------------------------------------------ conversions

    pub fn to_primitive(&mut self, v: &Value, hint: &str) -> R<Value> {
        let Value::Obj(o) = v else { return Ok(v.clone()) };
        if let Some(prim) = match &o.borrow().kind {
            ObjKind::Num(n) if hint != "string" => Some(Value::Num(*n)),
            ObjKind::Str(s) if hint != "number" || true => Some(Value::Str(s.clone())),
            ObjKind::Bool(b) => Some(Value::Bool(*b)),
            _ => None,
        } {
            if !matches!(prim, Value::Str(_)) || hint != "number" {
                return Ok(prim);
            }
        }
        let tp = self.get(v, &PropKey::Sym(self.sym_to_primitive.clone()))?;
        if tp.is_callable() {
            let r = self.call(&tp, v.clone(), &[Value::str(hint)])?;
            if !matches!(r, Value::Obj(_)) {
                return Ok(r);
            }
            throw!(self, "TypeError", "Cannot convert object to primitive value");
        }
        let is_date = matches!(o.borrow().kind, ObjKind::Date(_));
        let order: [&str; 2] = if hint == "string" || (hint == "default" && is_date) { ["toString", "valueOf"] } else { ["valueOf", "toString"] };
        for m in order {
            let f = self.get(v, &PropKey::from(m))?;
            if f.is_callable() {
                let r = self.call(&f, v.clone(), &[])?;
                if !matches!(r, Value::Obj(_)) {
                    return Ok(r);
                }
            }
        }
        throw!(self, "TypeError", "Cannot convert object to primitive value")
    }

    pub fn to_number(&mut self, v: &Value) -> R<f64> {
        Ok(match v {
            Value::Undefined => f64::NAN,
            Value::Null => 0.0,
            Value::Bool(b) => *b as i32 as f64,
            Value::Num(n) => *n,
            Value::BigInt(n) => *n as f64,
            Value::Str(s) => str_to_num(s),
            Value::Sym(_) => throw!(self, "TypeError", "Cannot convert a Symbol value to a number"),
            Value::Obj(_) => {
                let p = self.to_primitive(v, "number")?;
                self.to_number(&p)?
            }
        })
    }

    pub fn to_numeric(&mut self, v: &Value) -> R<Value> {
        let p = self.to_primitive(v, "number")?;
        if let Value::BigInt(_) = p {
            return Ok(p);
        }
        Ok(Value::Num(self.to_number(&p)?))
    }

    pub fn to_str(&mut self, v: &Value) -> R<JsStr> {
        Ok(match v {
            Value::Str(s) => s.clone(),
            Value::Undefined => Rc::from("undefined"),
            Value::Null => Rc::from("null"),
            Value::Bool(b) => Rc::from(if *b { "true" } else { "false" }),
            Value::Num(n) => Rc::from(num_to_string(*n).as_str()),
            Value::BigInt(n) => Rc::from(n.to_string().as_str()),
            Value::Sym(_) => throw!(self, "TypeError", "Cannot convert a Symbol value to a string"),
            Value::Obj(_) => {
                let p = self.to_primitive(v, "string")?;
                self.to_str(&p)?
            }
        })
    }

    pub fn to_string_lossy(&mut self, v: &Value) -> String {
        match self.to_str(v) {
            Ok(s) => s.to_string(),
            Err(_) => match v {
                Value::Sym(s) => format!("Symbol({})", s.0.description.as_deref().unwrap_or("")),
                _ => "[object]".into(),
            },
        }
    }

    pub fn to_object(&mut self, v: &Value) -> R<Obj> {
        Ok(match v {
            Value::Obj(o) => o.clone(),
            Value::Undefined | Value::Null => throw!(self, "TypeError", "Cannot convert undefined or null to object"),
            Value::Bool(b) => Obj::new(ObjKind::Bool(*b), Some(self.p.boolean.clone()), "Boolean"),
            Value::Num(n) => Obj::new(ObjKind::Num(*n), Some(self.p.number.clone()), "Number"),
            Value::Str(s) => Obj::new(ObjKind::Str(s.clone()), Some(self.p.string.clone()), "String"),
            Value::Sym(s) => Obj::new(ObjKind::Symbol(s.clone()), Some(self.p.symbol.clone()), "Symbol"),
            Value::BigInt(_) => self.new_object(),
        })
    }

    pub fn to_key(&mut self, v: &Value) -> R<PropKey> {
        Ok(match v {
            Value::Sym(s) => PropKey::Sym(s.clone()),
            Value::Str(s) => PropKey::Str(s.clone()),
            Value::Num(n) if n.fract() == 0.0 && *n >= 0.0 && *n < 1e15 => PropKey::Str(Rc::from((*n as u64).to_string().as_str())),
            _ => {
                let p = self.to_primitive(v, "string")?;
                match p {
                    Value::Sym(s) => PropKey::Sym(s),
                    _ => PropKey::Str(self.to_str(&p)?),
                }
            }
        })
    }

    pub fn to_integer(&mut self, v: &Value) -> R<f64> {
        let n = self.to_number(v)?;
        Ok(if n.is_nan() { 0.0 } else { n.trunc() })
    }

    pub fn to_length(&mut self, v: &Value) -> R<usize> {
        let n = self.to_integer(v)?;
        Ok(if n <= 0.0 { 0 } else { n.min(9007199254740991.0) as usize })
    }

    // ------------------------------------------------------------ property access

    pub fn get(&mut self, v: &Value, key: &PropKey) -> R<Value> {
        match v {
            Value::Obj(o) => self.get_from(o, key, v),
            Value::Str(s) => {
                if let Some(k) = key.as_str() {
                    if k == "length" {
                        return Ok(Value::Num(utf16_len(s) as f64));
                    }
                    if let Some(i) = key.array_index() {
                        return Ok(string_index(s, i).map(|c| Value::Str(Rc::from(c.as_str()))).unwrap_or(Value::Undefined));
                    }
                }
                let proto = self.p.string.clone();
                self.get_from(&proto, key, v)
            }
            Value::Num(_) => {
                let proto = self.p.number.clone();
                self.get_from(&proto, key, v)
            }
            Value::Bool(_) => {
                let proto = self.p.boolean.clone();
                self.get_from(&proto, key, v)
            }
            Value::Sym(s) => {
                if key.as_str() == Some("description") {
                    return Ok(s.0.description.clone().map(Value::Str).unwrap_or(Value::Undefined));
                }
                let proto = self.p.symbol.clone();
                self.get_from(&proto, key, v)
            }
            Value::BigInt(_) => {
                let proto = self.p.bigint.clone();
                self.get_from(&proto, key, v)
            }
            Value::Undefined | Value::Null => {
                let what = if v.is_undefined() { "undefined" } else { "null" };
                let k = match key {
                    PropKey::Str(s) => s.to_string(),
                    PropKey::Sym(_) => "Symbol()".into(),
                };
                throw!(self, "TypeError", "Cannot read properties of {what} (reading '{k}')")
            }
        }
    }

    pub fn get_str(&mut self, v: &Value, key: &str) -> R<Value> {
        self.get(v, &PropKey::from(key))
    }

    pub fn get_from(&mut self, o: &Obj, key: &PropKey, receiver: &Value) -> R<Value> {
        let mut cur = o.clone();
        for _ in 0..10_000 {
            // exotic / host behaviour
            let host = {
                let b = cur.borrow();
                match &b.kind {
                    ObjKind::Array(items) => {
                        if let Some(i) = key.array_index() {
                            if let Some(v) = items.get(i) {
                                return Ok(v.clone());
                            }
                        } else if key.as_str() == Some("length") {
                            return Ok(Value::Num(items.len() as f64));
                        }
                        None
                    }
                    ObjKind::Str(s) => {
                        if key.as_str() == Some("length") {
                            return Ok(Value::Num(utf16_len(s) as f64));
                        }
                        if let Some(i) = key.array_index() {
                            if let Some(c) = string_index(s, i) {
                                return Ok(Value::Str(Rc::from(c.as_str())));
                            }
                        }
                        None
                    }
                    ObjKind::TypedArray(t) => {
                        if let Some(i) = key.array_index() {
                            return Ok(t.get(i).map(Value::Num).unwrap_or(Value::Undefined));
                        }
                        if key.as_str() == Some("length") {
                            return Ok(Value::Num(t.len as f64));
                        }
                        None
                    }
                    ObjKind::Arguments => None,
                    ObjKind::Host(h) => Some(h.clone()),
                    _ => None,
                }
            };
            if let Some(h) = host {
                // own expando properties take precedence over host properties
                let own = cur.borrow().props.get(key).cloned();
                if let Some(p) = own {
                    return self.read_prop(&p, receiver);
                }
                if let Some(r) = h.get(self, key) {
                    return r;
                }
            }
            let (prop, next) = {
                let b = cur.borrow();
                (b.props.get(key).cloned(), b.proto.clone())
            };
            if let Some(p) = prop {
                return self.read_prop(&p, receiver);
            }
            match next {
                Some(n) => cur = n,
                None => return Ok(Value::Undefined),
            }
        }
        throw!(self, "RangeError", "prototype chain too deep")
    }

    fn read_prop(&mut self, p: &Prop, receiver: &Value) -> R<Value> {
        if let Some(g) = &p.getter {
            return self.call(g, receiver.clone(), &[]);
        }
        if p.setter.is_some() {
            return Ok(Value::Undefined);
        }
        Ok(p.value.clone())
    }

    pub fn has_property(&mut self, o: &Obj, key: &PropKey) -> bool {
        let mut cur = Some(o.clone());
        while let Some(c) = cur {
            if self.has_own(&c, key) {
                return true;
            }
            cur = c.proto();
        }
        false
    }

    pub fn has_own(&mut self, o: &Obj, key: &PropKey) -> bool {
        let host = {
            let b = o.borrow();
            if b.props.contains(key) {
                return true;
            }
            match &b.kind {
                ObjKind::Array(items) => return key.array_index().is_some_and(|i| i < items.len()) || key.as_str() == Some("length"),
                ObjKind::Str(s) => return key.array_index().is_some_and(|i| i < utf16_len(s)) || key.as_str() == Some("length"),
                ObjKind::TypedArray(t) => return key.array_index().is_some_and(|i| i < t.len),
                ObjKind::Host(h) => Some(h.clone()),
                _ => None,
            }
        };
        if let Some(h) = host {
            return h.get(self, key).is_some_and(|r| r.is_ok_and(|v| !v.is_undefined()));
        }
        false
    }

    pub fn set(&mut self, target: &Value, key: PropKey, value: Value) -> R<()> {
        let Value::Obj(o) = target else {
            if target.is_nullish() {
                let k = key.as_str().unwrap_or("Symbol()").to_string();
                throw!(self, "TypeError", "Cannot set properties of {} (setting '{k}')", if target.is_undefined() { "undefined" } else { "null" });
            }
            return Ok(());
        };
        // exotic
        let host = {
            let mut b = o.borrow_mut();
            if b.frozen {
                return Ok(());
            }
            match &mut b.kind {
                ObjKind::Array(items) => {
                    if let Some(i) = key.array_index() {
                        if i < items.len() {
                            items[i] = value;
                            return Ok(());
                        }
                        if i < items.len() + 10_000_000 && !b.props.contains(&key) {
                            if let ObjKind::Array(items) = &mut b.kind {
                                items.resize(i, Value::Undefined);
                                items.push(value);
                            }
                            return Ok(());
                        }
                    } else if key.as_str() == Some("length") {
                        drop(b);
                        let n = self.to_number(&value)?;
                        if n < 0.0 || n.fract() != 0.0 || n > 4294967295.0 {
                            throw!(self, "RangeError", "Invalid array length");
                        }
                        if let ObjKind::Array(items) = &mut o.borrow_mut().kind {
                            items.resize(n as usize, Value::Undefined);
                        }
                        return Ok(());
                    }
                    None
                }
                ObjKind::TypedArray(t) => {
                    if let Some(i) = key.array_index() {
                        let t = t.clone();
                        drop(b);
                        let n = self.to_number(&value)?;
                        t.set(i, n);
                        return Ok(());
                    }
                    None
                }
                ObjKind::Host(h) => Some(h.clone()),
                _ => None,
            }
        };
        if let Some(h) = host {
            if !o.borrow().props.contains(&key) {
                if let Some(r) = h.set(self, &key, &value) {
                    return r;
                }
            }
        }
        // setter lookup along the prototype chain
        let mut cur = Some(o.clone());
        while let Some(c) = cur {
            let p = c.borrow().props.get(&key).cloned();
            if let Some(p) = p {
                if p.is_accessor() {
                    if let Some(s) = p.setter {
                        self.call(&s, target.clone(), &[value])?;
                    }
                    return Ok(());
                }
                if c.ptr_eq(o) {
                    if p.writable {
                        if let Some(slot) = o.borrow_mut().props.get_mut(&key) {
                            slot.value = value;
                        }
                    }
                    return Ok(());
                }
                if !p.writable {
                    return Ok(());
                }
                break;
            }
            cur = c.proto();
        }
        let mut b = o.borrow_mut();
        if b.extensible {
            b.props.insert(key, Prop::data(value));
        }
        Ok(())
    }

    pub fn set_str(&mut self, target: &Value, key: &str, value: Value) -> R<()> {
        self.set(target, PropKey::from(key), value)
    }

    pub fn delete(&mut self, target: &Value, key: &PropKey) -> R<bool> {
        let Value::Obj(o) = target else { return Ok(true) };
        let mut b = o.borrow_mut();
        if b.frozen {
            return Ok(false);
        }
        if let ObjKind::Array(items) = &mut b.kind {
            if let Some(i) = key.array_index() {
                if i < items.len() {
                    items[i] = Value::Undefined;
                }
                return Ok(true);
            }
        }
        if let Some(p) = b.props.get(key) {
            if !p.configurable {
                return Ok(false);
            }
        }
        b.props.remove(key);
        Ok(true)
    }

    /// Own enumerable string keys (for Object.keys / for-in / spread).
    pub fn own_keys(&mut self, o: &Obj, include_hidden: bool) -> Vec<PropKey> {
        let (mut out, host) = {
            let b = o.borrow();
            let mut out: Vec<PropKey> = Vec::new();
            match &b.kind {
                ObjKind::Array(items) => out.extend((0..items.len()).map(|i| PropKey::Str(Rc::from(i.to_string().as_str())))),
                ObjKind::Str(s) => out.extend((0..utf16_len(s)).map(|i| PropKey::Str(Rc::from(i.to_string().as_str())))),
                ObjKind::TypedArray(t) => out.extend((0..t.len).map(|i| PropKey::Str(Rc::from(i.to_string().as_str())))),
                _ => {}
            }
            for k in b.props.keys() {
                let p = b.props.get(&k).unwrap();
                if (p.enumerable || include_hidden) && matches!(k, PropKey::Str(_)) {
                    out.push(k);
                }
            }
            let host = if let ObjKind::Host(h) = &b.kind { Some(h.clone()) } else { None };
            (out, host)
        };
        if let Some(h) = host {
            for k in h.keys(self) {
                if !out.contains(&k) {
                    out.push(k);
                }
            }
        }
        out
    }

    // ------------------------------------------------------------ functions

    pub fn call(&mut self, f: &Value, this: Value, args: &[Value]) -> R<Value> {
        let Value::Obj(fo) = f else {
            throw!(self, "TypeError", "{} is not a function", self.short_desc(f));
        };
        enum K {
            Closure(Rc<Closure>),
            Native(NativeFn),
            Bound(Obj, Value, Vec<Value>),
        }
        let k = match &fo.borrow().kind {
            ObjKind::Function(Func::Closure(c)) => K::Closure(c.clone()),
            ObjKind::Function(Func::Native { f, .. }) => K::Native(f.clone()),
            ObjKind::Function(Func::Bound { target, this, args }) => K::Bound(target.clone(), this.clone(), args.clone()),
            _ => {
                throw!(self, "TypeError", "{} is not a function", self.short_desc(f));
            }
        };
        self.check_stack()?;
        self.call_depth += 1;
        let r = match k {
            K::Closure(c) => {
                if c.class.is_some() {
                    self.call_depth -= 1;
                    throw!(self, "TypeError", "Class constructor {} cannot be invoked without 'new'", c.def.name.as_deref().unwrap_or(""));
                }
                self.call_closure(&c, fo, this, args, Value::Undefined)
            }
            K::Native(nf) => {
                self.constructing = false;
                nf(self, this, args)
            }
            K::Bound(t, bthis, bargs) => {
                let mut all = bargs;
                all.extend_from_slice(args);
                self.call(&Value::Obj(t), bthis, &all)
            }
        };
        self.call_depth -= 1;
        r
    }

    pub fn construct(&mut self, f: &Value, args: &[Value], new_target: Option<&Value>) -> R<Value> {
        let Value::Obj(fo) = f else {
            throw!(self, "TypeError", "{} is not a constructor", self.short_desc(f));
        };
        let nt = new_target.cloned().unwrap_or_else(|| f.clone());
        enum K {
            Closure(Rc<Closure>),
            Native(NativeFn),
            Bound(Obj, Vec<Value>),
        }
        let k = match &fo.borrow().kind {
            ObjKind::Function(Func::Closure(c)) if !c.def.is_arrow && !c.def.is_async && !c.def.is_generator && (!c.def.is_method || c.class.is_some()) => K::Closure(c.clone()),
            ObjKind::Function(Func::Native { f, ctor: true }) => K::Native(f.clone()),
            ObjKind::Function(Func::Bound { target, args, .. }) => K::Bound(target.clone(), args.clone()),
            _ => {
                throw!(self, "TypeError", "{} is not a constructor", self.short_desc(f));
            }
        };
        self.check_stack()?;
        self.call_depth += 1;
        let r = (|| -> R<Value> {
            match k {
                K::Closure(c) => {
                    let derived = c.class.as_ref().is_some_and(|ci| ci.derived);
                    let this = if derived { None } else { Some(Value::Obj(self.object_for_new(&nt)?)) };
                    if let (Some(ci), Some(Value::Obj(t))) = (&c.class, &this) {
                        self.init_fields(ci, t)?;
                    }
                    self.run_constructor(&c, fo, this, args, nt)
                }
                K::Native(nf) => {
                    let this = Value::Obj(self.object_for_new(&nt)?);
                    self.constructing = true;
                    let r = nf(self, this.clone(), args);
                    self.constructing = false;
                    let r = r?;
                    Ok(if matches!(r, Value::Obj(_)) { r } else { this })
                }
                K::Bound(t, bargs) => {
                    let mut all = bargs;
                    all.extend_from_slice(args);
                    let tv = Value::Obj(t);
                    let nt2 = if nt.as_obj().is_some_and(|o| o.ptr_eq(fo)) { tv.clone() } else { nt };
                    self.construct(&tv, &all, Some(&nt2))
                }
            }
        })();
        self.call_depth -= 1;
        r
    }

    fn object_for_new(&mut self, nt: &Value) -> R<Obj> {
        let proto = self.get(nt, &PropKey::from("prototype"))?;
        let proto = match proto {
            Value::Obj(p) => p,
            _ => self.p.object.clone(),
        };
        Ok(Obj::new(ObjKind::Ordinary, Some(proto), "Object"))
    }

    fn init_fields(&mut self, ci: &Rc<ClassInfo>, target: &Obj) -> R<()> {
        let fields = ci.fields.borrow().clone();
        if fields.is_empty() {
            return Ok(());
        }
        let home = ci.proto.borrow().clone();
        let env = Scope::new_func(Some(ci.env.clone()), FuncScope { this: RefCell::new(Some(Value::Obj(target.clone()))), home, new_target: Value::Undefined, func: None });
        for (key, idx) in fields {
            let v = match idx {
                Some(i) => match &ci.def.members[i] {
                    ClassMember::Field { value: Some(e), .. } => {
                        let v = self.eval(e, &env)?;
                        self.maybe_name(&v, &key);
                        v
                    }
                    _ => Value::Undefined,
                },
                None => Value::Undefined,
            };
            target.borrow_mut().props.insert(key, Prop::data(v));
        }
        Ok(())
    }

    fn run_constructor(&mut self, c: &Rc<Closure>, fo: &Obj, this: Option<Value>, args: &[Value], nt: Value) -> R<Value> {
        let env = Scope::new_func(Some(c.env.clone()), FuncScope { this: RefCell::new(this), home: c.home.borrow().clone(), new_target: nt, func: Some(fo.clone()) });
        let r = self.run_body(c, &env, args)?;
        if let Value::Obj(_) = r {
            return Ok(r);
        }
        let t = env.func.as_ref().unwrap().this.borrow().clone();
        match t {
            Some(t) => Ok(t),
            None => throw!(self, "ReferenceError", "Must call super constructor in derived class before accessing 'this' or returning from derived constructor"),
        }
    }

    fn call_closure(&mut self, c: &Rc<Closure>, fo: &Obj, this: Value, args: &[Value], nt: Value) -> R<Value> {
        let def = c.def.clone();
        let env = if def.is_arrow {
            Scope::new(Some(c.env.clone()))
        } else {
            let this = if this.is_nullish() && !def.is_method { Value::Obj(self.global.clone()) } else { this };
            Scope::new_func(Some(c.env.clone()), FuncScope { this: RefCell::new(Some(this)), home: c.home.borrow().clone(), new_target: nt, func: Some(fo.clone()) })
        };
        if def.is_generator {
            return self.start_generator(c.clone(), env, args.to_vec());
        }
        if def.is_async {
            return Ok(self.start_async(c.clone(), env, args.to_vec()));
        }
        self.run_body(c, &env, args)
    }

    fn bind_params(&mut self, c: &Closure, env: &Env, args: &[Value]) -> R<()> {
        let def = &c.def;
        if def.uses_arguments && !def.is_arrow {
            let ao = Obj::new(ObjKind::Array(args.to_vec()), Some(self.p.object.clone()), "Arguments");
            ao.borrow_mut().kind = ObjKind::Array(args.to_vec());
            ao.borrow_mut().proto = Some(self.p.object.clone());
            // make it iterable
            let it = self.get_from(&self.p.array.clone(), &PropKey::Sym(self.sym_iterator.clone()), &Value::Undefined)?;
            ao.borrow_mut().props.insert(PropKey::Sym(self.sym_iterator.clone()), Prop::hidden(it));
            env.declare(Rc::from("arguments"), Value::Obj(ao), true);
        }
        if def.simple_params {
            for (i, p) in def.params.iter().enumerate() {
                if let Pattern::Ident(n) = &p.pattern {
                    env.declare(n.clone(), args.get(i).cloned().unwrap_or(Value::Undefined), true);
                }
            }
        } else {
            for (i, p) in def.params.iter().enumerate() {
                let mut v = args.get(i).cloned().unwrap_or(Value::Undefined);
                if v.is_undefined() {
                    if let Some(d) = &p.default {
                        v = self.eval(d, env)?;
                    }
                }
                self.bind_pattern(&p.pattern, v, env, Some(DeclKind::Let))?;
            }
            if let Some(r) = &def.rest {
                let rest = if args.len() > def.params.len() { args[def.params.len()..].to_vec() } else { vec![] };
                let arr = self.new_array(rest);
                self.bind_pattern(r, arr, env, Some(DeclKind::Let))?;
            }
        }
        for n in &def.var_names {
            if !env.vars.borrow().contains_key(n) {
                env.declare(n.clone(), Value::Undefined, true);
            }
        }
        Ok(())
    }

    pub(crate) fn run_body(&mut self, c: &Closure, env: &Env, args: &[Value]) -> R<Value> {
        self.bind_params(c, env, args)?;
        match &c.def.body {
            FuncBody::Expr(e) => self.eval(e, env),
            FuncBody::Block(stmts) => {
                self.hoist(stmts, env)?;
                match self.exec_stmts(stmts, env)? {
                    Flow::Return(v) => Ok(v),
                    _ => Ok(Value::Undefined),
                }
            }
        }
    }

    // ------------------------------------------------------------ generators & async

    fn start_generator(&mut self, c: Rc<Closure>, env: Env, args: Vec<Value>) -> R<Value> {
        // bind params eagerly so argument errors surface at call time
        self.bind_params(&c, &env, &args)?;
        let body: Box<dyn FnOnce(&mut Interp) -> R<Value>> = Box::new(move |it: &mut Interp| {
            match &c.def.body {
                FuncBody::Expr(e) => it.eval(e, &env),
                FuncBody::Block(stmts) => {
                    it.hoist(stmts, &env)?;
                    match it.exec_stmts(stmts, &env)? {
                        Flow::Return(v) => Ok(v),
                        _ => Ok(Value::Undefined),
                    }
                }
            }
        });
        let co = Rc::new(Coroutine::new(body, false));
        self.coroutines.retain(|w| w.strong_count() > 0);
        self.coroutines.push(Rc::downgrade(&co));
        let g = Obj::new(ObjKind::Generator(co), Some(self.p.generator.clone()), "Generator");
        Ok(Value::Obj(g))
    }

    pub fn iter_result(&mut self, value: Value, done: bool) -> Value {
        let o = self.new_object();
        o.set_prop("value", value);
        o.set_prop("done", Value::Bool(done));
        Value::Obj(o)
    }

    pub fn generator_resume(&mut self, g: &Value, msg: Resume) -> R<Value> {
        let co = match g.as_obj().map(|o| match &o.borrow().kind {
            ObjKind::Generator(c) => Some(c.clone()),
            _ => None,
        }) {
            Some(Some(c)) => c,
            _ => throw!(self, "TypeError", "next method called on incompatible receiver"),
        };
        match co.resume(self, msg) {
            Yield::Yielded(v) => Ok(self.iter_result(v, false)),
            Yield::Done(Ok(v)) => Ok(self.iter_result(v, true)),
            Yield::Done(Err(e)) => Err(e),
            Yield::Awaiting(_) => throw!(self, "TypeError", "await is not supported inside generators"),
        }
    }

    fn start_async(&mut self, c: Rc<Closure>, env: Env, args: Vec<Value>) -> Value {
        let (promise, resolve, reject) = super::builtins::promise::new_capability(self);
        let body: Box<dyn FnOnce(&mut Interp) -> R<Value>> = Box::new(move |it: &mut Interp| it.run_body(&c, &env, &args));
        let co = Rc::new(Coroutine::new(body, true));
        self.coroutines.retain(|w| w.strong_count() > 0);
        self.coroutines.push(Rc::downgrade(&co));
        drive_async(self, co, Resume::Next(Value::Undefined), resolve, reject);
        promise
    }

    pub fn await_value(&mut self, v: Value) -> R<Value> {
        if self.in_async_coroutine() {
            return self.co_suspend(Yield::Awaiting(v));
        }
        // Await outside of an async coroutine (e.g. top-level): drain microtasks synchronously.
        let p = super::builtins::promise::promise_resolve(self, v)?;
        for _ in 0..100_000 {
            let st = super::builtins::promise::state_of(&p);
            match st {
                Some((PromiseStatus::Fulfilled, v)) => return Ok(v),
                Some((PromiseStatus::Rejected, e)) => {
                    super::builtins::promise::mark_handled(&p);
                    return Err(e);
                }
                _ => {}
            }
            if !self.run_one_microtask() {
                break;
            }
        }
        throw!(self, "Error", "top-level await of a promise that does not settle synchronously is not supported")
    }

    // ------------------------------------------------------------ microtasks

    pub fn enqueue(&mut self, job: Job) {
        self.microtasks.push_back(job);
    }

    pub fn run_one_microtask(&mut self) -> bool {
        let Some(job) = self.microtasks.pop_front() else { return false };
        let r = match job {
            Job::Call(f, args) => self.call(&f, Value::Undefined, &args).map(|_| ()),
            Job::Native(f) => f(self),
        };
        if let Err(e) = r {
            if !self.is_marker(&e) {
                let msg = self.describe_error(&e);
                self.console.push(ConsoleMsg { level: "error", text: format!("Uncaught (in promise) {msg}") });
            }
        }
        true
    }

    pub fn run_microtasks(&mut self) {
        let mut n = 0;
        while self.run_one_microtask() {
            n += 1;
            if n > 1_000_000 {
                self.microtasks.clear();
                break;
            }
        }
        // report unhandled rejections
        let un = std::mem::take(&mut self.unhandled);
        for p in un {
            if let Some((PromiseStatus::Rejected, e)) = super::builtins::promise::state_of(&p) {
                if !super::builtins::promise::is_handled(&p) {
                    let msg = self.describe_error(&e);
                    self.console.push(ConsoleMsg { level: "error", text: format!("Uncaught (in promise) {msg}") });
                }
            }
        }
    }

    // ------------------------------------------------------------ scripts

    /// Run a classic script; returns the completion value.
    pub fn eval_script(&mut self, src: &str, name: &str) -> R<Value> {
        self.script_name = name.to_string();
        let (stmts, var_names) = match super::parser::parse_program(src, 0) {
            Ok(r) => r,
            Err(e) => {
                let msg = e.trim_start_matches("SyntaxError: ").to_string();
                return Err(self.make_error("SyntaxError", &msg));
            }
        };
        let global = Value::Obj(self.global.clone());
        for n in var_names {
            let k = PropKey::Str(n.clone());
            if !self.global.borrow().props.contains(&k) && !self.global_env.vars.borrow().contains_key(&n) {
                self.global.borrow_mut().props.insert(k, Prop::data(Value::Undefined));
            }
        }
        let _ = global;
        let env = self.global_env.clone();
        self.hoist_global(&stmts)?;
        self.completion = Value::Undefined;
        for s in &stmts {
            match self.exec(s, &env)? {
                Flow::Normal => {}
                Flow::Return(v) => return Ok(v),
                _ => break,
            }
        }
        Ok(self.take_completion())
    }

    fn hoist_global(&mut self, stmts: &[Stmt]) -> R<()> {
        for s in stmts {
            if let StmtKind::Function(f) = &s.kind {
                let fv = self.make_function(f.clone(), &self.global_env.clone(), None);
                if let Some(n) = &f.name {
                    self.global.borrow_mut().props.insert(PropKey::Str(n.clone()), Prop::data(fv));
                }
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ variables

    pub fn lookup(&mut self, name: &JsStr, env: &Env) -> R<Value> {
        let mut cur = Some(env);
        while let Some(s) = cur {
            let link = match s.vars.borrow().get(name) {
                Some(b) => match &b.link {
                    None => return Ok(b.value.clone()),
                    Some(l) => Some(l.clone()),
                },
                None => None,
            };
            if let Some(l) = link {
                return match read_binding(&l.0, &l.1) {
                    Some(v) => Ok(v),
                    None => throw!(self, "ReferenceError", "Cannot access '{name}' before initialization"),
                };
            }
            cur = s.parent.as_ref();
        }
        let key = PropKey::Str(name.clone());
        let g = self.global.clone();
        if self.has_property(&g, &key) {
            return self.get_from(&g, &key, &Value::Obj(g.clone()));
        }
        match &**name {
            "undefined" => Ok(Value::Undefined),
            "NaN" => Ok(Value::Num(f64::NAN)),
            "Infinity" => Ok(Value::Num(f64::INFINITY)),
            _ => throw!(self, "ReferenceError", "{name} is not defined"),
        }
    }

    pub fn assign_var(&mut self, name: &JsStr, value: Value, env: &Env) -> R<()> {
        let mut cur = Some(env);
        while let Some(s) = cur {
            if let Some(b) = s.vars.borrow_mut().get_mut(name) {
                if !b.mutable {
                    throw!(self, "TypeError", "Assignment to constant variable.");
                }
                b.value = value;
                return Ok(());
            }
            cur = s.parent.as_ref();
        }
        let g = Value::Obj(self.global.clone());
        self.set(&g, PropKey::Str(name.clone()), value)
    }

    fn this_value(&mut self, env: &Env) -> R<Value> {
        let mut cur = Some(env);
        while let Some(s) = cur {
            if let Some(f) = &s.func {
                return match &*f.this.borrow() {
                    Some(v) => Ok(v.clone()),
                    None => throw!(self, "ReferenceError", "Must call super constructor in derived class before accessing 'this'"),
                };
            }
            cur = s.parent.as_ref();
        }
        Ok(Value::Undefined)
    }

    fn func_scope<'a>(&self, env: &'a Env) -> Option<&'a FuncScope> {
        let mut cur = Some(env);
        while let Some(s) = cur {
            if let Some(f) = &s.func {
                return Some(f);
            }
            cur = s.parent.as_ref();
        }
        None
    }

    // ------------------------------------------------------------ statements

    pub fn hoist(&mut self, stmts: &[Stmt], env: &Env) -> R<()> {
        for s in stmts {
            if let StmtKind::Function(f) = &s.kind {
                let fv = self.make_function(f.clone(), env, None);
                if let Some(n) = &f.name {
                    env.declare(n.clone(), fv, true);
                }
            }
        }
        Ok(())
    }

    pub fn exec_stmts(&mut self, stmts: &[Stmt], env: &Env) -> R<Flow> {
        for s in stmts {
            match self.exec(s, env)? {
                Flow::Normal => {}
                f => return Ok(f),
            }
        }
        Ok(Flow::Normal)
    }

    fn exec_block(&mut self, b: &Block, env: &Env) -> R<Flow> {
        if b.scoped {
            let inner = Scope::new(Some(env.clone()));
            self.hoist(&b.body, &inner)?;
            self.exec_stmts(&b.body, &inner)
        } else {
            self.exec_stmts(&b.body, env)
        }
    }

    pub fn exec(&mut self, s: &Stmt, env: &Env) -> R<Flow> {
        self.cur_line = s.line;
        match &s.kind {
            StmtKind::Expr(e) => {
                self.completion = self.eval(e, env)?;
                Ok(Flow::Normal)
            }
            StmtKind::Decl(kind, decls) => {
                for (p, init) in decls {
                    let v = match init {
                        Some(e) => {
                            let v = self.eval(e, env)?;
                            if let Pattern::Ident(n) = p {
                                if is_anon_fn(e) {
                                    self.maybe_name(&v, &PropKey::Str(n.clone()));
                                }
                            }
                            v
                        }
                        None => {
                            if *kind == DeclKind::Var {
                                continue;
                            }
                            Value::Undefined
                        }
                    };
                    self.bind_pattern(p, v, env, Some(*kind))?;
                }
                Ok(Flow::Normal)
            }
            StmtKind::Function(_) => Ok(Flow::Normal),
            StmtKind::Class(c) => {
                let cv = self.eval_class(c, env)?;
                if let Some(n) = &c.name {
                    env.declare(n.clone(), cv, true);
                }
                Ok(Flow::Normal)
            }
            StmtKind::Return(e) => {
                let v = match e {
                    Some(e) => self.eval(e, env)?,
                    None => Value::Undefined,
                };
                Ok(Flow::Return(v))
            }
            StmtKind::If(t, c, a) => {
                if self.eval(t, env)?.truthy() {
                    self.exec(c, env)
                } else if let Some(a) = a {
                    self.exec(a, env)
                } else {
                    Ok(Flow::Normal)
                }
            }
            StmtKind::Block(b) => self.exec_block(b, env),
            StmtKind::Empty | StmtKind::Debugger => Ok(Flow::Normal),
            StmtKind::Throw(e) => {
                let v = self.eval(e, env)?;
                Err(v)
            }
            StmtKind::Break(l) => Ok(Flow::Break(l.clone())),
            StmtKind::Continue(l) => Ok(Flow::Continue(l.clone())),
            StmtKind::Labeled(label, body) => {
                let r = self.exec_labeled(body, env, Some(label))?;
                match r {
                    Flow::Break(Some(l)) if &l == label => Ok(Flow::Normal),
                    f => Ok(f),
                }
            }
            StmtKind::While(..) | StmtKind::DoWhile(..) | StmtKind::For { .. } | StmtKind::ForIn { .. } | StmtKind::ForOf { .. } => self.exec_labeled(s, env, None),
            StmtKind::Try { block, param, handler, finalizer } => {
                let r = self.exec_block(block, env);
                let r = match r {
                    Err(e) if handler.is_some() && !self.is_marker(&e) => {
                        let h = handler.as_ref().unwrap();
                        let cenv = Scope::new(Some(env.clone()));
                        if let Some(p) = param {
                            self.bind_pattern(p, e, &cenv, Some(DeclKind::Let))?;
                        }
                        self.hoist(&h.body, &cenv)?;
                        self.exec_stmts(&h.body, &cenv)
                    }
                    r => r,
                };
                if let Some(f) = finalizer {
                    match self.exec_block(f, env)? {
                        Flow::Normal => r,
                        other => Ok(other),
                    }
                } else {
                    r
                }
            }
            StmtKind::Switch(disc, cases, scoped) => {
                let d = self.eval(disc, env)?;
                let senv = if *scoped { Scope::new(Some(env.clone())) } else { env.clone() };
                if *scoped {
                    for c in cases {
                        self.hoist(&c.body, &senv)?;
                    }
                }
                let mut matched = None;
                for (i, c) in cases.iter().enumerate() {
                    if let Some(t) = &c.test {
                        let tv = self.eval(t, &senv)?;
                        if strict_equals(&d, &tv) {
                            matched = Some(i);
                            break;
                        }
                    }
                }
                if matched.is_none() {
                    matched = cases.iter().position(|c| c.test.is_none());
                }
                if let Some(start) = matched {
                    for c in &cases[start..] {
                        match self.exec_stmts(&c.body, &senv)? {
                            Flow::Normal => {}
                            Flow::Break(None) => return Ok(Flow::Normal),
                            f => return Ok(f),
                        }
                    }
                }
                Ok(Flow::Normal)
            }
        }
    }

    /// Execute a (possibly labeled) loop statement.
    fn exec_labeled(&mut self, s: &Stmt, env: &Env, label: Option<&Name>) -> R<Flow> {
        macro_rules! body_flow {
            ($flow:expr) => {
                match $flow {
                    Flow::Normal => {}
                    Flow::Break(None) => break,
                    Flow::Break(Some(l)) => {
                        if label == Some(&l) {
                            break;
                        }
                        return Ok(Flow::Break(Some(l)));
                    }
                    Flow::Continue(None) => {}
                    Flow::Continue(Some(l)) => {
                        if label != Some(&l) {
                            return Ok(Flow::Continue(Some(l)));
                        }
                    }
                    Flow::Return(v) => return Ok(Flow::Return(v)),
                }
            };
        }
        match &s.kind {
            StmtKind::While(test, body) => {
                while self.eval(test, env)?.truthy() {
                    self.check_deadline()?;
                    let f = self.exec(body, env)?;
                    body_flow!(f);
                }
                Ok(Flow::Normal)
            }
            StmtKind::DoWhile(body, test) => {
                loop {
                    self.check_deadline()?;
                    let f = self.exec(body, env)?;
                    body_flow!(f);
                    if !self.eval(test, env)?.truthy() {
                        break;
                    }
                }
                Ok(Flow::Normal)
            }
            StmtKind::For { init, test, update, body, per_iteration } => {
                let mut fenv = Scope::new(Some(env.clone()));
                match init {
                    Some(ForInit::Decl(kind, decls)) => {
                        for (p, i) in decls {
                            let v = match i {
                                Some(e) => self.eval(e, &fenv)?,
                                None => Value::Undefined,
                            };
                            self.bind_pattern(p, v, &fenv, Some(*kind))?;
                        }
                    }
                    Some(ForInit::Expr(e)) => {
                        self.eval(e, &fenv)?;
                    }
                    None => {}
                }
                loop {
                    if let Some(t) = test {
                        if !self.eval(t, &fenv)?.truthy() {
                            break;
                        }
                    }
                    self.check_deadline()?;
                    let f = self.exec(body, &fenv)?;
                    body_flow!(f);
                    if !per_iteration.is_empty() {
                        // fresh binding per iteration so closures capture distinct values
                        let next = Scope::new(Some(env.clone()));
                        for n in per_iteration {
                            let v = fenv.vars.borrow().get(n).map(|b| b.value.clone()).unwrap_or(Value::Undefined);
                            next.declare(n.clone(), v, true);
                        }
                        fenv = next;
                    }
                    if let Some(u) = update {
                        self.eval(u, &fenv)?;
                    }
                }
                Ok(Flow::Normal)
            }
            StmtKind::ForIn { target, obj, body } => {
                let o = self.eval(obj, env)?;
                if o.is_nullish() {
                    return Ok(Flow::Normal);
                }
                let o = self.to_object(&o)?;
                let mut keys: Vec<PropKey> = Vec::new();
                let mut cur = Some(o.clone());
                let mut seen = std::collections::HashSet::new();
                while let Some(c) = cur {
                    for k in self.own_keys(&c, false) {
                        if seen.insert(k.clone()) {
                            keys.push(k);
                        }
                    }
                    // shadowing by non-enumerable own props
                    for (k, _) in c.borrow().props.iter() {
                        seen.insert(k.clone());
                    }
                    cur = c.proto();
                    if cur.as_ref().is_some_and(|p| p.ptr_eq(&self.p.object)) {
                        break;
                    }
                }
                for k in keys {
                    // skip keys deleted during iteration
                    if !self.has_property(&o, &k) {
                        continue;
                    }
                    self.check_deadline()?;
                    let ienv = Scope::new(Some(env.clone()));
                    self.bind_for_target(target, k.to_value(), &ienv)?;
                    let f = self.exec(body, &ienv)?;
                    body_flow!(f);
                }
                Ok(Flow::Normal)
            }
            StmtKind::ForOf { target, iter, body, is_await } => {
                let v = self.eval(iter, env)?;
                let mut it = self.get_iterator(&v)?;
                loop {
                    self.check_deadline()?;
                    let Some(mut item) = self.iter_step(&mut it)? else { break };
                    if *is_await {
                        item = self.await_value(item)?;
                    }
                    let ienv = Scope::new(Some(env.clone()));
                    if let Err(e) = self.bind_for_target(target, item, &ienv) {
                        self.iter_close(&it);
                        return Err(e);
                    }
                    let f = match self.exec(body, &ienv) {
                        Ok(f) => f,
                        Err(e) => {
                            self.iter_close(&it);
                            return Err(e);
                        }
                    };
                    match f {
                        Flow::Normal | Flow::Continue(None) => {}
                        Flow::Continue(Some(ref l)) if label == Some(l) => {}
                        Flow::Break(None) => {
                            self.iter_close(&it);
                            break;
                        }
                        Flow::Break(Some(ref l)) if label == Some(l) => {
                            self.iter_close(&it);
                            break;
                        }
                        other => {
                            self.iter_close(&it);
                            return Ok(other);
                        }
                    }
                }
                Ok(Flow::Normal)
            }
            _ => self.exec(s, env),
        }
    }

    fn bind_for_target(&mut self, t: &ForTarget, v: Value, env: &Env) -> R<()> {
        match t {
            ForTarget::Decl(k, p) => self.bind_pattern(p, v, env, Some(*k)),
            ForTarget::Pattern(p) => self.bind_pattern(p, v, env, None),
        }
    }

    // ------------------------------------------------------------ iteration

    pub fn get_iterator(&mut self, v: &Value) -> R<Iter> {
        if let Value::Obj(o) = v {
            let fast = {
                let b = o.borrow();
                matches!(b.kind, ObjKind::Array(_)) && !b.props.contains(&PropKey::Sym(self.sym_iterator.clone()))
            };
            if fast && self.array_iter_pristine() {
                return Ok(Iter::Array(o.clone(), 0));
            }
        }
        if let Value::Str(s) = v {
            return Ok(Iter::List(s.chars().map(|c| Value::Str(Rc::from(c.to_string().as_str()))).collect(), 0));
        }
        let f = if v.is_nullish() { Value::Undefined } else { self.get(v, &PropKey::Sym(self.sym_iterator.clone()))? };
        if !f.is_callable() {
            throw!(self, "TypeError", "{} is not iterable", self.short_desc(v));
        }
        let iter = self.call(&f, v.clone(), &[])?;
        if !matches!(iter, Value::Obj(_)) {
            throw!(self, "TypeError", "Result of the Symbol.iterator method is not an object");
        }
        let next = self.get_str(&iter, "next")?;
        Ok(Iter::Generic(iter, next))
    }

    fn array_iter_pristine(&self) -> bool {
        true
    }

    pub fn iter_step(&mut self, it: &mut Iter) -> R<Option<Value>> {
        match it {
            Iter::Array(o, i) => {
                let v = match &o.borrow().kind {
                    ObjKind::Array(items) => items.get(*i).cloned(),
                    _ => None,
                };
                *i += 1;
                Ok(v)
            }
            Iter::List(items, i) => {
                let v = items.get(*i).cloned();
                *i += 1;
                Ok(v)
            }
            Iter::Generic(iter, next) => {
                let r = self.call(next, iter.clone(), &[])?;
                if !matches!(r, Value::Obj(_)) {
                    throw!(self, "TypeError", "Iterator result is not an object");
                }
                if self.get_str(&r, "done")?.truthy() {
                    return Ok(None);
                }
                Ok(Some(self.get_str(&r, "value")?))
            }
        }
    }

    pub fn iter_close(&mut self, it: &Iter) {
        if let Iter::Generic(iter, _) = it {
            if let Ok(f) = self.get_str(iter, "return") {
                if f.is_callable() {
                    let _ = self.call(&f, iter.clone(), &[]);
                }
            }
        }
    }

    pub fn iterate_to_vec(&mut self, v: &Value) -> R<Vec<Value>> {
        if let Value::Obj(o) = v {
            if let ObjKind::Array(items) = &o.borrow().kind {
                return Ok(items.clone());
            }
        }
        let mut it = self.get_iterator(v)?;
        let mut out = Vec::new();
        while let Some(x) = self.iter_step(&mut it)? {
            out.push(x);
            if out.len() > 50_000_000 {
                throw!(self, "RangeError", "iterable too large");
            }
        }
        Ok(out)
    }

    // ------------------------------------------------------------ patterns

    pub fn bind_pattern(&mut self, p: &Pattern, v: Value, env: &Env, kind: Option<DeclKind>) -> R<()> {
        match p {
            Pattern::Ident(n) => match kind {
                Some(DeclKind::Let) => {
                    env.declare(n.clone(), v, true);
                    Ok(())
                }
                Some(DeclKind::Const) => {
                    env.declare(n.clone(), v, false);
                    Ok(())
                }
                Some(DeclKind::Var) | None => self.assign_var(n, v, env),
            },
            Pattern::Expr(e) => self.assign_to_expr(e, v, env),
            Pattern::Object { props, rest } => {
                if v.is_nullish() {
                    throw!(self, "TypeError", "Cannot destructure '{}' as it is {}.", self.short_desc(&v), if v.is_undefined() { "undefined" } else { "null" });
                }
                let mut used = Vec::new();
                for (k, sub, def) in props {
                    let key = match k {
                        Key::Name(n) => PropKey::Str(n.clone()),
                        Key::Private(n) => PropKey::Str(Rc::from(format!("#{n}").as_str())),
                        Key::Computed(e) => {
                            let kv = self.eval(e, env)?;
                            self.to_key(&kv)?
                        }
                    };
                    let mut val = self.get(&v, &key)?;
                    if val.is_undefined() {
                        if let Some(d) = def {
                            val = self.eval(d, env)?;
                            if let Pattern::Ident(n) = sub {
                                if is_anon_fn(d) {
                                    self.maybe_name(&val, &PropKey::Str(n.clone()));
                                }
                            }
                        }
                    }
                    used.push(key);
                    self.bind_pattern(sub, val, env, kind)?;
                }
                if let Some(r) = rest {
                    let o = self.to_object(&v)?;
                    let out = self.new_object();
                    for k in self.own_keys(&o, false) {
                        if !used.contains(&k) {
                            let val = self.get(&v, &k)?;
                            out.borrow_mut().props.insert(k, Prop::data(val));
                        }
                    }
                    self.bind_pattern(r, Value::Obj(out), env, kind)?;
                }
                Ok(())
            }
            Pattern::Array { elems, rest } => {
                let mut it = self.get_iterator(&v)?;
                let mut done = false;
                for el in elems {
                    let item = if done {
                        None
                    } else {
                        let x = self.iter_step(&mut it)?;
                        if x.is_none() {
                            done = true;
                        }
                        x
                    };
                    if let Some((sub, def)) = el {
                        let mut val = item.unwrap_or(Value::Undefined);
                        if val.is_undefined() {
                            if let Some(d) = def {
                                val = self.eval(d, env)?;
                            }
                        }
                        self.bind_pattern(sub, val, env, kind)?;
                    }
                }
                if let Some(r) = rest {
                    let mut items = Vec::new();
                    if !done {
                        while let Some(x) = self.iter_step(&mut it)? {
                            items.push(x);
                        }
                    }
                    let arr = self.new_array(items);
                    self.bind_pattern(r, arr, env, kind)?;
                } else if !done {
                    self.iter_close(&it);
                }
                Ok(())
            }
        }
    }

    fn assign_to_expr(&mut self, e: &Expr, v: Value, env: &Env) -> R<()> {
        match e {
            Expr::Ident(n) => self.assign_var(n, v, env),
            Expr::Member { obj, prop, .. } => {
                if let Expr::Super = **obj {
                    let this = self.this_value(env)?;
                    let key = self.member_key(prop, env, &this)?;
                    return self.set(&this, key, v);
                }
                let o = self.eval(obj, env)?;
                let key = self.member_key(prop, env, &o)?;
                self.set(&o, key, v)
            }
            _ => throw!(self, "SyntaxError", "Invalid assignment target"),
        }
    }

    fn member_key(&mut self, prop: &Key, env: &Env, _obj: &Value) -> R<PropKey> {
        Ok(match prop {
            Key::Name(n) => PropKey::Str(n.clone()),
            Key::Private(n) => PropKey::Str(Rc::from(format!("#{n}").as_str())),
            Key::Computed(e) => {
                let k = self.eval(e, env)?;
                self.to_key(&k)?
            }
        })
    }

    pub fn maybe_name(&mut self, v: &Value, key: &PropKey) {
        if let Value::Obj(o) = v {
            if !o.is_callable() {
                return;
            }
            let has_name = o.borrow().props.get(&PropKey::from("name")).is_some_and(|p| matches!(&p.value, Value::Str(s) if !s.is_empty()));
            if !has_name {
                let n = match key {
                    PropKey::Str(s) => s.to_string(),
                    PropKey::Sym(s) => format!("[{}]", s.0.description.as_deref().unwrap_or("")),
                };
                o.borrow_mut().props.insert(PropKey::from("name"), Prop { value: Value::str(&n), getter: None, setter: None, enumerable: false, writable: false, configurable: true });
            }
        }
    }

    // ------------------------------------------------------------ function objects

    pub fn make_function(&mut self, def: Rc<FuncDef>, env: &Env, home: Option<Obj>) -> Value {
        let c = Rc::new(Closure { def: def.clone(), env: env.clone(), home: RefCell::new(home), class: None });
        let proto = if def.is_generator { self.p.function.clone() } else { self.p.function.clone() };
        let f = Obj::new(ObjKind::Function(Func::Closure(c)), Some(proto), "Function");
        {
            let mut b = f.borrow_mut();
            b.props.insert(PropKey::from("name"), Prop { value: Value::str(def.name.as_deref().unwrap_or("")), getter: None, setter: None, enumerable: false, writable: false, configurable: true });
            b.props.insert(PropKey::from("length"), Prop { value: Value::Num(def.params.iter().take_while(|p| p.default.is_none()).count() as f64), getter: None, setter: None, enumerable: false, writable: false, configurable: true });
        }
        if def.is_generator {
            let po = Obj::new(ObjKind::Ordinary, Some(self.p.generator.clone()), "Generator");
            f.borrow_mut().props.insert(PropKey::from("prototype"), Prop::hidden(Value::Obj(po)));
        } else if !def.is_arrow && !def.is_method && !def.is_async {
            let po = self.new_object();
            po.set_hidden("constructor", Value::Obj(f.clone()));
            f.borrow_mut().props.insert(PropKey::from("prototype"), Prop { value: Value::Obj(po), getter: None, setter: None, enumerable: false, writable: true, configurable: false });
        }
        Value::Obj(f)
    }

    fn eval_class(&mut self, c: &Rc<ClassDef>, env: &Env) -> R<Value> {
        let cenv = Scope::new(Some(env.clone()));
        let (parent, proto_parent) = match &c.superclass {
            Some(e) => {
                let p = self.eval(e, env)?;
                match &p {
                    Value::Null => (None, None),
                    Value::Obj(o) if o.is_callable() => {
                        let pp = self.get_str(&p, "prototype")?;
                        let pp = match pp {
                            Value::Obj(o) => Some(o),
                            Value::Null => None,
                            _ => throw!(self, "TypeError", "Class extends value does not have valid prototype property"),
                        };
                        (Some(p), pp)
                    }
                    _ => throw!(self, "TypeError", "Class extends value {} is not a constructor or null", self.short_desc(&p)),
                }
            }
            None => (None, Some(self.p.object.clone())),
        };
        let derived = c.superclass.is_some();
        let proto = Obj::new(ObjKind::Ordinary, proto_parent, "Object");
        let ci = Rc::new(ClassInfo { def: c.clone(), env: cenv.clone(), derived, parent: parent.clone(), fields: RefCell::new(vec![]), proto: RefCell::new(Some(proto.clone())) });
        let ctor_def = match &c.constructor {
            Some(f) => f.clone(),
            None => Rc::new(FuncDef {
                name: c.name.clone(),
                params: vec![],
                rest: if derived { Some(Pattern::Ident(Rc::from("args"))) } else { None },
                body: FuncBody::Block(if derived {
                    vec![Stmt {
                        kind: StmtKind::Expr(Expr::Call { callee: Box::new(Expr::Super), args: vec![ArrayElem::Spread(Expr::Ident(Rc::from("args")))], optional: false }),
                        line: 0,
                    }]
                } else {
                    vec![]
                }),
                is_arrow: false,
                is_async: false,
                is_generator: false,
                is_method: true,
                var_names: vec![],
                uses_arguments: false,
                uses_this: false,
                simple_params: !derived,
                line: 0,
                source: Rc::from("class { }"),
            }),
        };
        let closure = Rc::new(Closure { def: ctor_def, env: cenv.clone(), home: RefCell::new(Some(proto.clone())), class: Some(ci.clone()) });
        let fproto = match &parent {
            Some(Value::Obj(p)) => p.clone(),
            _ => self.p.function.clone(),
        };
        let f = Obj::new(ObjKind::Function(Func::Closure(closure)), Some(fproto), "Function");
        {
            let mut b = f.borrow_mut();
            b.props.insert(PropKey::from("name"), Prop { value: Value::str(c.name.as_deref().unwrap_or("")), getter: None, setter: None, enumerable: false, writable: false, configurable: true });
            b.props.insert(PropKey::from("prototype"), Prop { value: Value::Obj(proto.clone()), getter: None, setter: None, enumerable: false, writable: false, configurable: false });
        }
        proto.set_hidden("constructor", Value::Obj(f.clone()));
        if let Some(n) = &c.name {
            cenv.declare(n.clone(), Value::Obj(f.clone()), false);
        }
        let fv = Value::Obj(f.clone());
        let mut static_inits: Vec<usize> = Vec::new();
        for (idx, m) in c.members.iter().enumerate() {
            match m {
                ClassMember::Method { key, kind, is_static, func } => {
                    let target = if *is_static { f.clone() } else { proto.clone() };
                    let k = self.class_key(key, &cenv)?;
                    let mv = self.make_function(func.clone(), &cenv, Some(target.clone()));
                    self.maybe_name(&mv, &k);
                    let mut b = target.borrow_mut();
                    match kind {
                        MethodKind::Method => b.props.insert(k, Prop::hidden(mv)),
                        MethodKind::Get => {
                            let existing = b.props.get(&k).cloned();
                            let setter = existing.and_then(|p| p.setter);
                            b.props.insert(k, Prop { value: Value::Undefined, getter: Some(mv), setter, enumerable: false, writable: true, configurable: true })
                        }
                        MethodKind::Set => {
                            let existing = b.props.get(&k).cloned();
                            let getter = existing.and_then(|p| p.getter);
                            b.props.insert(k, Prop { value: Value::Undefined, getter, setter: Some(mv), enumerable: false, writable: true, configurable: true })
                        }
                    }
                }
                ClassMember::Field { key, is_static, .. } => {
                    let k = self.class_key(key, &cenv)?;
                    if *is_static {
                        static_inits.push(idx);
                        let _ = k;
                    } else {
                        ci.fields.borrow_mut().push((k, Some(idx)));
                    }
                }
                ClassMember::StaticBlock(_) => static_inits.push(idx),
            }
        }
        // static fields and blocks run in order with `this` = the class
        let senv = Scope::new_func(Some(cenv.clone()), FuncScope { this: RefCell::new(Some(fv.clone())), home: Some(f.clone()), new_target: Value::Undefined, func: None });
        for idx in static_inits {
            match &c.members[idx] {
                ClassMember::Field { key, value, .. } => {
                    let k = self.class_key(key, &cenv)?;
                    let v = match value {
                        Some(e) => self.eval(e, &senv)?,
                        None => Value::Undefined,
                    };
                    self.maybe_name(&v, &k);
                    f.borrow_mut().props.insert(k, Prop::data(v));
                }
                ClassMember::StaticBlock(body) => {
                    let benv = Scope::new(Some(senv.clone()));
                    self.hoist(body, &benv)?;
                    self.exec_stmts(body, &benv)?;
                }
                _ => {}
            }
        }
        Ok(fv)
    }

    fn class_key(&mut self, key: &Key, env: &Env) -> R<PropKey> {
        Ok(match key {
            Key::Name(n) => PropKey::Str(n.clone()),
            Key::Private(n) => PropKey::Str(Rc::from(format!("#{n}").as_str())),
            Key::Computed(e) => {
                let v = self.eval(e, env)?;
                self.to_key(&v)?
            }
        })
    }

    // ------------------------------------------------------------ expressions

    fn eval_args(&mut self, args: &[ArrayElem], env: &Env) -> R<Vec<Value>> {
        let mut out = Vec::with_capacity(args.len());
        for a in args {
            match a {
                ArrayElem::Item(e) => out.push(self.eval(e, env)?),
                ArrayElem::Spread(e) => {
                    let v = self.eval(e, env)?;
                    out.extend(self.iterate_to_vec(&v)?);
                }
                ArrayElem::Hole => out.push(Value::Undefined),
            }
        }
        Ok(out)
    }

    fn short_circuit(&self) -> Value {
        Value::Obj(self.short_circuit.clone())
    }

    pub fn eval(&mut self, e: &Expr, env: &Env) -> R<Value> {
        match e {
            Expr::Num(n) => Ok(Value::Num(*n)),
            Expr::Str(s) => Ok(Value::Str(s.clone())),
            Expr::BigInt(s) => Ok(Value::BigInt(s.parse().unwrap_or(0))),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Null => Ok(Value::Null),
            Expr::Ident(n) => self.lookup(n, env),
            Expr::This => self.this_value(env),
            Expr::NewTarget => Ok(self.func_scope(env).map(|f| f.new_target.clone()).unwrap_or(Value::Undefined)),
            Expr::Template { quasis, exprs } => {
                let mut s = String::new();
                for (i, q) in quasis.iter().enumerate() {
                    s.push_str(q);
                    if let Some(e) = exprs.get(i) {
                        let v = self.eval(e, env)?;
                        s.push_str(&self.to_str(&v)?);
                    }
                }
                Ok(Value::Str(Rc::from(s.as_str())))
            }
            Expr::TaggedTemplate { tag, cooked, raw, exprs } => {
                let (f, this) = self.eval_callee(tag, env)?;
                let strings = self.new_array(cooked.iter().map(|s| Value::Str(s.clone())).collect());
                let raws = self.new_array(raw.iter().map(|s| Value::Str(s.clone())).collect());
                if let Value::Obj(o) = &strings {
                    o.set_hidden("raw", raws);
                }
                let mut args = vec![strings];
                for e in exprs {
                    args.push(self.eval(e, env)?);
                }
                self.call(&f, this, &args)
            }
            Expr::Regex { pattern, flags } => super::builtins::regexp::new_regexp(self, pattern, flags),
            Expr::Array(elems) => {
                let mut items = Vec::with_capacity(elems.len());
                for el in elems {
                    match el {
                        ArrayElem::Item(e) => items.push(self.eval(e, env)?),
                        ArrayElem::Spread(e) => {
                            let v = self.eval(e, env)?;
                            items.extend(self.iterate_to_vec(&v)?);
                        }
                        ArrayElem::Hole => items.push(Value::Undefined),
                    }
                }
                Ok(self.new_array(items))
            }
            Expr::Object(props) => self.eval_object(props, env),
            Expr::Function(def) => {
                if let (Some(n), false) = (&def.name, def.is_arrow) {
                    // named function expressions can refer to themselves
                    let fenv = Scope::new(Some(env.clone()));
                    let f = self.make_function(def.clone(), &fenv, None);
                    fenv.declare(n.clone(), f.clone(), false);
                    return Ok(f);
                }
                Ok(self.make_function(def.clone(), env, None))
            }
            Expr::Class(c) => self.eval_class(c, env),
            Expr::Unary(op, arg) => self.eval_unary(*op, arg, env),
            Expr::Update { inc, prefix, target } => {
                let old = self.eval(target, env)?;
                let oldn = self.to_numeric(&old)?;
                let newv = match &oldn {
                    Value::BigInt(b) => Value::BigInt(if *inc { b + 1 } else { b - 1 }),
                    Value::Num(n) => Value::Num(if *inc { n + 1.0 } else { n - 1.0 }),
                    _ => Value::Num(f64::NAN),
                };
                self.assign_to_expr(target, newv.clone(), env)?;
                Ok(if *prefix { newv } else { oldn })
            }
            Expr::Binary(op, l, r) => {
                let a = self.eval(l, env)?;
                let b = self.eval(r, env)?;
                self.binary(*op, &a, &b)
            }
            Expr::Logical(op, l, r) => {
                let a = self.eval(l, env)?;
                match op {
                    LogicOp::And => {
                        if a.truthy() {
                            self.eval(r, env)
                        } else {
                            Ok(a)
                        }
                    }
                    LogicOp::Or => {
                        if a.truthy() {
                            Ok(a)
                        } else {
                            self.eval(r, env)
                        }
                    }
                    LogicOp::Nullish => {
                        if a.is_nullish() {
                            self.eval(r, env)
                        } else {
                            Ok(a)
                        }
                    }
                }
            }
            Expr::Assign(op, target, value) => self.eval_assign(*op, target, value, env),
            Expr::Cond(t, a, b) => {
                if self.eval(t, env)?.truthy() {
                    self.eval(a, env)
                } else {
                    self.eval(b, env)
                }
            }
            Expr::Seq(v) => {
                let mut last = Value::Undefined;
                for e in v {
                    last = self.eval(e, env)?;
                }
                Ok(last)
            }
            Expr::Member { obj, prop, optional } => {
                if let Expr::Super = **obj {
                    let home = self.func_scope_home(env);
                    let this = self.this_value(env)?;
                    let key = self.member_key(prop, env, &this)?;
                    let proto = home.and_then(|h| h.proto());
                    return match proto {
                        Some(p) => self.get_from(&p, &key, &this),
                        None => Ok(Value::Undefined),
                    };
                }
                let o = self.eval(obj, env)?;
                if *optional && o.is_nullish() {
                    return Err(self.short_circuit());
                }
                // fast path: array[index]
                if let (Key::Computed(ke), Value::Obj(ao)) = (&**prop, &o) {
                    let k = self.eval(ke, env)?;
                    if let Value::Num(n) = k {
                        if n >= 0.0 && n.fract() == 0.0 {
                            if let ObjKind::Array(items) = &ao.borrow().kind {
                                if let Some(v) = items.get(n as usize) {
                                    return Ok(v.clone());
                                }
                            }
                        }
                    }
                    let key = self.to_key(&k)?;
                    return self.get(&o, &key);
                }
                let key = self.member_key(prop, env, &o)?;
                self.get(&o, &key)
            }
            Expr::OptChain(inner) => match self.eval(inner, env) {
                Err(Value::Obj(o)) if o.ptr_eq(&self.short_circuit) => Ok(Value::Undefined),
                r => r,
            },
            Expr::Call { callee, args, optional } => {
                if let Expr::Super = **callee {
                    return self.super_call(args, env);
                }
                let (f, this) = self.eval_callee(callee, env)?;
                if *optional && f.is_nullish() {
                    return Err(self.short_circuit());
                }
                let argv = self.eval_args(args, env)?;
                if !f.is_callable() {
                    let desc = callee_desc(callee);
                    throw!(self, "TypeError", "{desc} is not a function");
                }
                self.call(&f, this, &argv)
            }
            Expr::New { callee, args } => {
                let f = self.eval(callee, env)?;
                let argv = self.eval_args(args, env)?;
                if !f.is_callable() {
                    let desc = callee_desc(callee);
                    throw!(self, "TypeError", "{desc} is not a constructor");
                }
                self.construct(&f, &argv, None)
            }
            Expr::Super => throw!(self, "SyntaxError", "'super' keyword unexpected here"),
            Expr::Await(arg) => {
                let v = self.eval(arg, env)?;
                self.await_value(v)
            }
            Expr::Yield { arg, delegate } => {
                let v = match arg {
                    Some(a) => self.eval(a, env)?,
                    None => Value::Undefined,
                };
                if *delegate {
                    let mut it = self.get_iterator(&v)?;
                    let mut last = Value::Undefined;
                    loop {
                        // delegate: we only forward values (not sent values) for simplicity
                        match self.iter_step(&mut it)? {
                            Some(x) => {
                                last = self.co_suspend(Yield::Yielded(x))?;
                            }
                            None => break,
                        }
                    }
                    let _ = last;
                    return Ok(Value::Undefined);
                }
                self.co_suspend(Yield::Yielded(v))
            }
            Expr::PrivateIn(n, obj) => {
                let o = self.eval(obj, env)?;
                let Value::Obj(o) = o else { throw!(self, "TypeError", "Cannot use 'in' operator to search for '#{n}' in non-object") };
                let k = PropKey::Str(Rc::from(format!("#{n}").as_str()));
                Ok(Value::Bool(self.has_property(&o, &k)))
            }
        }
    }

    fn func_scope_home(&self, env: &Env) -> Option<Obj> {
        let mut cur = Some(env);
        while let Some(s) = cur {
            if let Some(f) = &s.func {
                if f.home.is_some() {
                    return f.home.clone();
                }
            }
            cur = s.parent.as_ref();
        }
        None
    }

    fn super_call(&mut self, args: &[ArrayElem], env: &Env) -> R<Value> {
        let argv = self.eval_args(args, env)?;
        // find the nearest constructor function scope
        let mut cur = Some(env);
        let mut fs: Option<&FuncScope> = None;
        while let Some(s) = cur {
            if let Some(f) = &s.func {
                if f.func.as_ref().is_some_and(|fo| matches!(&fo.borrow().kind, ObjKind::Function(Func::Closure(c)) if c.class.is_some())) {
                    fs = Some(f);
                    break;
                }
            }
            cur = s.parent.as_ref();
        }
        let Some(fs) = fs else { throw!(self, "SyntaxError", "'super' keyword unexpected here") };
        let fo = fs.func.clone().unwrap();
        let ci = match &fo.borrow().kind {
            ObjKind::Function(Func::Closure(c)) => c.class.clone().unwrap(),
            _ => unreachable!(),
        };
        let parent = ci.parent.clone().unwrap_or(Value::Undefined);
        if fs.this.borrow().is_some() {
            throw!(self, "ReferenceError", "Super constructor may only be called once");
        }
        let nt = fs.new_target.clone();
        let result = if parent.is_undefined() {
            Value::Obj(self.object_for_new(&nt)?)
        } else {
            self.construct(&parent, &argv, Some(&nt))?
        };
        *fs.this.borrow_mut() = Some(result.clone());
        if let Value::Obj(o) = &result {
            self.init_fields(&ci, o)?;
        }
        Ok(result)
    }

    /// Evaluate a callee, producing (function, this).
    fn eval_callee(&mut self, callee: &Expr, env: &Env) -> R<(Value, Value)> {
        match callee {
            Expr::Member { obj, prop, optional } => {
                if let Expr::Super = **obj {
                    let home = self.func_scope_home(env);
                    let this = self.this_value(env)?;
                    let key = self.member_key(prop, env, &this)?;
                    let f = match home.and_then(|h| h.proto()) {
                        Some(p) => self.get_from(&p, &key, &this)?,
                        None => Value::Undefined,
                    };
                    return Ok((f, this));
                }
                let o = self.eval(obj, env)?;
                if *optional && o.is_nullish() {
                    return Err(self.short_circuit());
                }
                let key = self.member_key(prop, env, &o)?;
                let f = self.get(&o, &key)?;
                Ok((f, o))
            }
            Expr::OptChain(inner) => match self.eval_callee(inner, env) {
                Err(Value::Obj(o)) if o.ptr_eq(&self.short_circuit) => Ok((Value::Undefined, Value::Undefined)),
                r => r,
            },
            _ => Ok((self.eval(callee, env)?, Value::Undefined)),
        }
    }

    fn eval_object(&mut self, props: &[ObjProp], env: &Env) -> R<Value> {
        let o = self.new_object();
        for p in props {
            match p {
                ObjProp::KeyValue(k, v) => {
                    let key = self.class_key(k, env)?;
                    let val = self.eval(v, env)?;
                    if is_anon_fn(v) {
                        self.maybe_name(&val, &key);
                    }
                    if key.as_str() == Some("__proto__") && matches!(k, Key::Name(_)) {
                        match val {
                            Value::Obj(p) => o.borrow_mut().proto = Some(p),
                            Value::Null => o.borrow_mut().proto = None,
                            _ => {}
                        }
                        continue;
                    }
                    o.borrow_mut().props.insert(key, Prop::data(val));
                }
                ObjProp::Shorthand(n) => {
                    let v = self.lookup(n, env)?;
                    o.borrow_mut().props.insert(PropKey::Str(n.clone()), Prop::data(v));
                }
                ObjProp::Method(k, kind, f) => {
                    let key = self.class_key(k, env)?;
                    let fv = self.make_function(f.clone(), env, Some(o.clone()));
                    self.maybe_name(&fv, &key);
                    let mut b = o.borrow_mut();
                    match kind {
                        MethodKind::Method => b.props.insert(key, Prop::data(fv)),
                        MethodKind::Get => {
                            let setter = b.props.get(&key).and_then(|p| p.setter.clone());
                            b.props.insert(key, Prop { value: Value::Undefined, getter: Some(fv), setter, enumerable: true, writable: true, configurable: true })
                        }
                        MethodKind::Set => {
                            let getter = b.props.get(&key).and_then(|p| p.getter.clone());
                            b.props.insert(key, Prop { value: Value::Undefined, getter, setter: Some(fv), enumerable: true, writable: true, configurable: true })
                        }
                    }
                }
                ObjProp::Spread(e) => {
                    let v = self.eval(e, env)?;
                    if v.is_nullish() {
                        continue;
                    }
                    if let Value::Str(s) = &v {
                        for (i, c) in s.chars().enumerate() {
                            o.set_prop(&i.to_string(), Value::Str(Rc::from(c.to_string().as_str())));
                        }
                        continue;
                    }
                    if let Value::Obj(src) = &v {
                        for k in self.own_keys(src, false) {
                            let val = self.get(&v, &k)?;
                            o.borrow_mut().props.insert(k, Prop::data(val));
                        }
                    }
                }
            }
        }
        Ok(Value::Obj(o))
    }

    fn eval_unary(&mut self, op: UnaryOp, arg: &Expr, env: &Env) -> R<Value> {
        match op {
            UnaryOp::TypeOf => {
                if let Expr::Ident(n) = arg {
                    return match self.lookup(n, env) {
                        Ok(v) => Ok(Value::str(v.type_of())),
                        Err(_) => Ok(Value::str("undefined")),
                    };
                }
                let v = self.eval(arg, env)?;
                Ok(Value::str(v.type_of()))
            }
            UnaryOp::Delete => match arg {
                Expr::Member { obj, prop, .. } => {
                    let o = self.eval(obj, env)?;
                    let k = self.member_key(prop, env, &o)?;
                    Ok(Value::Bool(self.delete(&o, &k)?))
                }
                Expr::OptChain(inner) => match self.eval_unary(UnaryOp::Delete, inner, env) {
                    Err(Value::Obj(o)) if o.ptr_eq(&self.short_circuit) => Ok(Value::Bool(true)),
                    r => r,
                },
                _ => {
                    self.eval(arg, env).ok();
                    Ok(Value::Bool(true))
                }
            },
            UnaryOp::Void => {
                self.eval(arg, env)?;
                Ok(Value::Undefined)
            }
            UnaryOp::Not => Ok(Value::Bool(!self.eval(arg, env)?.truthy())),
            UnaryOp::Neg => {
                let v = self.eval(arg, env)?;
                Ok(match self.to_numeric(&v)? {
                    Value::BigInt(b) => Value::BigInt(-b),
                    Value::Num(n) => Value::Num(-n),
                    _ => Value::Num(f64::NAN),
                })
            }
            UnaryOp::Plus => {
                let v = self.eval(arg, env)?;
                Ok(Value::Num(self.to_number(&v)?))
            }
            UnaryOp::BitNot => {
                let v = self.eval(arg, env)?;
                Ok(match self.to_numeric(&v)? {
                    Value::BigInt(b) => Value::BigInt(!b),
                    Value::Num(n) => Value::Num(!to_int32(n) as f64),
                    _ => Value::Num(-1.0),
                })
            }
        }
    }

    fn eval_assign(&mut self, op: AssignOp, target: &Pattern, value: &Expr, env: &Env) -> R<Value> {
        match op {
            AssignOp::Assign => {
                let v = self.eval(value, env)?;
                if let Pattern::Ident(n) = target {
                    if is_anon_fn(value) {
                        self.maybe_name(&v, &PropKey::Str(n.clone()));
                    }
                }
                self.bind_pattern(target, v.clone(), env, None)?;
                Ok(v)
            }
            AssignOp::Op(bop) => {
                // evaluate the reference once
                match target {
                    Pattern::Ident(n) => {
                        let old = self.lookup(n, env)?;
                        let rhs = self.eval(value, env)?;
                        let nv = self.binary(bop, &old, &rhs)?;
                        self.assign_var(n, nv.clone(), env)?;
                        Ok(nv)
                    }
                    Pattern::Expr(e) => match &**e {
                        Expr::Member { obj, prop, .. } => {
                            let o = self.eval(obj, env)?;
                            let k = self.member_key(prop, env, &o)?;
                            let old = self.get(&o, &k)?;
                            let rhs = self.eval(value, env)?;
                            let nv = self.binary(bop, &old, &rhs)?;
                            self.set(&o, k, nv.clone())?;
                            Ok(nv)
                        }
                        Expr::Ident(n) => {
                            let old = self.lookup(n, env)?;
                            let rhs = self.eval(value, env)?;
                            let nv = self.binary(bop, &old, &rhs)?;
                            self.assign_var(n, nv.clone(), env)?;
                            Ok(nv)
                        }
                        _ => throw!(self, "SyntaxError", "Invalid assignment target"),
                    },
                    _ => throw!(self, "SyntaxError", "Invalid assignment target"),
                }
            }
            AssignOp::Logic(lop) => {
                let (get_old, setter): (Value, Box<dyn Fn(&mut Interp, Value) -> R<()>>) = match target {
                    Pattern::Ident(n) => {
                        let old = self.lookup(n, env)?;
                        let n = n.clone();
                        let env = env.clone();
                        (old, Box::new(move |it: &mut Interp, v| it.assign_var(&n, v, &env)))
                    }
                    Pattern::Expr(e) => match &**e {
                        Expr::Member { obj, prop, .. } => {
                            let o = self.eval(obj, env)?;
                            let k = self.member_key(prop, env, &o)?;
                            let old = self.get(&o, &k)?;
                            (old, Box::new(move |it: &mut Interp, v| it.set(&o, k.clone(), v)))
                        }
                        Expr::Ident(n) => {
                            let old = self.lookup(n, env)?;
                            let n = n.clone();
                            let env = env.clone();
                            (old, Box::new(move |it: &mut Interp, v| it.assign_var(&n, v, &env)))
                        }
                        _ => throw!(self, "SyntaxError", "Invalid assignment target"),
                    },
                    _ => throw!(self, "SyntaxError", "Invalid assignment target"),
                };
                let assign = match lop {
                    LogicOp::And => get_old.truthy(),
                    LogicOp::Or => !get_old.truthy(),
                    LogicOp::Nullish => get_old.is_nullish(),
                };
                if assign {
                    let v = self.eval(value, env)?;
                    setter(self, v.clone())?;
                    Ok(v)
                } else {
                    Ok(get_old)
                }
            }
        }
    }

    pub fn binary(&mut self, op: BinOp, a: &Value, b: &Value) -> R<Value> {
        // fast numeric path
        if let (Value::Num(x), Value::Num(y)) = (a, b) {
            let (x, y) = (*x, *y);
            return Ok(match op {
                BinOp::Add => Value::Num(x + y),
                BinOp::Sub => Value::Num(x - y),
                BinOp::Mul => Value::Num(x * y),
                BinOp::Div => Value::Num(x / y),
                BinOp::Mod => Value::Num(js_mod(x, y)),
                BinOp::Exp => Value::Num(js_pow(x, y)),
                BinOp::Lt => Value::Bool(x < y),
                BinOp::Gt => Value::Bool(x > y),
                BinOp::Le => Value::Bool(x <= y),
                BinOp::Ge => Value::Bool(x >= y),
                BinOp::Eq | BinOp::StrictEq => Value::Bool(x == y),
                BinOp::Ne | BinOp::StrictNe => Value::Bool(x != y),
                BinOp::BitAnd => Value::Num((to_int32(x) & to_int32(y)) as f64),
                BinOp::BitOr => Value::Num((to_int32(x) | to_int32(y)) as f64),
                BinOp::BitXor => Value::Num((to_int32(x) ^ to_int32(y)) as f64),
                BinOp::Shl => Value::Num(to_int32(x).wrapping_shl(to_uint32(y) & 31) as f64),
                BinOp::Shr => Value::Num((to_int32(x) >> (to_uint32(y) & 31)) as f64),
                BinOp::UShr => Value::Num((to_uint32(x) >> (to_uint32(y) & 31)) as f64),
                BinOp::In | BinOp::InstanceOf => return self.binary_slow(op, a, b),
            });
        }
        self.binary_slow(op, a, b)
    }

    fn binary_slow(&mut self, op: BinOp, a: &Value, b: &Value) -> R<Value> {
        match op {
            BinOp::StrictEq => return Ok(Value::Bool(strict_equals(a, b))),
            BinOp::StrictNe => return Ok(Value::Bool(!strict_equals(a, b))),
            BinOp::Eq => return Ok(Value::Bool(self.loose_equals(a, b)?)),
            BinOp::Ne => return Ok(Value::Bool(!self.loose_equals(a, b)?)),
            BinOp::InstanceOf => return Ok(Value::Bool(self.instance_of(a, b)?)),
            BinOp::In => {
                let Value::Obj(o) = b else { throw!(self, "TypeError", "Cannot use 'in' operator to search for '{}' in {}", self.to_string_lossy(a), self.short_desc(b)) };
                let k = self.to_key(a)?;
                return Ok(Value::Bool(self.has_property(o, &k)));
            }
            BinOp::Add => {
                let pa = self.to_primitive(a, "default")?;
                let pb = self.to_primitive(b, "default")?;
                if matches!(pa, Value::Str(_)) || matches!(pb, Value::Str(_)) {
                    let sa = self.to_str(&pa)?;
                    let sb = self.to_str(&pb)?;
                    let mut s = String::with_capacity(sa.len() + sb.len());
                    s.push_str(&sa);
                    s.push_str(&sb);
                    return Ok(Value::Str(Rc::from(s.as_str())));
                }
                if let (Value::BigInt(x), Value::BigInt(y)) = (&pa, &pb) {
                    return Ok(Value::BigInt(x.wrapping_add(*y)));
                }
                let x = self.to_number(&pa)?;
                let y = self.to_number(&pb)?;
                return Ok(Value::Num(x + y));
            }
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                let pa = self.to_primitive(a, "number")?;
                let pb = self.to_primitive(b, "number")?;
                if let (Value::Str(x), Value::Str(y)) = (&pa, &pb) {
                    let xs: Vec<u16> = x.encode_utf16().collect();
                    let ys: Vec<u16> = y.encode_utf16().collect();
                    return Ok(Value::Bool(match op {
                        BinOp::Lt => xs < ys,
                        BinOp::Gt => xs > ys,
                        BinOp::Le => xs <= ys,
                        _ => xs >= ys,
                    }));
                }
                if let (Value::BigInt(x), Value::BigInt(y)) = (&pa, &pb) {
                    return Ok(Value::Bool(match op {
                        BinOp::Lt => x < y,
                        BinOp::Gt => x > y,
                        BinOp::Le => x <= y,
                        _ => x >= y,
                    }));
                }
                let x = self.to_number(&pa)?;
                let y = self.to_number(&pb)?;
                return Ok(Value::Bool(match op {
                    BinOp::Lt => x < y,
                    BinOp::Gt => x > y,
                    BinOp::Le => x <= y,
                    _ => x >= y,
                }));
            }
            _ => {}
        }
        let na = self.to_numeric(a)?;
        let nb = self.to_numeric(b)?;
        if let (Value::BigInt(x), Value::BigInt(y)) = (&na, &nb) {
            let (x, y) = (*x, *y);
            return Ok(Value::BigInt(match op {
                BinOp::Sub => x.wrapping_sub(y),
                BinOp::Mul => x.wrapping_mul(y),
                BinOp::Div => {
                    if y == 0 {
                        throw!(self, "RangeError", "Division by zero");
                    }
                    x / y
                }
                BinOp::Mod => {
                    if y == 0 {
                        throw!(self, "RangeError", "Division by zero");
                    }
                    x % y
                }
                BinOp::Exp => x.wrapping_pow(y.clamp(0, 1000) as u32),
                BinOp::BitAnd => x & y,
                BinOp::BitOr => x | y,
                BinOp::BitXor => x ^ y,
                BinOp::Shl => x.wrapping_shl(y as u32),
                BinOp::Shr => x.wrapping_shr(y as u32),
                _ => throw!(self, "TypeError", "BigInts have no unsigned right shift, use >> instead"),
            }));
        }
        if matches!(na, Value::BigInt(_)) || matches!(nb, Value::BigInt(_)) {
            throw!(self, "TypeError", "Cannot mix BigInt and other types, use explicit conversions");
        }
        let x = if let Value::Num(n) = na { n } else { f64::NAN };
        let y = if let Value::Num(n) = nb { n } else { f64::NAN };
        self.binary(op, &Value::Num(x), &Value::Num(y))
    }

    pub fn loose_equals(&mut self, a: &Value, b: &Value) -> R<bool> {
        Ok(match (a, b) {
            (Value::Undefined | Value::Null, Value::Undefined | Value::Null) => true,
            (Value::Undefined | Value::Null, _) | (_, Value::Undefined | Value::Null) => false,
            (Value::Num(_), Value::Str(s)) => strict_equals(a, &Value::Num(str_to_num(s))),
            (Value::Str(s), Value::Num(_)) => strict_equals(&Value::Num(str_to_num(s)), b),
            (Value::BigInt(x), Value::Num(y)) | (Value::Num(y), Value::BigInt(x)) => (*x as f64) == *y,
            (Value::Bool(x), _) => {
                let n = Value::Num(*x as i32 as f64);
                self.loose_equals(&n, b)?
            }
            (_, Value::Bool(y)) => {
                let n = Value::Num(*y as i32 as f64);
                self.loose_equals(a, &n)?
            }
            (Value::Obj(_), Value::Num(_) | Value::Str(_) | Value::BigInt(_) | Value::Sym(_)) => {
                let p = self.to_primitive(a, "default")?;
                self.loose_equals(&p, b)?
            }
            (Value::Num(_) | Value::Str(_) | Value::BigInt(_) | Value::Sym(_), Value::Obj(_)) => {
                let p = self.to_primitive(b, "default")?;
                self.loose_equals(a, &p)?
            }
            _ => strict_equals(a, b),
        })
    }

    pub fn instance_of(&mut self, v: &Value, f: &Value) -> R<bool> {
        let Value::Obj(fo) = f else { throw!(self, "TypeError", "Right-hand side of 'instanceof' is not callable") };
        let hi = self.get(f, &PropKey::Sym(self.sym_has_instance.clone()))?;
        if hi.is_callable() && !matches!(&hi, Value::Obj(h) if h.borrow().class == "Function" && h.get_own("name").is_some_and(|n| matches!(n, Value::Str(ref s) if &**s == "[Symbol.hasInstance]"))) {
            return Ok(self.call(&hi, f.clone(), std::slice::from_ref(v))?.truthy());
        }
        if !fo.is_callable() {
            throw!(self, "TypeError", "Right-hand side of 'instanceof' is not callable");
        }
        if let ObjKind::Function(Func::Bound { target, .. }) = &fo.borrow().kind {
            let t = Value::Obj(target.clone());
            return self.instance_of(v, &t);
        }
        let Value::Obj(o) = v else { return Ok(false) };
        let proto = self.get_str(f, "prototype")?;
        let Value::Obj(proto) = proto else { return Ok(false) };
        let mut cur = o.proto();
        while let Some(c) = cur {
            if c.ptr_eq(&proto) {
                return Ok(true);
            }
            cur = c.proto();
        }
        Ok(false)
    }

    pub fn short_desc(&mut self, v: &Value) -> String {
        match v {
            Value::Str(s) => format!("\"{}\"", s.chars().take(40).collect::<String>()),
            Value::Obj(o) if o.is_callable() => {
                let n = o.get_own("name").map(|n| self.to_string_lossy(&n)).unwrap_or_default();
                format!("function {n}")
            }
            Value::Obj(_) => "object".into(),
            _ => self.to_string_lossy(v),
        }
    }

    /// Debug representation used by console.log and DevTools.
    pub fn inspect(&mut self, v: &Value, depth: usize) -> String {
        let mut seen = Vec::new();
        self.inspect_inner(v, depth, &mut seen, true)
    }

    fn inspect_inner(&mut self, v: &Value, depth: usize, seen: &mut Vec<usize>, top: bool) -> String {
        match v {
            Value::Str(s) => {
                if top {
                    s.to_string()
                } else {
                    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'").replace('\n', "\\n"))
                }
            }
            Value::Num(n) => {
                if *n == 0.0 && n.is_sign_negative() {
                    "-0".into()
                } else {
                    num_to_string(*n)
                }
            }
            Value::BigInt(n) => format!("{n}n"),
            Value::Obj(o) => {
                if seen.contains(&o.id()) {
                    return "[Circular]".into();
                }
                let kind_desc = {
                    let b = o.borrow();
                    match &b.kind {
                        ObjKind::Function(f) => {
                            let n = b.props.get(&PropKey::from("name")).map(|p| match &p.value {
                                Value::Str(s) => s.to_string(),
                                _ => String::new(),
                            });
                            let is_class = matches!(f, Func::Closure(c) if c.class.is_some());
                            let n = n.unwrap_or_default();
                            Some(if is_class {
                                format!("[class {}]", if n.is_empty() { "(anonymous)" } else { &n })
                            } else if n.is_empty() {
                                "[Function (anonymous)]".to_string()
                            } else {
                                format!("[Function: {n}]")
                            })
                        }
                        ObjKind::Error => None,
                        ObjKind::Date(t) => Some(super::builtins::date::to_iso(*t).unwrap_or_else(|| "Invalid Date".into())),
                        ObjKind::RegExp(r) => Some(format!("/{}/{}", r.source, r.flags)),
                        ObjKind::Promise(p) => Some(match p.status {
                            PromiseStatus::Pending => "Promise { <pending> }".to_string(),
                            _ => String::new(),
                        }),
                        ObjKind::Host(h) => Some(format!("[object {}]", h.class_name())),
                        ObjKind::Symbol(s) => Some(format!("[Symbol: {}]", s.0.description.as_deref().unwrap_or(""))),
                        ObjKind::Generator(_) => Some("Object [Generator] {}".into()),
                        _ => None,
                    }
                };
                if let Some(d) = kind_desc {
                    if !d.is_empty() {
                        if let Some(h) = o.host() {
                            if let Some(desc) = super::builtins::host_inspect(self, &h) {
                                return desc;
                            }
                        }
                        return d;
                    }
                }
                if matches!(o.borrow().kind, ObjKind::Error) {
                    let stack = o.get_own("stack");
                    let s = self.describe_error(v);
                    let _ = stack;
                    return s;
                }
                if depth == 0 {
                    return if o.is_array() { "[Array]".into() } else { "[Object]".into() };
                }
                seen.push(o.id());
                let out = if let ObjKind::Array(items) = &o.borrow().kind {
                    let items = items.clone();
                    let mut parts = Vec::new();
                    for (i, it) in items.iter().enumerate() {
                        if i >= 100 {
                            parts.push(format!("... {} more items", items.len() - 100));
                            break;
                        }
                        parts.push(self.inspect_inner(it, depth - 1, seen, false));
                    }
                    Some(format!("[ {} ]", parts.join(", ")).replace("[  ]", "[]"))
                } else {
                    None
                };
                let out = match out {
                    Some(s) => s,
                    None => {
                        let map_like = {
                            let b = o.borrow();
                            match &b.kind {
                                ObjKind::Map(m) => Some((true, m.entries.iter().flatten().cloned().collect::<Vec<_>>())),
                                ObjKind::Set(m) => Some((false, m.entries.iter().flatten().cloned().collect::<Vec<_>>())),
                                _ => None,
                            }
                        };
                        if let Some((is_map, entries)) = map_like {
                            let mut parts = Vec::new();
                            for (k, val) in entries.iter().take(50) {
                                if is_map {
                                    parts.push(format!("{} => {}", self.inspect_inner(k, depth - 1, seen, false), self.inspect_inner(val, depth - 1, seen, false)));
                                } else {
                                    parts.push(self.inspect_inner(k, depth - 1, seen, false));
                                }
                            }
                            format!("{}({}) {{ {} }}", if is_map { "Map" } else { "Set" }, entries.len(), parts.join(", "))
                        } else {
                            let keys = self.own_keys(o, false);
                            let mut parts = Vec::new();
                            for k in keys.iter().take(50) {
                                let val = self.get(v, k).unwrap_or(Value::Undefined);
                                let ks = match k {
                                    PropKey::Str(s) => {
                                        if s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$') && !s.is_empty() {
                                            s.to_string()
                                        } else {
                                            format!("'{s}'")
                                        }
                                    }
                                    PropKey::Sym(s) => format!("[Symbol({})]", s.0.description.as_deref().unwrap_or("")),
                                };
                                parts.push(format!("{ks}: {}", self.inspect_inner(&val, depth - 1, seen, false)));
                            }
                            let cls = {
                                let proto = o.proto();
                                let ctor = proto.as_ref().and_then(|p| p.get_own("constructor"));
                                ctor.and_then(|c| c.as_obj().and_then(|co| co.get_own("name"))).map(|n| self.to_string_lossy(&n)).unwrap_or_default()
                            };
                            let prefix = if cls.is_empty() || cls == "Object" { String::new() } else { format!("{cls} ") };
                            if parts.is_empty() {
                                format!("{prefix}{{}}")
                            } else {
                                format!("{prefix}{{ {} }}", parts.join(", "))
                            }
                        }
                    }
                };
                seen.pop();
                out
            }
            _ => self.to_string_lossy(v),
        }
    }
}

impl Drop for Interp {
    fn drop(&mut self) {
        // Unwind any suspended coroutines so their threads exit.
        let cos: Vec<Rc<Coroutine>> = self.coroutines.drain(..).filter_map(|w| w.upgrade()).collect();
        for c in cos {
            c.abort(self);
        }
        self.microtasks.clear();
    }
}

pub enum Iter {
    Array(Obj, usize),
    List(Vec<Value>, usize),
    Generic(Value, Value),
}

fn is_anon_fn(e: &Expr) -> bool {
    match e {
        Expr::Function(f) => f.name.is_none(),
        Expr::Class(c) => c.name.is_none(),
        _ => false,
    }
}

fn callee_desc(e: &Expr) -> String {
    match e {
        Expr::Ident(n) => n.to_string(),
        Expr::Member { obj, prop, .. } => {
            let o = callee_desc(obj);
            match &**prop {
                Key::Name(n) => format!("{o}.{n}"),
                Key::Private(n) => format!("{o}.#{n}"),
                Key::Computed(_) => format!("{o}[...]"),
            }
        }
        Expr::This => "this".into(),
        Expr::OptChain(i) => callee_desc(i),
        Expr::Call { callee, .. } => format!("{}(...)", callee_desc(callee)),
        _ => "expression".into(),
    }
}

pub fn strict_equals(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Num(x), Value::Num(y)) => x == y,
        (Value::BigInt(x), Value::BigInt(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Sym(x), Value::Sym(y)) => x == y,
        (Value::Obj(x), Value::Obj(y)) => x.ptr_eq(y),
        _ => false,
    }
}

pub fn same_value_zero(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Num(x), Value::Num(y)) if x.is_nan() && y.is_nan() => true,
        _ => strict_equals(a, b),
    }
}

pub fn js_mod(x: f64, y: f64) -> f64 {
    if y == 0.0 || x.is_infinite() || x.is_nan() || y.is_nan() {
        return f64::NAN;
    }
    if y.is_infinite() {
        return x;
    }
    x % y
}

pub fn js_pow(x: f64, y: f64) -> f64 {
    if y.is_nan() || ((x == 1.0 || x == -1.0) && y.is_infinite()) {
        return f64::NAN;
    }
    x.powf(y)
}

/// The UTF-16 code unit at index `i`, as a string (surrogate halves become U+FFFD).
pub fn string_index(s: &str, i: usize) -> Option<String> {
    if s.is_ascii() {
        return s.get(i..i + 1).map(|c| c.to_string());
    }
    let u = s.encode_utf16().nth(i)?;
    Some(String::from_utf16_lossy(&[u]))
}

/// Advance an async function's coroutine and settle its promise when it finishes.
pub fn drive_async(it: &mut Interp, co: Rc<Coroutine>, msg: Resume, resolve: Value, reject: Value) {
    match co.resume(it, msg) {
        Yield::Done(Ok(v)) => {
            let _ = it.call(&resolve, Value::Undefined, &[v]);
        }
        Yield::Done(Err(e)) => {
            let _ = it.call(&reject, Value::Undefined, &[e]);
        }
        Yield::Awaiting(v) | Yield::Yielded(v) => {
            let p = match super::builtins::promise::promise_resolve(it, v) {
                Ok(p) => p,
                Err(e) => {
                    let next = Resume::Throw(e);
                    return drive_async(it, co, next, resolve, reject);
                }
            };
            let (c1, r1, j1) = (co.clone(), resolve.clone(), reject.clone());
            let on_ful = it.native("", 1, move |it, _, args| {
                drive_async(it, c1.clone(), Resume::Next(arg(args, 0)), r1.clone(), j1.clone());
                Ok(Value::Undefined)
            });
            let (c2, r2, j2) = (co, resolve, reject);
            let on_rej = it.native("", 1, move |it, _, args| {
                drive_async(it, c2.clone(), Resume::Throw(arg(args, 0)), r2.clone(), j2.clone());
                Ok(Value::Undefined)
            });
            super::builtins::promise::then_internal(it, &p, Some(on_ful), Some(on_rej));
        }
    }
}
