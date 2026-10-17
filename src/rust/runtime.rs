//! Execution: frames, calls, operators and method dispatch.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use super::ast::{BinOp, Pos, SelfKind, Type};
use super::value::*;

pub enum Ctrl {
    Break(Option<Rc<str>>, Value),
    Continue(Option<Rc<str>>),
    Return(Value),
    Panic(String, Pos),
}

pub type R<T> = Result<T, Ctrl>;

pub struct Frame {
    pub slots: Vec<Value>,
}

pub type Code = Rc<dyn Fn(&mut Ctx, &mut Frame) -> R<Value>>;

pub struct FnCode {
    pub name: String,
    pub nslots: usize,
    /// Slots holding the closure's captured cells.
    pub capture_slots: Vec<usize>,
    pub nparams: usize,
    /// Binds arguments (by pattern) into slots.
    pub prologue: Rc<dyn Fn(&mut Ctx, &mut Frame, Vec<Value>) -> R<()>>,
    pub body: Code,
    pub self_kind: Option<SelfKind>,
    pub param_types: Vec<Type>,
    pub ret: Option<Type>,
    pub pos: Pos,
}

pub struct Program {
    pub file: String,
    pub types: RefCell<HashMap<String, Rc<TypeInfo>>>,
    pub fns: RefCell<HashMap<String, Rc<RefCell<Option<Rc<FnCode>>>>>>,
    /// type name -> method name -> code ("*" holds blanket impls)
    pub methods: RefCell<HashMap<String, HashMap<String, Rc<FnCode>>>>,
    pub consts: RefCell<HashMap<String, Value>>,
    pub exports: RefCell<Vec<String>>,
    pub option: Rc<TypeInfo>,
    pub result: Rc<TypeInfo>,
    pub ordering: Rc<TypeInfo>,
}

fn builtin_enum(name: &str, variants: &[(&str, usize)]) -> Rc<TypeInfo> {
    Rc::new(TypeInfo {
        name: name.into(),
        fields: vec![],
        is_tuple: false,
        variants: variants.iter().enumerate().map(|(i, (n, k))| (n.to_string(), (0..*k).map(|j| j.to_string()).collect(), *k > 0, i as i128)).collect(),
        is_enum: true,
        copy: false,
        derives: vec!["Debug".into(), "Clone".into(), "PartialEq".into()],
    })
}

impl Program {
    pub fn new(file: &str) -> Program {
        Program {
            file: file.into(),
            types: RefCell::new(HashMap::new()),
            fns: RefCell::new(HashMap::new()),
            methods: RefCell::new(HashMap::new()),
            consts: RefCell::new(HashMap::new()),
            exports: RefCell::new(vec![]),
            option: builtin_enum("Option", &[("None", 0), ("Some", 1)]),
            result: builtin_enum("Result", &[("Ok", 1), ("Err", 1)]),
            ordering: {
                let t = builtin_enum("Ordering", &[("Less", 0), ("Equal", 0), ("Greater", 0)]);
                let mut t = Rc::try_unwrap(t).ok().unwrap();
                t.variants[0].3 = -1;
                t.variants[1].3 = 0;
                t.variants[2].3 = 1;
                t.copy = true;
                Rc::new(t)
            },
        }
    }

    pub fn method(&self, ty: &str, name: &str) -> Option<Rc<FnCode>> {
        let m = self.methods.borrow();
        m.get(ty).and_then(|t| t.get(name)).or_else(|| m.get("*").and_then(|t| t.get(name))).cloned()
    }
}

impl Drop for Program {
    /// Compiled functions refer to each other (recursion goes through shared cells), so break
    /// those cycles when the page lets go of the program.
    fn drop(&mut self) {
        for cell in self.fns.get_mut().values() {
            cell.borrow_mut().take();
        }
        self.methods.get_mut().clear();
        self.consts.get_mut().clear();
    }
}

pub struct Ctx {
    pub prog: Rc<Program>,
    /// The page's JavaScript interpreter (for the DOM bridge); null when running headless.
    pub js: *mut crate::js::Interp,
    pub output: Vec<(&'static str, String)>,
    pub depth: usize,
    pub deadline: Option<Instant>,
    pub ticks: u32,
    /// Native stack pointer when this context was created (recursion budget).
    pub stack_base: usize,
}

/// Native stack the interpreter may use below the point it was entered (JS coroutine threads
/// have 64 MB stacks).
const STACK_BUDGET: usize = 24 * 1024 * 1024;

impl Ctx {
    pub fn new(prog: Rc<Program>, js: *mut crate::js::Interp) -> Ctx {
        Ctx { prog, js, output: vec![], depth: 0, deadline: Some(Instant::now() + std::time::Duration::from_secs(10)), ticks: 0, stack_base: crate::js::interp::stack_pointer() }
    }

    pub fn interp(&mut self) -> Option<&mut crate::js::Interp> {
        // SAFETY: the interpreter outlives every Rust call made on its behalf and isn't otherwise borrowed meanwhile.
        unsafe { self.js.as_mut() }
    }

    /// Called on loop back-edges: keeps runaway loops from hanging the browser.
    #[inline]
    pub fn tick(&mut self, pos: Pos) -> R<()> {
        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks & 0xfff == 0 {
            if let Some(d) = self.deadline {
                if Instant::now() > d {
                    return Err(Ctrl::Panic("script took too long to run (over 10s) and was stopped".into(), pos));
                }
            }
        }
        Ok(())
    }

    /// Queue console output. `print!` text ("print") accumulates until the next line.
    pub fn print(&mut self, level: &'static str, s: String) {
        match self.output.last_mut() {
            Some((last @ "print", buf)) => {
                buf.push_str(&s);
                if level != "print" {
                    *last = level;
                }
            }
            _ => self.output.push((level, s)),
        }
    }

    pub fn call_code(&mut self, f: &Rc<FnCode>, captures: &[Rc<RefCell<Value>>], args: Vec<Value>, pos: Pos) -> R<Value> {
        if self.depth > 4000 || self.stack_base.saturating_sub(crate::js::interp::stack_pointer()) > STACK_BUDGET {
            return Err(Ctrl::Panic(format!("stack overflow calling `{}`", f.name), pos));
        }
        self.depth += 1;
        let mut frame = Frame { slots: vec![Value::Unit; f.nslots.max(1)] };
        for (slot, c) in f.capture_slots.iter().zip(captures) {
            frame.slots[*slot] = Value::Boxed(c.clone());
        }
        let r = (f.prologue)(self, &mut frame, args).and_then(|_| (f.body)(self, &mut frame));
        self.depth -= 1;
        match r {
            Ok(v) => Ok(v),
            Err(Ctrl::Return(v)) => Ok(v),
            Err(Ctrl::Break(..)) | Err(Ctrl::Continue(_)) => Err(Ctrl::Panic("`break` outside of a loop".into(), pos)),
            Err(e) => Err(e),
        }
    }

    /// Call a function value (closure, fn item, native).
    pub fn call_value(&mut self, f: &Value, args: Vec<Value>, pos: Pos) -> R<Value> {
        match f.deref_all() {
            Value::Func(fv) => match &*fv {
                FuncVal::Code(code, caps) => {
                    let (code, caps) = (code.clone(), caps.clone());
                    self.call_code(&code, &caps, args, pos)
                }
                FuncVal::Native(_, nf) => {
                    let nf = nf.clone();
                    nf(self, args, pos)
                }
            },
            other => Err(Ctrl::Panic(format!("expected function, found `{}`", type_name(&other)), pos)),
        }
    }

    pub fn call_fn_name(&mut self, name: &str, args: Vec<Value>, pos: Pos) -> R<Value> {
        let cell = self.prog.fns.borrow().get(name).cloned();
        let code = cell.and_then(|c| c.borrow().clone());
        match code {
            Some(c) => self.call_code(&c, &[], args, pos),
            None => Err(Ctrl::Panic(format!("cannot find function `{name}`"), pos)),
        }
    }

    pub fn some(&self, v: Value) -> Value {
        Value::Enum(Rc::new(EnumVal { ty: self.prog.option.clone(), variant: 1, fields: RefCell::new(vec![v]) }))
    }
    pub fn none(&self) -> Value {
        Value::Enum(Rc::new(EnumVal { ty: self.prog.option.clone(), variant: 0, fields: RefCell::new(vec![]) }))
    }
    pub fn opt(&self, v: Option<Value>) -> Value {
        match v {
            Some(v) => self.some(v),
            None => self.none(),
        }
    }
    pub fn ok(&self, v: Value) -> Value {
        Value::Enum(Rc::new(EnumVal { ty: self.prog.result.clone(), variant: 0, fields: RefCell::new(vec![v]) }))
    }
    pub fn err(&self, v: Value) -> Value {
        Value::Enum(Rc::new(EnumVal { ty: self.prog.result.clone(), variant: 1, fields: RefCell::new(vec![v]) }))
    }
    pub fn ordering(&self, o: std::cmp::Ordering) -> Value {
        let i = match o {
            std::cmp::Ordering::Less => 0,
            std::cmp::Ordering::Equal => 1,
            std::cmp::Ordering::Greater => 2,
        };
        Value::Enum(Rc::new(EnumVal { ty: self.prog.ordering.clone(), variant: i, fields: RefCell::new(vec![]) }))
    }
}

pub fn panic<T>(msg: impl Into<String>, pos: Pos) -> R<T> {
    Err(Ctrl::Panic(msg.into(), pos))
}

/// `Option`/`Result` inspection helpers.
pub fn as_option(v: &Value) -> Option<Option<Value>> {
    match v.deref_all() {
        Value::Enum(e) if e.ty.name == "Option" => Some(if e.variant == 1 { e.fields.borrow().first().cloned() } else { None }),
        _ => None,
    }
}

pub fn as_result(v: &Value) -> Option<Result<Value, Value>> {
    match v.deref_all() {
        Value::Enum(e) if e.ty.name == "Result" => {
            let f = e.fields.borrow().first().cloned().unwrap_or(Value::Unit);
            Some(if e.variant == 0 { Ok(f) } else { Err(f) })
        }
        _ => None,
    }
}

pub fn type_name(v: &Value) -> String {
    match v {
        Value::Unit => "()".into(),
        Value::Bool(_) => "bool".into(),
        Value::Int(_, k) => k.name().into(),
        Value::Float(_, f32) => if *f32 { "f32" } else { "f64" }.into(),
        Value::Char(_) => "char".into(),
        Value::Str(_) => "String".into(),
        Value::Vec(_) => "Vec".into(),
        Value::Array(_) => "array".into(),
        Value::Tuple(_) => "tuple".into(),
        Value::Struct(s) => s.ty.name.clone(),
        Value::Enum(e) => e.ty.name.clone(),
        Value::Map(m) => {
            let m = m.borrow();
            match (m.sorted, m.is_set) {
                (false, false) => "HashMap",
                (true, false) => "BTreeMap",
                (false, true) => "HashSet",
                (true, true) => "BTreeSet",
            }
            .into()
        }
        Value::Func(_) => "closure".into(),
        Value::Ref(p) => type_name(&p.get()),
        Value::RcBox(_) => "Rc".into(),
        Value::Cell(_) => "RefCell".into(),
        Value::Boxed(b) => type_name(&b.borrow()),
        Value::Iter(_) => "Iterator".into(),
        Value::Range(_) => "Range".into(),
        Value::Js(_) => "JsValue".into(),
        Value::Formatter(..) => "Formatter".into(),
    }
}

// ---------------------------------------------------------------- arithmetic

fn int_kind(a: IntKind, b: IntKind) -> IntKind {
    if a == IntKind::Lit { b } else { a }
}

fn checked(v: i128, k: IntKind, what: &str, pos: Pos) -> R<Value> {
    let (lo, hi) = k.range();
    if v < lo || v > hi {
        return panic(format!("attempt to {what} with overflow"), pos);
    }
    Ok(Value::Int(v, k))
}

pub fn binary(ctx: &mut Ctx, op: BinOp, a: &Value, b: &Value, pos: Pos) -> R<Value> {
    let (a, b) = (a.deref_all(), b.deref_all());
    use BinOp::*;
    match op {
        Eq => return Ok(Value::Bool(user_eq(ctx, &a, &b, pos)?)),
        Ne => return Ok(Value::Bool(!user_eq(ctx, &a, &b, pos)?)),
        Lt | Le | Gt | Ge => {
            let o = user_cmp(ctx, &a, &b, pos)?;
            return Ok(Value::Bool(match op {
                Lt => o == std::cmp::Ordering::Less,
                Le => o != std::cmp::Ordering::Greater,
                Gt => o == std::cmp::Ordering::Greater,
                _ => o != std::cmp::Ordering::Less,
            }));
        }
        _ => {}
    }
    match (&a, &b) {
        (Value::Int(x, kx), Value::Int(y, ky)) => {
            let k = int_kind(*kx, *ky);
            let (x, y) = (*x, *y);
            match op {
                Add => checked(x + y, k, "add", pos),
                Sub => checked(x - y, k, "subtract", pos),
                Mul => match x.checked_mul(y) {
                    Some(v) => checked(v, k, "multiply", pos),
                    None => panic("attempt to multiply with overflow", pos),
                },
                Div => {
                    if y == 0 {
                        return panic("attempt to divide by zero", pos);
                    }
                    checked(x / y, k, "divide", pos)
                }
                Rem => {
                    if y == 0 {
                        return panic("attempt to calculate the remainder with a divisor of zero", pos);
                    }
                    Ok(Value::Int(x % y, k))
                }
                BitAnd => Ok(Value::Int(k.wrap(x & y), k)),
                BitOr => Ok(Value::Int(k.wrap(x | y), k)),
                BitXor => Ok(Value::Int(k.wrap(x ^ y), k)),
                Shl => {
                    let k = *kx;
                    if y < 0 || y as u32 >= k.bits() {
                        return panic("attempt to shift left with overflow", pos);
                    }
                    Ok(Value::Int(k.wrap(x << y), k))
                }
                Shr => {
                    let k = *kx;
                    if y < 0 || y as u32 >= k.bits() {
                        return panic("attempt to shift right with overflow", pos);
                    }
                    Ok(Value::Int(x >> y, k))
                }
                _ => unreachable!(),
            }
        }
        (Value::Float(x, f32a), Value::Float(y, _)) => float_op(op, *x, *y, *f32a, pos),
        // untyped integer literals mix with floats in our dynamic world
        (Value::Float(x, f), Value::Int(y, IntKind::Lit)) => float_op(op, *x, *y as f64, *f, pos),
        (Value::Int(x, IntKind::Lit), Value::Float(y, f)) => float_op(op, *x as f64, *y, *f, pos),
        (Value::Bool(x), Value::Bool(y)) => match op {
            BitAnd | And => Ok(Value::Bool(*x && *y)),
            BitOr | Or => Ok(Value::Bool(*x || *y)),
            BitXor => Ok(Value::Bool(*x ^ *y)),
            _ => panic(format!("cannot apply `{op:?}` to `bool`"), pos),
        },
        (Value::Str(x), _) if op == Add => {
            let rhs = match &b {
                Value::Str(s) => s.borrow().clone(),
                Value::Char(c) => c.to_string(),
                other => return panic(format!("cannot add `{}` to a `String`", type_name(other)), pos),
            };
            let mut s = x.borrow().clone();
            s.push_str(&rhs);
            Ok(Value::str(s))
        }
        (Value::Struct(_) | Value::Enum(_), _) => {
            let name = match op {
                Add => "add",
                Sub => "sub",
                Mul => "mul",
                Div => "div",
                Rem => "rem",
                BitAnd => "bitand",
                BitOr => "bitor",
                BitXor => "bitxor",
                Shl => "shl",
                Shr => "shr",
                _ => "",
            };
            let tn = type_name(&a);
            match ctx.prog.method(&tn, name) {
                Some(code) => ctx.call_code(&code, &[], vec![a.clone(), b.clone()], pos),
                None => panic(format!("cannot apply binary operator `{}` to type `{tn}`", op_symbol(op)), pos),
            }
        }
        _ => panic(format!("cannot apply binary operator `{}` to `{}` and `{}`", op_symbol(op), type_name(&a), type_name(&b)), pos),
    }
}

fn float_op(op: BinOp, x: f64, y: f64, f32: bool, pos: Pos) -> R<Value> {
    use BinOp::*;
    let r = match op {
        Add => x + y,
        Sub => x - y,
        Mul => x * y,
        Div => x / y,
        Rem => x % y,
        _ => return panic(format!("cannot apply binary operator `{}` to floats", op_symbol(op)), pos),
    };
    Ok(Value::Float(if f32 { r as f32 as f64 } else { r }, f32))
}

pub fn op_symbol(op: BinOp) -> &'static str {
    use BinOp::*;
    match op {
        Add => "+",
        Sub => "-",
        Mul => "*",
        Div => "/",
        Rem => "%",
        And => "&&",
        Or => "||",
        BitAnd => "&",
        BitOr => "|",
        BitXor => "^",
        Shl => "<<",
        Shr => ">>",
        Eq => "==",
        Ne => "!=",
        Lt => "<",
        Le => "<=",
        Gt => ">",
        Ge => ">=",
    }
}

/// `==`, honouring hand-written `PartialEq` impls.
pub fn user_eq(ctx: &mut Ctx, a: &Value, b: &Value, pos: Pos) -> R<bool> {
    if let Value::Struct(_) | Value::Enum(_) = a {
        let tn = type_name(a);
        if let Some(code) = ctx.prog.method(&tn, "eq") {
            return Ok(ctx.call_code(&code, &[], vec![a.clone(), b.clone()], pos)?.truthy());
        }
    }
    Ok(eq_values(a, b))
}

/// Ordering, honouring hand-written `Ord`/`PartialOrd` impls.
pub fn user_cmp(ctx: &mut Ctx, a: &Value, b: &Value, pos: Pos) -> R<std::cmp::Ordering> {
    if let Value::Struct(_) | Value::Enum(_) = a {
        let tn = type_name(a);
        for m in ["cmp", "partial_cmp"] {
            if let Some(code) = ctx.prog.method(&tn, m) {
                let r = ctx.call_code(&code, &[], vec![a.clone(), b.clone()], pos)?;
                let r = as_option(&r).flatten().unwrap_or(r);
                if let Value::Enum(e) = r.deref_all() {
                    if e.ty.name == "Ordering" {
                        return Ok(match e.variant {
                            0 => std::cmp::Ordering::Less,
                            1 => std::cmp::Ordering::Equal,
                            _ => std::cmp::Ordering::Greater,
                        });
                    }
                }
            }
        }
    }
    Ok(cmp_values(a, b))
}

pub fn negate(ctx: &mut Ctx, v: &Value, pos: Pos) -> R<Value> {
    match v.deref_all() {
        Value::Int(x, k) => {
            if !k.signed() {
                return panic(format!("cannot apply unary operator `-` to type `{}`", k.name()), pos);
            }
            checked(-x, k, "negate", pos)
        }
        Value::Float(x, f) => Ok(Value::Float(-x, f)),
        other @ (Value::Struct(_) | Value::Enum(_)) => {
            let tn = type_name(&other);
            match ctx.prog.method(&tn, "neg") {
                Some(code) => ctx.call_code(&code, &[], vec![other.clone()], pos),
                None => panic(format!("cannot apply unary operator `-` to type `{tn}`"), pos),
            }
        }
        other => panic(format!("cannot apply unary operator `-` to type `{}`", type_name(&other)), pos),
    }
}

pub fn not(v: &Value, pos: Pos) -> R<Value> {
    match v.deref_all() {
        Value::Bool(b) => Ok(Value::Bool(!b)),
        Value::Int(x, k) => Ok(Value::Int(k.wrap(!x), k)),
        other => panic(format!("cannot apply unary operator `!` to type `{}`", type_name(&other)), pos),
    }
}

/// `expr as Type`
pub fn cast(v: &Value, ty: &Type, pos: Pos) -> R<Value> {
    let v = v.deref_all();
    let name = ty.name();
    if let Some(k) = IntKind::from_name(name) {
        return Ok(Value::Int(
            match v {
                Value::Int(x, _) => k.wrap(x),
                Value::Float(f, _) => {
                    if f.is_nan() {
                        0
                    } else {
                        let (lo, hi) = k.range();
                        (f.trunc() as i128).clamp(lo, hi)
                    }
                }
                Value::Char(c) => k.wrap(c as i128),
                Value::Bool(b) => b as i128,
                Value::Enum(e) => e.ty.variants.get(e.variant).map(|v| v.3).unwrap_or(e.variant as i128),
                other => return panic(format!("non-primitive cast: `{}` as `{name}`", type_name(&other)), pos),
            },
            k,
        ));
    }
    match name {
        "f64" | "f32" => {
            let f32 = name == "f32";
            let x = match v {
                Value::Int(x, _) => x as f64,
                Value::Float(f, _) => f,
                other => return panic(format!("non-primitive cast: `{}` as `{name}`", type_name(&other)), pos),
            };
            Ok(Value::Float(if f32 { x as f32 as f64 } else { x }, f32))
        }
        "char" => match v {
            Value::Int(x, _) if (0..=255).contains(&x) => Ok(Value::Char(x as u8 as char)),
            Value::Char(c) => Ok(Value::Char(c)),
            other => panic(format!("only `u8` can be cast as `char`, not `{}`", type_name(&other)), pos),
        },
        "bool" => Ok(v),
        _ => Ok(v),
    }
}

// ---------------------------------------------------------------- method dispatch

/// Call `recv.name(args)`. `place` is where the receiver lives (for `&mut self` methods and
/// in-place operations on `Option`, integers, ...).
pub fn call_method(ctx: &mut Ctx, recv: Value, place: Option<Rc<Place>>, name: &str, generics: &[Type], args: Vec<Value>, pos: Pos) -> R<Value> {
    let mut v = recv;
    let mut place = place;
    for _ in 0..16 {
        let tn = type_name(&v);
        let user_ty = match &v {
            Value::Struct(_) | Value::Enum(_) => true,
            Value::Ref(_) | Value::RcBox(_) | Value::Boxed(_) => false,
            _ => true,
        };
        if user_ty {
            if let Some(code) = ctx.prog.method(&tn, name) {
                // `&mut self` on a value stored inline needs a reference to its place
                let needs_place = matches!(code.self_kind, Some(SelfKind::RefMut)) && matches!(v, Value::Enum(_) | Value::Int(..) | Value::Float(..) | Value::Bool(_) | Value::Char(_) | Value::Tuple(_) | Value::Array(_) | Value::Unit);
                let self_val = match (&place, needs_place) {
                    (Some(p), true) => Value::Ref(p.clone()),
                    _ => match code.self_kind {
                        Some(SelfKind::Value) => v.clone().moved(),
                        _ => v.clone(),
                    },
                };
                let mut all = Vec::with_capacity(args.len() + 1);
                all.push(self_val);
                all.extend(args);
                return ctx.call_code(&code, &[], all, pos);
            }
        }
        // builtin methods
        match super::stdlib::call_builtin(ctx, &v, place.clone(), name, generics, &args, pos)? {
            Some(r) => return Ok(r),
            None => {}
        }
        // auto-deref
        match &v {
            Value::Ref(p) => {
                let inner = p.get();
                place = Some(p.clone());
                v = inner;
            }
            Value::RcBox(b) => {
                v = (**b).clone();
                place = None;
            }
            Value::Boxed(b) => {
                let inner = b.borrow().clone();
                place = Some(Rc::new(Place::Cell(b.clone())));
                v = inner;
            }
            _ => break,
        }
    }
    panic(format!("no method named `{name}` found for `{}`", type_name(&v)), pos)
}
