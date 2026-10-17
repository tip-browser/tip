//! Compiles the syntax tree into closures over pre-resolved slots.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::ast::*;
use super::format::{self, ArgRef, Count, Piece};
use super::parser::CompileError;
use super::runtime::*;
use super::value::*;

type CResult<T> = Result<T, CompileError>;
/// Binds a value against a pattern; returns false on mismatch. The flag is true when the
/// value was reached through a `&mut` (bindings then become references into it).
pub type Binder = Rc<dyn Fn(&mut Ctx, &mut Frame, Value, bool) -> R<bool>>;
pub type PlaceCode = Rc<dyn Fn(&mut Ctx, &mut Frame) -> R<Rc<Place>>>;

#[derive(Clone, Copy, Debug)]
struct Var {
    slot: usize,
    boxed: bool,
}

#[derive(Default)]
struct FnCtx {
    scopes: Vec<Vec<(String, Var)>>,
    nslots: usize,
    boxed_names: HashSet<String>,
    /// (name, slot here, slot in the enclosing function)
    captures: Vec<(String, usize, usize)>,
    is_closure: bool,
    ret: Option<Type>,
}

impl FnCtx {
    fn alloc(&mut self) -> usize {
        self.nslots += 1;
        self.nslots - 1
    }
    fn find(&self, name: &str) -> Option<Var> {
        for s in self.scopes.iter().rev() {
            if let Some((_, v)) = s.iter().rev().find(|(n, _)| n == name) {
                return Some(*v);
            }
        }
        self.captures.iter().find(|c| c.0 == name).map(|c| Var { slot: c.1, boxed: true })
    }
}

pub struct Compiler {
    pub prog: Rc<Program>,
    fns: Vec<FnCtx>,
    self_type: Option<String>,
    /// Bare variant name -> enum name (for `use Enum::*` style references).
    variants: HashMap<String, String>,
    traits: HashMap<String, Vec<Rc<FnDecl>>>,
    trait_consts: HashMap<String, Vec<(String, Type, Expr)>>,
    const_decls: HashMap<String, (Type, Expr)>,
    consts: RefCell<HashMap<String, Rc<(Code, usize, RefCell<Option<Value>>)>>>,
    field_types: HashMap<String, Vec<(String, Type)>>,
    aliases: HashMap<String, Type>,
}

fn cerr<T>(msg: impl Into<String>, pos: Pos) -> CResult<T> {
    Err(CompileError { msg: msg.into(), pos })
}

macro_rules! code {
    (|$c:ident, $f:ident| $body:expr) => {
        Rc::new(move |$c: &mut Ctx, $f: &mut Frame| -> R<Value> { $body }) as Code
    };
}

// ---------------------------------------------------------------- analysis

/// Names used inside closures of a body (those variables must live in shared cells).
fn closure_names_block(b: &Block, inside: bool, out: &mut HashSet<String>) {
    for s in &b.stmts {
        match s {
            Stmt::Let(p, _, init, els, _) => {
                if inside {
                    pat_names(p, out);
                }
                if let Some(e) = init {
                    closure_names(e, inside, out);
                }
                if let Some(b) = els {
                    closure_names_block(b, inside, out);
                }
            }
            Stmt::Expr(e, _) => closure_names(e, inside, out),
            _ => {}
        }
    }
}

fn pat_names(p: &Pat, out: &mut HashSet<String>) {
    match p {
        Pat::Bind { name, sub, .. } => {
            out.insert(name.clone());
            if let Some(s) = sub {
                pat_names(s, out);
            }
        }
        Pat::Tuple(ps) | Pat::TupleStruct(_, ps) | Pat::Slice(ps) | Pat::Or(ps) => ps.iter().for_each(|p| pat_names(p, out)),
        Pat::Struct(_, fs, _) => fs.iter().for_each(|(_, p)| pat_names(p, out)),
        Pat::Ref(p) => pat_names(p, out),
        _ => {}
    }
}

fn format_names(fmt: &str, out: &mut HashSet<String>) {
    if let Ok(pieces) = format::parse(fmt) {
        for p in pieces {
            if let Piece::Arg(s) = p {
                if let ArgRef::Name(n) = &s.arg {
                    out.insert(n.clone());
                }
                for c in [&s.width, &s.precision].into_iter().flatten() {
                    if let Count::Arg(ArgRef::Name(n)) = c {
                        out.insert(n.clone());
                    }
                }
            }
        }
    }
}

fn closure_names(e: &Expr, inside: bool, out: &mut HashSet<String>) {
    let mut go = |e: &Expr| closure_names(e, inside, out);
    match &e.kind {
        ExprKind::Path(p, _) => {
            if inside && p.len() == 1 {
                out.insert(p[0].clone());
            }
        }
        ExprKind::Closure(c) => {
            for (p, _) in &c.params {
                pat_names(p, out);
            }
            closure_names(&c.body, true, out);
        }
        ExprKind::Lit(_) | ExprKind::Continue(_) => {}
        ExprKind::Unary(_, a) | ExprKind::Ref(_, a) | ExprKind::Field(a, _) | ExprKind::TupleField(a, _) | ExprKind::Try(a) | ExprKind::Cast(a, _) => go(a),
        ExprKind::Binary(_, a, b) | ExprKind::Assign(a, b) | ExprKind::AssignOp(_, a, b) | ExprKind::Index(a, b) | ExprKind::ArrayRepeat(a, b) => {
            go(a);
            go(b);
        }
        ExprKind::Call(f, args) => {
            go(f);
            args.iter().for_each(go);
        }
        ExprKind::MethodCall(r, _, _, args) => {
            go(r);
            args.iter().for_each(go);
        }
        ExprKind::Tuple(xs) | ExprKind::Array(xs) | ExprKind::Macro(_, xs) => xs.iter().for_each(go),
        ExprKind::FormatMacro(_, fmt, xs, named) => {
            xs.iter().for_each(&mut go);
            named.iter().for_each(|(_, e)| go(e));
            if inside {
                format_names(fmt, out);
            }
        }
        ExprKind::StructLit(_, fs, base) => {
            for (_, e) in fs {
                go(e);
            }
            if let Some(b) = base {
                go(b);
            }
            if inside {
                out.extend(fs.iter().map(|(n, _)| n.clone()));
            }
        }
        ExprKind::Range(a, b, _) => {
            if let Some(a) = a {
                go(a);
            }
            if let Some(b) = b {
                go(b);
            }
        }
        ExprKind::Block(b) | ExprKind::Loop(_, b) => closure_names_block(b, inside, out),
        ExprKind::If(c, t, e) => {
            go(c);
            closure_names_block(t, inside, out);
            if let Some(e) = e {
                closure_names(e, inside, out);
            }
        }
        ExprKind::IfLet(_, s, t, e) => {
            go(s);
            closure_names_block(t, inside, out);
            if let Some(e) = e {
                closure_names(e, inside, out);
            }
        }
        ExprKind::While(_, c, b) | ExprKind::WhileLet(_, _, c, b) | ExprKind::For(_, _, c, b) => {
            go(c);
            closure_names_block(b, inside, out);
        }
        ExprKind::Match(s, arms) => {
            go(s);
            for a in arms {
                if let Some(g) = &a.guard {
                    closure_names(g, inside, out);
                }
                closure_names(&a.body, inside, out);
            }
        }
        ExprKind::Return(v) | ExprKind::Break(_, v) => {
            if let Some(v) = v {
                go(v);
            }
        }
    }
}

fn is_place_expr(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Path(p, _) => p.len() == 1,
        ExprKind::Field(b, _) | ExprKind::TupleField(b, _) => is_place_expr(b) || matches!(b.kind, ExprKind::MethodCall(..) | ExprKind::Call(..) | ExprKind::Index(..) | ExprKind::Unary(UnOp::Deref, _)),
        ExprKind::Index(..) | ExprKind::Unary(UnOp::Deref, _) => true,
        _ => false,
    }
}

/// Coerce untyped literals (and simple containers of them) to a declared type.
fn coerce(v: Value, ty: &Type) -> Value {
    match ty {
        Type::Ref(_, t) => coerce(v, t),
        Type::Path(p, args) => {
            let name = p.last().map(|s| s.as_str()).unwrap_or("");
            if let Some(k) = IntKind::from_name(name) {
                if let Value::Int(x, kx) = &v {
                    if *kx == IntKind::Lit || *kx != k {
                        return Value::Int(*x, k);
                    }
                }
                return v;
            }
            if name == "f32" || name == "f64" {
                return match v {
                    Value::Float(f, _) => Value::Float(if name == "f32" { f as f32 as f64 } else { f }, name == "f32"),
                    Value::Int(x, IntKind::Lit) => Value::Float(x as f64, name == "f32"),
                    v => v,
                };
            }
            match (name, args.first(), &v) {
                ("Vec" | "VecDeque", Some(t), Value::Vec(items)) => {
                    let mut items = items.borrow_mut();
                    for it in items.iter_mut() {
                        *it = coerce(it.clone(), t);
                    }
                    drop(items);
                    v
                }
                ("Option", Some(t), Value::Enum(e)) if e.ty.name == "Option" => {
                    if let Some(f) = e.fields.borrow_mut().first_mut() {
                        *f = coerce(f.clone(), t);
                    }
                    v
                }
                _ => v,
            }
        }
        Type::Array(t, _) => {
            if let Value::Array(items) | Value::Vec(items) = &v {
                let mut items = items.borrow_mut();
                for it in items.iter_mut() {
                    *it = coerce(it.clone(), t);
                }
            }
            v
        }
        Type::Tuple(ts) => {
            if let Value::Tuple(items) = &v {
                let mut items = items.borrow_mut();
                for (it, t) in items.iter_mut().zip(ts) {
                    *it = coerce(it.clone(), t);
                }
            }
            v
        }
        _ => v,
    }
}

fn wants_type_hint(name: &str) -> bool {
    matches!(name, "collect" | "parse" | "into" | "sum" | "product" | "try_into" | "unwrap_or_default" | "default")
}

/// Give `collect()`/`parse()` the type a `let` annotation or return type asks for.
fn inject_hint(e: &Expr, ty: &Type) -> Expr {
    let mut e = e.clone();
    if let ExprKind::MethodCall(recv, name, generics, args) = &mut e.kind {
        if wants_type_hint(name) && generics.is_empty() {
            let hint = match ty {
                Type::Path(p, a) if (p.last().map(|s| s.as_str()) == Some("Result") || p.last().map(|s| s.as_str()) == Some("Option")) && name == "parse" => a.first().cloned().unwrap_or(ty.clone()),
                _ => ty.clone(),
            };
            generics.push(hint);
        } else if matches!(name.as_str(), "unwrap" | "expect" | "unwrap_or" | "unwrap_or_else" | "unwrap_or_default" | "clone" | "to_owned") {
            **recv = inject_hint(recv, ty);
        }
        let _ = args;
    } else if let ExprKind::Try(inner) = &mut e.kind {
        **inner = inject_hint(inner, ty);
    } else if let ExprKind::Call(callee, args) = &mut e.kind {
        if let (ExprKind::Path(p, g), true, Type::Path(tp, targs)) = (&mut callee.kind, args.is_empty(), ty) {
            let generic_owner = p.len() == 2 && (p[0] == "Default" || (p[0].len() <= 2 && p[0].chars().next().is_some_and(|c| c.is_uppercase())));
            if generic_owner && p[1] == "default" {
                *p = tp.iter().cloned().chain(std::iter::once("default".to_string())).collect();
                *g = targs.clone();
            } else if p.len() >= 2 && g.is_empty() && matches!(p[p.len() - 1].as_str(), "new" | "with_capacity" | "default") && tp.last() == Some(&p[p.len() - 2]) {
                // `let m: HashMap<K, Vec<V>> = HashMap::new();` — keep the element types
                *g = targs.clone();
            }
        }
    }
    e
}

impl Compiler {
    pub fn new(prog: Rc<Program>) -> Compiler {
        Compiler {
            prog,
            fns: vec![],
            self_type: None,
            variants: HashMap::new(),
            traits: HashMap::new(),
            trait_consts: HashMap::new(),
            const_decls: HashMap::new(),
            consts: RefCell::new(HashMap::new()),
            field_types: HashMap::new(),
            aliases: HashMap::new(),
        }
    }

    // ------------------------------------------------------------ program

    pub fn compile_program(&mut self, items: &[Item]) -> CResult<()> {
        let mut flat = Vec::new();
        flatten(items, &mut flat);
        // nested items inside function bodies are global too
        let mut nested = Vec::new();
        for it in &flat {
            collect_nested(it, &mut nested);
        }
        flat.extend(nested);
        // pass 1: declare types, functions, traits, consts
        for it in &flat {
            self.declare(it)?;
        }
        // pass 2: compile code
        for it in &flat {
            match it {
                Item::Fn(d) => {
                    self.self_type = None;
                    let code = self.compile_fn(d)?;
                    self.set_fn(&d.name, code.clone());
                    if d.export {
                        self.prog.exports.borrow_mut().push(d.name.clone());
                    }
                }
                Item::Impl { ty, trait_name, items, pos } => {
                    let ty = self.resolve_type_name(ty);
                    self.self_type = Some(ty.clone());
                    let mut defined = HashSet::new();
                    for item in items {
                        match item {
                            Item::Fn(d) => {
                                defined.insert(d.name.clone());
                                let code = self.compile_fn(d)?;
                                self.register_method(&ty, trait_name.as_deref(), d, code);
                            }
                            Item::Const { name, ty: cty, value: Some(value), .. } => {
                                self.const_decls.insert(format!("{ty}::{name}"), (cty.clone(), value.clone()));
                            }
                            _ => {}
                        }
                    }
                    // trait consts with default values the impl didn't override
                    if let Some(consts) = trait_name.as_ref().and_then(|t| self.trait_consts.get(t)).cloned() {
                        for (name, cty, value) in consts {
                            let key = format!("{ty}::{name}");
                            if !self.const_decls.contains_key(&key) {
                                self.const_decls.insert(key, (cty, value));
                            }
                        }
                    }
                    if let Some(t) = trait_name {
                        if let Some(defaults) = self.traits.get(t).cloned() {
                            for d in defaults {
                                if !defined.contains(&d.name) && d.body.is_some() {
                                    let code = self.compile_fn(&d)?;
                                    self.register_method(&ty, Some(t), &d, code);
                                }
                            }
                        } else if !matches!(t.as_str(), "Display" | "Debug" | "Clone" | "PartialEq" | "Eq" | "PartialOrd" | "Ord" | "Hash" | "Default" | "From" | "Into" | "Add" | "Sub" | "Mul" | "Div" | "Rem" | "Neg" | "Iterator" | "Drop" | "AddAssign" | "SubAssign" | "MulAssign" | "DivAssign" | "Index" | "IndexMut" | "FromStr" | "Deref" | "DerefMut" | "AsRef" | "Error" | "Copy" | "Send" | "Sync" | "TryFrom" | "Write" | "Not" | "BitAnd" | "BitOr" | "BitXor" | "Shl" | "Shr" | "Sum" | "Product" | "FromIterator" | "IntoIterator" | "ToString" | "Borrow" | "Ordering" | "Hasher" | "ExactSizeIterator" | "DoubleEndedIterator" | "PartialEqOrd") {
                            let _ = pos;
                        }
                    }
                    self.self_type = None;
                }
                _ => {}
            }
        }
        // `#[derive(Default)]` without a hand-written impl: synthesize `Type::default`
        for it in &flat {
            if let Item::Struct { name, derives, pos, .. } = it {
                let fname = format!("{name}::default");
                let has_user = self.fn_cell(&fname).is_some_and(|c| c.borrow().is_some());
                if derives.iter().any(|d| d == "Default") && !has_user {
                    let ftypes = self.field_types.get(name).cloned().unwrap_or_default();
                    let fcodes = ftypes.iter().map(|(_, t)| self.default_code(t, *pos, 1)).collect::<CResult<Vec<_>>>()?;
                    let tname = name.clone();
                    let pos = *pos;
                    let body = code!(|c, f| {
                        let Some(ti) = c.prog.types.borrow().get(&tname).cloned() else { return panic("unknown type", pos) };
                        let mut vals = Vec::with_capacity(fcodes.len());
                        for x in fcodes.iter() {
                            vals.push(x(c, f)?);
                        }
                        Ok(Value::Struct(Rc::new(StructVal { ty: ti, fields: RefCell::new(vals) })))
                    });
                    let code = Rc::new(FnCode {
                        name: fname.clone(),
                        nslots: 1,
                        capture_slots: vec![],
                        nparams: 0,
                        prologue: Rc::new(|_, _, _| Ok(())),
                        body,
                        self_kind: None,
                        param_types: vec![],
                        ret: None,
                        pos,
                    });
                    self.set_fn(&fname, code);
                }
            }
        }
        // consts are compiled on first use; check they compile now for early errors
        let names: Vec<String> = self.const_decls.keys().cloned().collect();
        for n in names {
            self.const_code(&n)?;
        }
        Ok(())
    }

    fn resolve_type_name(&self, ty: &str) -> String {
        match self.aliases.get(ty) {
            Some(t) => t.name().to_string(),
            None => ty.to_string(),
        }
    }

    fn register_method(&mut self, ty: &str, trait_name: Option<&str>, d: &FnDecl, code: Rc<FnCode>) {
        let key = if self.prog.types.borrow().contains_key(ty) || IntKind::from_name(ty).is_some() || matches!(ty, "f64" | "f32" | "bool" | "char" | "String" | "str" | "Vec" | "HashMap" | "Option" | "Result") {
            ty.to_string()
        } else if ty.len() <= 2 && ty.chars().next().is_some_and(|c| c.is_uppercase()) {
            // `impl<T: Bound> Trait for T`
            "*".to_string()
        } else {
            ty.to_string()
        };
        let key = if key == "str" { "String".to_string() } else { key };
        let fname = format!("{ty}::{}", d.name);
        self.set_fn(&fname, code.clone());
        if let Some(t) = trait_name {
            self.set_fn(&format!("{t}::{}", d.name), code.clone());
        }
        if d.self_param.is_some() {
            let mut m = self.prog.methods.borrow_mut();
            let table = m.entry(key).or_default();
            if let Some(t) = trait_name {
                table.insert(format!("{}#{t}", d.name), code.clone());
            }
            // `fmt` from Display wins the plain name (used by `to_string`)
            if !(d.name == "fmt" && trait_name == Some("Debug") && table.contains_key("fmt")) {
                table.insert(d.name.clone(), code);
            }
        }
    }

    fn set_fn(&self, name: &str, code: Rc<FnCode>) {
        let cell = self.prog.fns.borrow_mut().entry(name.to_string()).or_insert_with(|| Rc::new(RefCell::new(None))).clone();
        *cell.borrow_mut() = Some(code);
    }

    fn fn_cell(&self, name: &str) -> Option<Rc<RefCell<Option<Rc<FnCode>>>>> {
        self.prog.fns.borrow().get(name).cloned()
    }

    fn declare(&mut self, it: &Item) -> CResult<()> {
        match it {
            Item::Struct { name, kind, derives, .. } => {
                let (fields, is_tuple, types) = match kind {
                    StructKind::Named(f) => (f.iter().map(|(n, _)| n.clone()).collect(), false, f.clone()),
                    StructKind::Tuple(t) => ((0..t.len()).map(|i| i.to_string()).collect(), true, t.iter().enumerate().map(|(i, t)| (i.to_string(), t.clone())).collect()),
                    StructKind::Unit => (vec![], false, vec![]),
                };
                self.field_types.insert(name.clone(), types);
                self.prog.types.borrow_mut().insert(
                    name.clone(),
                    Rc::new(TypeInfo { name: name.clone(), fields, is_tuple, variants: vec![], is_enum: false, copy: derives.iter().any(|d| d == "Copy"), derives: derives.clone() }),
                );
            }
            Item::Enum { name, variants, derives, .. } => {
                let mut disc = 0i128;
                let mut vs = Vec::new();
                for v in variants {
                    if let Some(d) = v.discriminant {
                        disc = d;
                    }
                    let (fields, is_tuple) = match &v.kind {
                        StructKind::Named(f) => (f.iter().map(|(n, _)| n.clone()).collect(), false),
                        StructKind::Tuple(t) => ((0..t.len()).map(|i| i.to_string()).collect(), true),
                        StructKind::Unit => (vec![], false),
                    };
                    vs.push((v.name.clone(), fields, is_tuple, disc));
                    disc += 1;
                    if self.variants.contains_key(&v.name) {
                        self.variants.insert(v.name.clone(), String::new()); // ambiguous
                    } else {
                        self.variants.insert(v.name.clone(), name.clone());
                    }
                }
                self.prog.types.borrow_mut().insert(
                    name.clone(),
                    Rc::new(TypeInfo { name: name.clone(), fields: vec![], is_tuple: false, variants: vs, is_enum: true, copy: derives.iter().any(|d| d == "Copy"), derives: derives.clone() }),
                );
            }
            Item::Fn(d) => {
                self.prog.fns.borrow_mut().entry(d.name.clone()).or_insert_with(|| Rc::new(RefCell::new(None)));
            }
            Item::Impl { ty, trait_name, items, .. } => {
                for item in items {
                    if let Item::Fn(d) = item {
                        self.prog.fns.borrow_mut().entry(format!("{ty}::{}", d.name)).or_insert_with(|| Rc::new(RefCell::new(None)));
                        if let Some(t) = trait_name {
                            self.prog.fns.borrow_mut().entry(format!("{t}::{}", d.name)).or_insert_with(|| Rc::new(RefCell::new(None)));
                        }
                    }
                    if let Item::Const { name, ty: cty, value: Some(value), .. } = item {
                        self.const_decls.insert(format!("{ty}::{name}"), (cty.clone(), value.clone()));
                    }
                }
            }
            Item::Trait { name, items, .. } => {
                let mut defaults = Vec::new();
                let mut consts = Vec::new();
                for item in items {
                    if let Item::Fn(d) = item {
                        defaults.push(d.clone());
                    }
                    if let Item::Const { name: cn, ty: cty, value: Some(v), .. } = item {
                        consts.push((cn.clone(), cty.clone(), v.clone()));
                    }
                }
                self.traits.insert(name.clone(), defaults);
                self.trait_consts.insert(name.clone(), consts);
            }
            Item::Const { name, ty, value: Some(value), .. } => {
                self.const_decls.insert(name.clone(), (ty.clone(), value.clone()));
            }
            Item::TypeAlias { name, ty } => {
                self.aliases.insert(name.clone(), ty.clone());
            }
            _ => {}
        }
        Ok(())
    }

    fn const_code(&mut self, name: &str) -> CResult<Rc<(Code, usize, RefCell<Option<Value>>)>> {
        if let Some(c) = self.consts.borrow().get(name) {
            return Ok(c.clone());
        }
        let (ty, e) = self.const_decls.get(name).cloned().ok_or(CompileError { msg: format!("cannot find value `{name}`"), pos: Pos { line: 0, col: 0 } })?;
        let saved = std::mem::take(&mut self.fns);
        self.fns.push(FnCtx { scopes: vec![vec![]], ..Default::default() });
        let code = self.expr(&e);
        let ctx = self.fns.pop().unwrap();
        self.fns = saved;
        let code = code?;
        let ty2 = ty.clone();
        let code: Code = Rc::new(move |c, f| Ok(coerce(code(c, f)?, &ty2)));
        let entry = Rc::new((code, ctx.nslots, RefCell::new(None)));
        self.consts.borrow_mut().insert(name.to_string(), entry.clone());
        Ok(entry)
    }

    // ------------------------------------------------------------ functions

    fn compile_fn(&mut self, d: &FnDecl) -> CResult<Rc<FnCode>> {
        let Some(body) = &d.body else { return cerr(format!("function `{}` has no body", d.name), d.pos) };
        let mut boxed = HashSet::new();
        closure_names_block(body, false, &mut boxed);
        let saved = std::mem::take(&mut self.fns);
        self.fns.push(FnCtx { scopes: vec![vec![]], boxed_names: boxed, ret: d.ret.clone(), ..Default::default() });
        let r = self.compile_fn_inner(d, body);
        self.fns = saved;
        r
    }

    fn compile_fn_inner(&mut self, d: &FnDecl, body: &Rc<Block>) -> CResult<Rc<FnCode>> {
        let mut binders: Vec<(Binder, Option<Type>)> = Vec::new();
        if d.self_param.is_some() {
            let v = self.declare_var("self");
            binders.push((self.bind_var(v), None));
        }
        for (p, t) in &d.params {
            let b = self.pattern(p)?;
            binders.push((b, Some(t.clone())));
        }
        let ret = d.ret.clone().map(|t| self.expand_alias(&t));
        let mut body_code = self.block_with_hint(body, ret.as_ref())?;
        if let Some(rt) = ret.clone() {
            let inner = body_code;
            body_code = Rc::new(move |c, f| match inner(c, f) {
                Ok(v) => Ok(coerce(v, &rt)),
                Err(Ctrl::Return(v)) => Ok(coerce(v, &rt)),
                Err(e) => Err(e),
            });
        }
        let nparams = binders.len();
        let name = d.name.clone();
        let pos = d.pos;
        let prologue: Rc<dyn Fn(&mut Ctx, &mut Frame, Vec<Value>) -> R<()>> = Rc::new(move |c, f, args| {
            if args.len() != nparams {
                return panic(format!("function `{name}` takes {nparams} arguments but {} were supplied", args.len()), pos);
            }
            for ((b, ty), a) in binders.iter().zip(args) {
                let by_ref = matches!(a, Value::Ref(_));
                let a = match ty {
                    Some(t) => coerce(a, t),
                    None => a,
                };
                if !b(c, f, a, by_ref)? {
                    return panic("refutable pattern in function argument", pos);
                }
            }
            Ok(())
        });
        let ctx = self.fns.last().unwrap();
        Ok(Rc::new(FnCode {
            name: d.name.clone(),
            nslots: ctx.nslots,
            capture_slots: vec![],
            nparams,
            prologue,
            body: body_code,
            self_kind: d.self_param,
            param_types: d.params.iter().map(|(_, t)| t.clone()).collect(),
            ret: d.ret.clone(),
            pos: d.pos,
        }))
    }

    fn expand_alias(&self, t: &Type) -> Type {
        match t {
            Type::Path(p, _) if p.len() == 1 => self.aliases.get(&p[0]).cloned().unwrap_or_else(|| t.clone()),
            _ => t.clone(),
        }
    }

    // ------------------------------------------------------------ variables

    fn cur(&mut self) -> &mut FnCtx {
        self.fns.last_mut().unwrap()
    }

    fn declare_var(&mut self, name: &str) -> Var {
        let c = self.cur();
        let slot = c.alloc();
        let v = Var { slot, boxed: c.boxed_names.contains(name) };
        c.scopes.last_mut().unwrap().push((name.to_string(), v));
        v
    }

    fn lookup(&mut self, name: &str) -> Option<Var> {
        let n = self.fns.len();
        for level in (0..n).rev() {
            if let Some(v) = self.fns[level].find(name) {
                if level == n - 1 {
                    return Some(v);
                }
                let mut src = v.slot;
                for l in level + 1..n {
                    if let Some(c) = self.fns[l].captures.iter().find(|c| c.0 == name) {
                        src = c.1;
                        continue;
                    }
                    let slot = self.fns[l].alloc();
                    self.fns[l].captures.push((name.to_string(), slot, src));
                    src = slot;
                }
                return Some(Var { slot: src, boxed: true });
            }
            if !self.fns[level].is_closure {
                break;
            }
        }
        None
    }

    fn bind_var(&self, v: Var) -> Binder {
        let slot = v.slot;
        if v.boxed {
            Rc::new(move |_, f, val, _| {
                f.slots[slot] = Value::Boxed(Rc::new(RefCell::new(val)));
                Ok(true)
            })
        } else {
            Rc::new(move |_, f, val, _| {
                f.slots[slot] = val;
                Ok(true)
            })
        }
    }

    fn read_var(v: Var) -> Code {
        let slot = v.slot;
        if v.boxed {
            code!(|_c, f| Ok(match &f.slots[slot] {
                Value::Boxed(b) => b.borrow().clone(),
                other => other.clone(),
            }))
        } else {
            code!(|_c, f| Ok(f.slots[slot].clone()))
        }
    }

    fn push_scope(&mut self) {
        self.cur().scopes.push(vec![]);
    }
    fn pop_scope(&mut self) {
        self.cur().scopes.pop();
    }

    // ------------------------------------------------------------ blocks

    fn block(&mut self, b: &Block) -> CResult<Code> {
        self.block_with_hint(b, None)
    }

    fn block_with_hint(&mut self, b: &Block, hint: Option<&Type>) -> CResult<Code> {
        self.push_scope();
        let r = self.block_inner(b, hint);
        self.pop_scope();
        r
    }

    fn block_inner(&mut self, b: &Block, hint: Option<&Type>) -> CResult<Code> {
        let mut stmts: Vec<Code> = Vec::new();
        let mut tail: Option<Code> = None;
        let n = b.stmts.len();
        for (i, s) in b.stmts.iter().enumerate() {
            let last = i + 1 == n;
            match s {
                Stmt::Empty | Stmt::Item(_) => {}
                Stmt::Let(pat, ty, init, els, pos) => stmts.push(self.let_stmt(pat, ty.as_ref(), init.as_ref(), els.as_ref(), *pos)?),
                Stmt::Expr(e, semi) => {
                    if last && !semi {
                        let e2 = match hint {
                            Some(t) => inject_hint(e, t),
                            None => e.clone(),
                        };
                        tail = Some(self.expr(&e2)?);
                    } else {
                        let c = self.expr(e)?;
                        stmts.push(c);
                    }
                }
            }
        }
        Ok(match (stmts.len(), tail) {
            (0, Some(t)) => t,
            (0, None) => code!(|_c, _f| Ok(Value::Unit)),
            (_, tail) => {
                let stmts: Rc<[Code]> = stmts.into();
                match tail {
                    Some(t) => code!(|c, f| {
                        for s in stmts.iter() {
                            s(c, f)?;
                        }
                        t(c, f)
                    }),
                    None => code!(|c, f| {
                        for s in stmts.iter() {
                            s(c, f)?;
                        }
                        Ok(Value::Unit)
                    }),
                }
            }
        })
    }

    fn let_stmt(&mut self, pat: &Pat, ty: Option<&Type>, init: Option<&Expr>, els: Option<&Rc<Block>>, pos: Pos) -> CResult<Code> {
        let ty = ty.map(|t| self.expand_alias(t));
        let init_code = match init {
            Some(e) => {
                let e = match &ty {
                    Some(t) => inject_hint(e, t),
                    None => e.clone(),
                };
                Some(self.expr(&e)?)
            }
            None => None,
        };
        let els_code = match els {
            Some(b) => Some(self.block(b)?),
            None => None,
        };
        // the pattern's bindings are in scope after the statement
        let binder = self.pattern(pat)?;
        let Some(init) = init_code else {
            return Ok(code!(|_c, _f| Ok(Value::Unit)));
        };
        Ok(match (ty, els_code) {
            (None, None) => code!(|c, f| {
                let v = init(c, f)?.moved();
                let by_ref = matches!(v, Value::Ref(_));
                if !binder(c, f, v, by_ref)? {
                    return panic("refutable pattern in `let` binding did not match", pos);
                }
                Ok(Value::Unit)
            }),
            (ty, els) => code!(|c, f| {
                let mut v = init(c, f)?.moved();
                if let Some(t) = &ty {
                    v = coerce(v, t);
                }
                let by_ref = matches!(v, Value::Ref(_));
                if !binder(c, f, v, by_ref)? {
                    match &els {
                        Some(e) => {
                            e(c, f)?;
                            return panic("`let...else` block must diverge", pos);
                        }
                        None => return panic("refutable pattern in `let` binding did not match", pos),
                    }
                }
                Ok(Value::Unit)
            }),
        })
    }

    // ------------------------------------------------------------ patterns

    fn pattern(&mut self, p: &Pat) -> CResult<Binder> {
        let mut declared = HashMap::new();
        self.pattern_inner(p, &mut declared)
    }

    fn pattern_inner(&mut self, p: &Pat, declared: &mut HashMap<String, Var>) -> CResult<Binder> {
        let pos = Pos { line: 0, col: 0 };
        Ok(match p {
            Pat::Wild | Pat::Rest => Rc::new(|_, _, _, _| Ok(true)),
            Pat::Bind { name, sub, .. } => {
                let v = match declared.get(name) {
                    Some(v) => *v,
                    None => {
                        let v = self.declare_var(name);
                        declared.insert(name.clone(), v);
                        v
                    }
                };
                let store = self.bind_var(v);
                match sub {
                    None => store,
                    Some(s) => {
                        let sb = self.pattern_inner(s, declared)?;
                        Rc::new(move |c, f, val, r| {
                            if !sb(c, f, val.clone(), r)? {
                                return Ok(false);
                            }
                            store(c, f, val, r)
                        })
                    }
                }
            }
            Pat::Lit(l, neg) => {
                let lit = lit_value(l, *neg);
                Rc::new(move |c, _, val, _| user_eq(c, &val.deref_all(), &lit, pos))
            }
            Pat::Range(lo, hi, incl) => {
                let lo_v = self.pat_const(lo)?;
                let hi_v = self.pat_const(hi)?;
                let incl = *incl;
                Rc::new(move |_, _, val, _| {
                    let v = val.deref_all();
                    let ge = cmp_values(&v, &lo_v) != std::cmp::Ordering::Less;
                    let o = cmp_values(&v, &hi_v);
                    Ok(ge && (o == std::cmp::Ordering::Less || (incl && o == std::cmp::Ordering::Equal)))
                })
            }
            Pat::Ref(inner) => self.pattern_inner(inner, declared)?,
            Pat::Or(alts) => {
                let bs: Vec<Binder> = alts.iter().map(|a| self.pattern_inner(a, declared)).collect::<CResult<_>>()?;
                Rc::new(move |c, f, val, r| {
                    for b in &bs {
                        if b(c, f, val.clone(), r)? {
                            return Ok(true);
                        }
                    }
                    Ok(false)
                })
            }
            Pat::Tuple(items) => {
                let (before, after, rest) = self.split_rest(items, declared)?;
                Rc::new(move |c, f, val, by_ref| {
                    let (v, r) = deref_for_match(val, by_ref);
                    let Value::Tuple(t) = &v else { return Ok(false) };
                    let items = t.borrow().clone();
                    match_seq(c, f, &items, &before, &after, rest.as_ref(), r, |i| Place::Elem(t.clone(), i))
                })
            }
            Pat::Slice(items) => {
                let (before, after, rest) = self.split_rest(items, declared)?;
                Rc::new(move |c, f, val, by_ref| {
                    let (v, r) = deref_for_match(val, by_ref);
                    let (Value::Vec(t) | Value::Array(t)) = &v else { return Ok(false) };
                    let items = t.borrow().clone();
                    if rest.is_none() && items.len() != before.len() {
                        return Ok(false);
                    }
                    match_seq(c, f, &items, &before, &after, rest.as_ref(), r, |i| Place::Elem(t.clone(), i))
                })
            }
            Pat::TupleStruct(path, items) => {
                let target = self.resolve_ctor(path, pos)?;
                let (before, after, rest) = self.split_rest(items, declared)?;
                match target {
                    Ctor::Variant(ty, idx) => Rc::new(move |c, f, val, by_ref| {
                        let (v, r) = deref_for_match(val, by_ref);
                        let Value::Enum(e) = &v else { return Ok(false) };
                        if e.variant != idx || e.ty.name != ty.name {
                            return Ok(false);
                        }
                        let fields = e.fields.borrow().clone();
                        match_seq(c, f, &fields, &before, &after, rest.as_ref(), r, |i| Place::EnumField(e.clone(), i))
                    }),
                    Ctor::Struct(ty) => Rc::new(move |c, f, val, by_ref| {
                        let (v, r) = deref_for_match(val, by_ref);
                        let Value::Struct(s) = &v else { return Ok(false) };
                        if !Rc::ptr_eq(&s.ty, &ty) {
                            return Ok(false);
                        }
                        let fields = s.fields.borrow().clone();
                        match_seq(c, f, &fields, &before, &after, rest.as_ref(), r, |i| Place::Field(s.clone(), i))
                    }),
                }
            }
            Pat::Struct(path, fields, _rest) => {
                let target = self.resolve_ctor(path, pos)?;
                let mut fbs: Vec<(String, Binder)> = Vec::new();
                for (n, fp) in fields {
                    fbs.push((n.clone(), self.pattern_inner(fp, declared)?));
                }
                match target {
                    Ctor::Variant(ty, idx) => {
                        let names = ty.variants[idx].1.clone();
                        let map: Vec<(usize, Binder)> = fbs.into_iter().map(|(n, b)| (names.iter().position(|x| *x == n).unwrap_or(usize::MAX), b)).collect();
                        Rc::new(move |c, f, val, by_ref| {
                            let (v, r) = deref_for_match(val, by_ref);
                            let Value::Enum(e) = &v else { return Ok(false) };
                            if e.variant != idx || e.ty.name != ty.name {
                                return Ok(false);
                            }
                            for (i, b) in &map {
                                let fv = if r { Value::Ref(Rc::new(Place::EnumField(e.clone(), *i))) } else { e.fields.borrow().get(*i).cloned().unwrap_or(Value::Unit) };
                                if !b(c, f, fv, r)? {
                                    return Ok(false);
                                }
                            }
                            Ok(true)
                        })
                    }
                    Ctor::Struct(ty) => {
                        let map: Vec<(usize, Binder)> = fbs.into_iter().map(|(n, b)| (ty.fields.iter().position(|x| *x == n).unwrap_or(usize::MAX), b)).collect();
                        Rc::new(move |c, f, val, by_ref| {
                            let (v, r) = deref_for_match(val, by_ref);
                            let Value::Struct(s) = &v else { return Ok(false) };
                            if !Rc::ptr_eq(&s.ty, &ty) {
                                return Ok(false);
                            }
                            for (i, b) in &map {
                                let fv = if r { Value::Ref(Rc::new(Place::Field(s.clone(), *i))) } else { s.fields.borrow().get(*i).cloned().unwrap_or(Value::Unit) };
                                if !b(c, f, fv, r)? {
                                    return Ok(false);
                                }
                            }
                            Ok(true)
                        })
                    }
                }
            }
            Pat::Path(path) => {
                // unit variant, unit struct or constant
                if let Ok(target) = self.resolve_ctor(path, pos) {
                    match target {
                        Ctor::Variant(ty, idx) => Rc::new(move |_, _, val, _| Ok(matches!(val.deref_all(), Value::Enum(e) if e.variant == idx && e.ty.name == ty.name))),
                        Ctor::Struct(ty) => Rc::new(move |_, _, val, _| Ok(matches!(val.deref_all(), Value::Struct(s) if Rc::ptr_eq(&s.ty, &ty)))),
                    }
                } else {
                    let v = self.pat_const(&Pat::Path(path.clone()))?;
                    Rc::new(move |c, _, val, _| user_eq(c, &val.deref_all(), &v, pos))
                }
            }
        })
    }

    fn split_rest(&mut self, items: &[Pat], declared: &mut HashMap<String, Var>) -> CResult<(Vec<Binder>, Vec<Binder>, Option<Binder>)> {
        let mut before = Vec::new();
        let mut after = Vec::new();
        let mut rest: Option<Binder> = None;
        let mut seen_rest = false;
        for p in items {
            let is_rest = matches!(p, Pat::Rest) || matches!(p, Pat::Bind { sub: Some(s), .. } if matches!(**s, Pat::Rest));
            if is_rest {
                seen_rest = true;
                rest = Some(match p {
                    Pat::Bind { name, .. } => {
                        let v = self.declare_var(name);
                        declared.insert(name.clone(), v);
                        self.bind_var(v)
                    }
                    _ => Rc::new(|_, _, _, _| Ok(true)),
                });
                continue;
            }
            let b = self.pattern_inner(p, declared)?;
            if seen_rest {
                after.push(b);
            } else {
                before.push(b);
            }
        }
        Ok((before, after, rest))
    }

    /// A constant value used in a pattern (literal, const item or unit variant).
    fn pat_const(&mut self, p: &Pat) -> CResult<Value> {
        let pos = Pos { line: 0, col: 0 };
        match p {
            Pat::Lit(l, neg) => Ok(lit_value(l, *neg)),
            Pat::Path(path) => {
                let name = path.join("::");
                if self.const_decls.contains_key(&name) {
                    let entry = self.const_code(&name)?;
                    let prog = self.prog.clone();
                    let mut ctx = Ctx::new(prog, std::ptr::null_mut());
                    let mut fr = Frame { slots: vec![Value::Unit; entry.1.max(1)] };
                    return (entry.0)(&mut ctx, &mut fr).map_err(|_| CompileError { msg: format!("could not evaluate constant `{name}`"), pos });
                }
                if let Some(v) = builtin_const(path) {
                    return Ok(v);
                }
                cerr(format!("cannot find unit struct, unit variant or constant `{name}`"), pos)
            }
            _ => cerr("unsupported pattern in range", pos),
        }
    }

    fn resolve_ctor(&self, path: &[String], pos: Pos) -> CResult<Ctor> {
        let path: Vec<String> = path.iter().map(|s| if s == "Self" { self.self_type.clone().unwrap_or_default() } else { s.clone() }).collect();
        let types = self.prog.types.borrow();
        let last = path.last().map(|s| s.as_str()).unwrap_or("");
        if path.len() >= 2 {
            let ty_name = &path[path.len() - 2];
            let ty_name = self.aliases.get(ty_name).map(|t| t.name().to_string()).unwrap_or(ty_name.clone());
            if let Some(t) = types.get(&ty_name) {
                if let Some(i) = t.variant_index(last) {
                    return Ok(Ctor::Variant(t.clone(), i));
                }
            }
            if ty_name == "Option" || ty_name == "Result" || ty_name == "Ordering" {
                return self.builtin_variant(last, pos);
            }
        }
        if path.len() == 1 {
            if let Some(t) = types.get(last) {
                if !t.is_enum {
                    return Ok(Ctor::Struct(t.clone()));
                }
            }
            if let Ok(c) = self.builtin_variant(last, pos) {
                return Ok(c);
            }
            if let Some(e) = self.variants.get(last).filter(|e| !e.is_empty()) {
                if let Some(t) = types.get(e) {
                    return Ok(Ctor::Variant(t.clone(), t.variant_index(last).unwrap()));
                }
            }
        }
        // `Self { .. }` / `Type { .. }` with a module path
        if let Some(t) = types.get(last) {
            if !t.is_enum {
                return Ok(Ctor::Struct(t.clone()));
            }
        }
        cerr(format!("cannot find tuple struct or variant `{}`", path.join("::")), pos)
    }

    fn builtin_variant(&self, name: &str, pos: Pos) -> CResult<Ctor> {
        Ok(match name {
            "None" => Ctor::Variant(self.prog.option.clone(), 0),
            "Some" => Ctor::Variant(self.prog.option.clone(), 1),
            "Ok" => Ctor::Variant(self.prog.result.clone(), 0),
            "Err" => Ctor::Variant(self.prog.result.clone(), 1),
            "Less" => Ctor::Variant(self.prog.ordering.clone(), 0),
            "Equal" => Ctor::Variant(self.prog.ordering.clone(), 1),
            "Greater" => Ctor::Variant(self.prog.ordering.clone(), 2),
            _ => return cerr(format!("cannot find `{name}`"), pos),
        })
    }

    // ------------------------------------------------------------ places

    fn place(&mut self, e: &Expr) -> CResult<PlaceCode> {
        let pos = e.pos;
        Ok(match &e.kind {
            ExprKind::Path(p, _) if p.len() == 1 => {
                let Some(v) = self.lookup(&p[0]) else {
                    // assigning to something that isn't a local (a const?) — evaluate into a temporary
                    let c = self.expr(e)?;
                    return Ok(Rc::new(move |cx, f| Ok(Rc::new(Place::Temp(RefCell::new(c(cx, f)?))))));
                };
                let slot = v.slot;
                if v.boxed {
                    Rc::new(move |_, f| {
                        Ok(Rc::new(match &f.slots[slot] {
                            Value::Boxed(b) => Place::Cell(b.clone()),
                            _ => Place::Slot(&mut f.slots[slot] as *mut Value),
                        }))
                    })
                } else {
                    Rc::new(move |_, f| Ok(Rc::new(Place::Slot(&mut f.slots[slot] as *mut Value))))
                }
            }
            ExprKind::Field(base, name) => {
                let b = self.expr_noderef(base)?;
                let name = name.clone();
                let cache: Rc<RefCell<(usize, usize)>> = Rc::new(RefCell::new((0, 0)));
                Rc::new(move |c, f| {
                    let v = b(c, f)?.deref_all();
                    match &v {
                        Value::Struct(s) => {
                            let key = Rc::as_ptr(&s.ty) as usize;
                            let idx = {
                                let ca = cache.borrow();
                                if ca.0 == key { Some(ca.1) } else { None }
                            };
                            let idx = match idx {
                                Some(i) => i,
                                None => {
                                    let Some(i) = s.ty.fields.iter().position(|x| *x == name) else {
                                        return panic(format!("no field `{name}` on type `{}`", s.ty.name), pos);
                                    };
                                    *cache.borrow_mut() = (key, i);
                                    i
                                }
                            };
                            Ok(Rc::new(Place::Field(s.clone(), idx)))
                        }
                        Value::Cell(cell) => {
                            // RefCell<struct> borrowed through a guard
                            let inner = cell.borrow().clone();
                            match inner.deref_all() {
                                Value::Struct(s) => match s.ty.fields.iter().position(|x| *x == name) {
                                    Some(i) => Ok(Rc::new(Place::Field(s.clone(), i))),
                                    None => panic(format!("no field `{name}` on type `{}`", s.ty.name), pos),
                                },
                                other => panic(format!("no field `{name}` on type `{}`", type_name(&other)), pos),
                            }
                        }
                        other => panic(format!("no field `{name}` on type `{}`", type_name(other)), pos),
                    }
                })
            }
            ExprKind::TupleField(base, i) => {
                let b = self.expr_noderef(base)?;
                let i = *i;
                Rc::new(move |c, f| match b(c, f)?.deref_all() {
                    Value::Tuple(t) | Value::Array(t) | Value::Vec(t) => Ok(Rc::new(Place::Elem(t, i))),
                    Value::Struct(s) => Ok(Rc::new(Place::Field(s, i))),
                    other => panic(format!("no field `{i}` on type `{}`", type_name(&other)), pos),
                })
            }
            ExprKind::Index(base, idx) => {
                let b = self.expr_noderef(base)?;
                let ix = self.expr(idx)?;
                Rc::new(move |c, f| {
                    let container = b(c, f)?.deref_all();
                    let i = ix(c, f)?.deref_all();
                    match (&container, &i) {
                        (Value::Vec(v) | Value::Array(v), Value::Int(n, _)) => {
                            let len = v.borrow().len();
                            if *n < 0 || *n as usize >= len {
                                return panic(format!("index out of bounds: the len is {len} but the index is {n}"), pos);
                            }
                            Ok(Rc::new(Place::Elem(v.clone(), *n as usize)))
                        }
                        (Value::Map(m), k) => {
                            if m.borrow().get(k).is_none() {
                                return panic("key not found in map", pos);
                            }
                            Ok(Rc::new(Place::MapVal(m.clone(), k.clone())))
                        }
                        _ => {
                            let v = super::stdlib::index(c, &container, &i, pos)?;
                            Ok(Rc::new(Place::Temp(RefCell::new(v))))
                        }
                    }
                })
            }
            ExprKind::Unary(UnOp::Deref, inner) => {
                if is_place_expr(inner) {
                    let p = self.place(inner)?;
                    Rc::new(move |c, f| {
                        let pl = p(c, f)?;
                        Ok(match pl.get() {
                            Value::Ref(r) => r,
                            Value::Cell(cell) => Rc::new(Place::Cell(cell)),
                            _ => pl,
                        })
                    })
                } else {
                    let v = self.expr(inner)?;
                    Rc::new(move |c, f| {
                        Ok(match v(c, f)? {
                            Value::Ref(r) => r,
                            Value::Cell(cell) => Rc::new(Place::Cell(cell)),
                            Value::Boxed(b) => Rc::new(Place::Cell(b)),
                            other => Rc::new(Place::Temp(RefCell::new(other))),
                        })
                    })
                }
            }
            ExprKind::MethodCall(..) | ExprKind::Call(..) => {
                // e.g. `v.last_mut().unwrap()` returning a reference
                let v = self.expr(e)?;
                Rc::new(move |c, f| {
                    Ok(match v(c, f)? {
                        Value::Ref(r) => r,
                        Value::Cell(cell) => Rc::new(Place::Cell(cell)),
                        other => Rc::new(Place::Temp(RefCell::new(other))),
                    })
                })
            }
            _ => {
                let v = self.expr(e)?;
                Rc::new(move |c, f| Ok(Rc::new(Place::Temp(RefCell::new(v(c, f)?)))))
            }
        })
    }

    /// Evaluate without copying aggregate values (for field/index access and receivers).
    fn expr_noderef(&mut self, e: &Expr) -> CResult<Code> {
        self.expr(e)
    }

    // ------------------------------------------------------------ expressions

    pub fn expr(&mut self, e: &Expr) -> CResult<Code> {
        let pos = e.pos;
        Ok(match &e.kind {
            ExprKind::Lit(l) => {
                let v = lit_value(l, false);
                match v {
                    Value::Str(s) => {
                        let s = s.borrow().clone();
                        code!(|_c, _f| Ok(Value::str(s.clone())))
                    }
                    Value::Array(a) => {
                        let items = a.borrow().clone();
                        code!(|_c, _f| Ok(Value::Array(Rc::new(RefCell::new(items.clone())))))
                    }
                    v => code!(|_c, _f| Ok(v.clone())),
                }
            }
            ExprKind::Path(path, generics) => self.path_expr(path, generics, pos)?,
            ExprKind::Unary(op, inner) => {
                let v = self.expr(inner)?;
                match op {
                    UnOp::Neg => code!(|c, f| {
                        let x = v(c, f)?;
                        negate(c, &x, pos)
                    }),
                    UnOp::Not => code!(|c, f| not(&v(c, f)?, pos)),
                    UnOp::Deref => code!(|c, f| Ok(match v(c, f)? {
                        Value::Ref(p) => p.get(),
                        Value::RcBox(b) => (*b).clone(),
                        Value::Cell(cell) => cell.borrow().clone(),
                        other => other,
                    })),
                }
            }
            ExprKind::Ref(mutable, inner) => {
                if *mutable && is_place_expr(inner) {
                    let p = self.place(inner)?;
                    code!(|c, f| Ok(Value::Ref(p(c, f)?)))
                } else if *mutable {
                    let v = self.expr(inner)?;
                    code!(|c, f| Ok(match v(c, f)? {
                        r @ Value::Ref(_) => r,
                        other => Value::Ref(Rc::new(Place::Temp(RefCell::new(other)))),
                    }))
                } else {
                    self.expr(inner)?
                }
            }
            ExprKind::Binary(op, a, b) => {
                let (ca, cb) = (self.expr(a)?, self.expr(b)?);
                match op {
                    BinOp::And => code!(|c, f| Ok(Value::Bool(ca(c, f)?.truthy() && cb(c, f)?.truthy()))),
                    BinOp::Or => code!(|c, f| Ok(Value::Bool(ca(c, f)?.truthy() || cb(c, f)?.truthy()))),
                    op => {
                        let op = *op;
                        code!(|c, f| {
                            let x = ca(c, f)?;
                            let y = cb(c, f)?;
                            binary(c, op, &x, &y, pos)
                        })
                    }
                }
            }
            ExprKind::Assign(lhs, rhs) => self.assign(lhs, rhs, pos)?,
            ExprKind::AssignOp(op, lhs, rhs) => {
                let p = self.place(lhs)?;
                let r = self.expr(rhs)?;
                let op = *op;
                // user `impl AddAssign`
                let assign_name = match op {
                    BinOp::Add => "add_assign",
                    BinOp::Sub => "sub_assign",
                    BinOp::Mul => "mul_assign",
                    BinOp::Div => "div_assign",
                    _ => "",
                };
                code!(|c, f| {
                    let rv = r(c, f)?;
                    let pl = p(c, f)?;
                    let cur = pl.get();
                    if let Value::Struct(_) | Value::Enum(_) = cur.deref_all() {
                        let tn = type_name(&cur.deref_all());
                        if let Some(code) = c.prog.method(&tn, assign_name) {
                            c.call_code(&code, &[], vec![Value::Ref(pl.clone()), rv], pos)?;
                            return Ok(Value::Unit);
                        }
                    }
                    // `*s += "x"` on a String mutates in place
                    if let (BinOp::Add, Value::Str(s)) = (op, cur.deref_all()) {
                        let add = match rv.deref_all() {
                            Value::Str(x) => x.borrow().clone(),
                            Value::Char(ch) => ch.to_string(),
                            other => return panic(format!("cannot add `{}` to a String", type_name(&other)), pos),
                        };
                        s.borrow_mut().push_str(&add);
                        return Ok(Value::Unit);
                    }
                    let nv = binary(c, op, &cur, &rv, pos)?;
                    // keep the variable's integer type
                    let nv = match (&cur.deref_all(), nv) {
                        (Value::Int(_, k), Value::Int(x, IntKind::Lit)) => Value::Int(x, *k),
                        (_, nv) => nv,
                    };
                    match cur {
                        Value::Ref(inner) => inner.set(nv),
                        _ => pl.set(nv),
                    }
                    Ok(Value::Unit)
                })
            }
            ExprKind::Call(callee, args) => self.call(callee, args, pos)?,
            ExprKind::MethodCall(recv, name, generics, args) => self.method_call(recv, name, generics, args, pos)?,
            ExprKind::Field(..) | ExprKind::TupleField(..) => {
                let p = self.place(e)?;
                code!(|c, f| Ok(p(c, f)?.get()))
            }
            ExprKind::Index(base, idx) => {
                let b = self.expr(base)?;
                let ix = self.expr(idx)?;
                code!(|c, f| {
                    let container = b(c, f)?.deref_all();
                    let i = ix(c, f)?.deref_all();
                    super::stdlib::index(c, &container, &i, pos)
                })
            }
            ExprKind::Tuple(items) => {
                if items.is_empty() {
                    return Ok(code!(|_c, _f| Ok(Value::Unit)));
                }
                let cs: Vec<Code> = items.iter().map(|x| self.expr(x)).collect::<CResult<_>>()?;
                code!(|c, f| {
                    let mut out = Vec::with_capacity(cs.len());
                    for x in &cs {
                        out.push(x(c, f)?.moved());
                    }
                    Ok(Value::tuple(out))
                })
            }
            ExprKind::Array(items) => {
                let cs: Vec<Code> = items.iter().map(|x| self.expr(x)).collect::<CResult<_>>()?;
                code!(|c, f| {
                    let mut out = Vec::with_capacity(cs.len());
                    for x in &cs {
                        out.push(x(c, f)?.moved());
                    }
                    Ok(Value::Array(Rc::new(RefCell::new(out))))
                })
            }
            ExprKind::ArrayRepeat(v, n) => {
                let (cv, cn) = (self.expr(v)?, self.expr(n)?);
                code!(|c, f| {
                    let v = cv(c, f)?;
                    let Value::Int(n, _) = cn(c, f)?.deref_all() else { return panic("array length must be an integer", pos) };
                    let items = (0..n.max(0)).map(|_| v.deep_clone()).collect();
                    Ok(Value::Array(Rc::new(RefCell::new(items))))
                })
            }
            ExprKind::StructLit(path, fields, base) => self.struct_lit(path, fields, base.as_deref(), pos)?,
            ExprKind::Range(a, b, incl) => {
                let ca = match a {
                    Some(a) => Some(self.expr(a)?),
                    None => None,
                };
                let cb = match b {
                    Some(b) => Some(self.expr(b)?),
                    None => None,
                };
                let incl = *incl;
                code!(|c, f| {
                    let mut lo = match &ca {
                        Some(x) => x(c, f)?.deref_all(),
                        None => Value::Unit,
                    };
                    let mut hi = match &cb {
                        Some(x) => x(c, f)?.deref_all(),
                        None => Value::Unit,
                    };
                    // `0..n` takes the type of `n`
                    if let (Value::Int(a, IntKind::Lit), Value::Int(_, k)) = (&lo, &hi) {
                        lo = Value::Int(*a, *k);
                    }
                    if let (Value::Int(_, k), Value::Int(b, IntKind::Lit)) = (&lo, &hi) {
                        hi = Value::Int(*b, *k);
                    }
                    Ok(Value::Range(Rc::new((lo, hi, incl))))
                })
            }
            ExprKind::Block(b) => self.block(b)?,
            ExprKind::If(cond, then, els) => {
                let cc = self.expr(cond)?;
                let ct = self.block(then)?;
                let ce = match els {
                    Some(e) => Some(self.expr(e)?),
                    None => None,
                };
                code!(|c, f| {
                    if cc(c, f)?.truthy() {
                        ct(c, f)
                    } else if let Some(e) = &ce {
                        e(c, f)
                    } else {
                        Ok(Value::Unit)
                    }
                })
            }
            ExprKind::IfLet(pat, scrut, then, els) => {
                let cs = self.expr(scrut)?;
                self.push_scope();
                let binder = self.pattern(pat)?;
                let ct = self.block(then);
                self.pop_scope();
                let ct = ct?;
                let ce = match els {
                    Some(e) => Some(self.expr(e)?),
                    None => None,
                };
                code!(|c, f| {
                    let v = cs(c, f)?;
                    let by_ref = matches!(v, Value::Ref(_));
                    if binder(c, f, v, by_ref)? {
                        ct(c, f)
                    } else if let Some(e) = &ce {
                        e(c, f)
                    } else {
                        Ok(Value::Unit)
                    }
                })
            }
            ExprKind::While(label, cond, body) => {
                let cc = self.expr(cond)?;
                let cb = self.block(body)?;
                let label: Option<Rc<str>> = label.as_deref().map(Rc::from);
                code!(|c, f| {
                    while cc(c, f)?.truthy() {
                        c.tick(pos)?;
                        match cb(c, f) {
                            Ok(_) => {}
                            Err(Ctrl::Break(l, _)) if l.is_none() || l == label => break,
                            Err(Ctrl::Continue(l)) if l.is_none() || l == label => continue,
                            Err(e) => return Err(e),
                        }
                    }
                    Ok(Value::Unit)
                })
            }
            ExprKind::WhileLet(label, pat, scrut, body) => {
                let cs = self.expr(scrut)?;
                self.push_scope();
                let binder = self.pattern(pat)?;
                let cb = self.block(body);
                self.pop_scope();
                let cb = cb?;
                let label: Option<Rc<str>> = label.as_deref().map(Rc::from);
                code!(|c, f| {
                    loop {
                        c.tick(pos)?;
                        let v = cs(c, f)?;
                        let by_ref = matches!(v, Value::Ref(_));
                        if !binder(c, f, v, by_ref)? {
                            break;
                        }
                        match cb(c, f) {
                            Ok(_) => {}
                            Err(Ctrl::Break(l, _)) if l.is_none() || l == label => break,
                            Err(Ctrl::Continue(l)) if l.is_none() || l == label => continue,
                            Err(e) => return Err(e),
                        }
                    }
                    Ok(Value::Unit)
                })
            }
            ExprKind::Loop(label, body) => {
                let cb = self.block(body)?;
                let label: Option<Rc<str>> = label.as_deref().map(Rc::from);
                code!(|c, f| {
                    loop {
                        c.tick(pos)?;
                        match cb(c, f) {
                            Ok(_) => {}
                            Err(Ctrl::Break(l, v)) if l.is_none() || l == label => return Ok(v),
                            Err(Ctrl::Continue(l)) if l.is_none() || l == label => continue,
                            Err(e) => return Err(e),
                        }
                    }
                })
            }
            ExprKind::For(label, pat, iter, body) => self.for_loop(label.as_deref(), pat, iter, body, pos)?,
            ExprKind::Match(scrut, arms) => {
                let cs = self.expr(scrut)?;
                let mut carms: Vec<(Binder, Option<Code>, Code)> = Vec::new();
                for a in arms {
                    self.push_scope();
                    let r = (|| -> CResult<(Binder, Option<Code>, Code)> {
                        let b = self.pattern(&a.pat)?;
                        let g = match &a.guard {
                            Some(g) => Some(self.expr(g)?),
                            None => None,
                        };
                        let body = self.expr(&a.body)?;
                        Ok((b, g, body))
                    })();
                    self.pop_scope();
                    carms.push(r?);
                }
                code!(|c, f| {
                    let v = cs(c, f)?;
                    let by_ref = matches!(v, Value::Ref(_));
                    for (b, g, body) in &carms {
                        if b(c, f, v.clone(), by_ref)? {
                            if let Some(g) = g {
                                if !g(c, f)?.truthy() {
                                    continue;
                                }
                            }
                            return body(c, f);
                        }
                    }
                    panic("non-exhaustive patterns: no `match` arm matched the value", pos)
                })
            }
            ExprKind::Closure(cl) => self.closure(cl, pos)?,
            ExprKind::Return(v) => {
                let cv = match v {
                    Some(v) => {
                        let v = match self.fns.last().and_then(|f| if f.is_closure { None } else { f.ret.clone() }) {
                            Some(t) => inject_hint(v, &t),
                            None => (**v).clone(),
                        };
                        Some(self.expr(&v)?)
                    }
                    None => None,
                };
                code!(|c, f| {
                    let v = match &cv {
                        Some(x) => x(c, f)?,
                        None => Value::Unit,
                    };
                    Err(Ctrl::Return(v))
                })
            }
            ExprKind::Break(label, v) => {
                let cv = match v {
                    Some(v) => Some(self.expr(v)?),
                    None => None,
                };
                let label: Option<Rc<str>> = label.as_deref().map(Rc::from);
                code!(|c, f| {
                    let v = match &cv {
                        Some(x) => x(c, f)?,
                        None => Value::Unit,
                    };
                    Err(Ctrl::Break(label.clone(), v))
                })
            }
            ExprKind::Continue(label) => {
                let label: Option<Rc<str>> = label.as_deref().map(Rc::from);
                code!(|_c, _f| Err(Ctrl::Continue(label.clone())))
            }
            ExprKind::Try(inner) => {
                let ci = self.expr(inner)?;
                code!(|c, f| {
                    let v = ci(c, f)?;
                    if let Some(r) = as_result(&v) {
                        return match r {
                            Ok(x) => Ok(x),
                            Err(e) => Err(Ctrl::Return(c.err(e))),
                        };
                    }
                    if let Some(o) = as_option(&v) {
                        return match o {
                            Some(x) => Ok(x),
                            None => Err(Ctrl::Return(c.none())),
                        };
                    }
                    panic(format!("the `?` operator can only be applied to values of type `Result` or `Option`, found `{}`", type_name(&v)), pos)
                })
            }
            ExprKind::Cast(inner, ty) => {
                let ci = self.expr(inner)?;
                let ty = self.expand_alias(ty);
                code!(|c, f| cast(&ci(c, f)?, &ty, pos))
            }
            ExprKind::Macro(name, args) => self.macro_expr(name, args, pos)?,
            ExprKind::FormatMacro(name, fmt, args, named) => self.format_macro(name, fmt, args, named, pos)?,
        })
    }

    fn assign(&mut self, lhs: &Expr, rhs: &Expr, pos: Pos) -> CResult<Code> {
        let r = self.expr(rhs)?;
        match &lhs.kind {
            ExprKind::Path(p, _) if p.len() == 1 && p[0] == "_" => Ok(code!(|c, f| {
                r(c, f)?;
                Ok(Value::Unit)
            })),
            ExprKind::Tuple(targets) => {
                let places: Vec<PlaceCode> = targets.iter().map(|t| self.place(t)).collect::<CResult<_>>()?;
                Ok(code!(|c, f| {
                    let v = r(c, f)?.deref_all();
                    let Value::Tuple(items) = v else { return panic("destructuring assignment needs a tuple", pos) };
                    let items = items.borrow().clone();
                    for (p, v) in places.iter().zip(items) {
                        p(c, f)?.set(v.moved());
                    }
                    Ok(Value::Unit)
                }))
            }
            ExprKind::Path(p, _) if p.len() == 1 && self.lookup(&p[0]).is_some_and(|v| !v.boxed) => {
                let v = self.lookup(&p[0]).unwrap();
                let slot = v.slot;
                Ok(code!(|c, f| {
                    let nv = r(c, f)?.moved();
                    // keep the variable's integer type for untyped literals
                    let nv = match (&f.slots[slot], nv) {
                        (Value::Int(_, k), Value::Int(x, IntKind::Lit)) => Value::Int(x, *k),
                        (_, nv) => nv,
                    };
                    f.slots[slot] = nv;
                    Ok(Value::Unit)
                }))
            }
            _ => {
                let p = self.place(lhs)?;
                Ok(code!(|c, f| {
                    let nv = r(c, f)?.moved();
                    let pl = p(c, f)?;
                    let nv = match (&pl.get(), nv) {
                        (Value::Int(_, k), Value::Int(x, IntKind::Lit)) => Value::Int(x, *k),
                        (_, nv) => nv,
                    };
                    pl.set(nv);
                    Ok(Value::Unit)
                }))
            }
        }
    }

    fn for_loop(&mut self, label: Option<&str>, pat: &Pat, iter: &Expr, body: &Block, pos: Pos) -> CResult<Code> {
        let ci = self.expr(iter)?;
        self.push_scope();
        let binder = self.pattern(pat)?;
        let cb = self.block(body);
        self.pop_scope();
        let cb = cb?;
        let label: Option<Rc<str>> = label.map(Rc::from);
        Ok(code!(|c, f| {
            let it = ci(c, f)?;
            let by_ref = matches!(it, Value::Ref(_));
            macro_rules! run_body {
                ($v:expr) => {{
                    c.tick(pos)?;
                    let v = $v;
                    let r = matches!(v, Value::Ref(_));
                    if !binder(c, f, v, r)? {
                        return panic("refutable pattern in `for` loop", pos);
                    }
                    match cb(c, f) {
                        Ok(_) => {}
                        Err(Ctrl::Break(l, _)) if l.is_none() || l == label => break,
                        Err(Ctrl::Continue(l)) if l.is_none() || l == label => continue,
                        Err(e) => return Err(e),
                    }
                }};
            }
            match it.deref_all() {
                Value::Range(r) => {
                    if let (Value::Int(a, k), b) = (&r.0, &r.1) {
                        let end = match b {
                            Value::Int(b, _) => {
                                if r.2 {
                                    *b + 1
                                } else {
                                    *b
                                }
                            }
                            Value::Unit => i128::MAX,
                            _ => return panic("range bounds must be integers", pos),
                        };
                        let k = if *k == IntKind::Lit { IntKind::I32 } else { *k };
                        let mut i = *a;
                        while i < end {
                            let cur = i;
                            i += 1;
                            run_body!(Value::Int(cur, k));
                        }
                    } else if let (Value::Char(a), Value::Char(b)) = (&r.0, &r.1) {
                        let end = *b as u32 + if r.2 { 1 } else { 0 };
                        for cp in (*a as u32)..end {
                            if let Some(ch) = char::from_u32(cp) {
                                run_body!(Value::Char(ch));
                            }
                        }
                    } else {
                        return panic("cannot iterate over this range", pos);
                    }
                }
                Value::Vec(v) | Value::Array(v) => {
                    let n = v.borrow().len();
                    let mut i = 0;
                    while i < n {
                        let item = if by_ref { Value::Ref(Rc::new(Place::Elem(v.clone(), i))) } else { v.borrow().get(i).cloned().unwrap_or(Value::Unit) };
                        i += 1;
                        run_body!(item);
                    }
                }
                Value::Map(m) => {
                    let (keys, vals, is_set) = {
                        let m = m.borrow();
                        (m.keys.clone(), m.vals.clone(), m.is_set)
                    };
                    for (k, v) in keys.into_iter().zip(vals) {
                        let item = if is_set { k } else if by_ref { Value::tuple(vec![k.clone(), Value::Ref(Rc::new(Place::MapVal(m.clone(), k)))]) } else { Value::tuple(vec![k, v]) };
                        run_body!(item);
                    }
                }
                Value::Iter(itv) => loop {
                    let next = itv.borrow_mut().pull(c)?;
                    match next {
                        Some(v) => run_body!(v),
                        None => break,
                    }
                },
                Value::Str(_) => return panic("`String` is not an iterator; use `.chars()` or `.bytes()`", pos),
                other => {
                    // user types implementing Iterator
                    let tn = type_name(&other);
                    if let Some(code) = c.prog.method(&tn, "next") {
                        let holder = Rc::new(Place::Temp(RefCell::new(other.clone())));
                        loop {
                            let n = c.call_code(&code, &[], vec![Value::Ref(holder.clone())], pos)?;
                            match as_option(&n).flatten() {
                                Some(v) => run_body!(v),
                                None => break,
                            }
                        }
                    } else {
                        return panic(format!("`{tn}` is not an iterator"), pos);
                    }
                }
            }
            Ok(Value::Unit)
        }))
    }

    fn closure(&mut self, cl: &Closure, pos: Pos) -> CResult<Code> {
        let mut boxed = HashSet::new();
        closure_names(&cl.body, false, &mut boxed);
        self.fns.push(FnCtx { scopes: vec![vec![]], boxed_names: boxed, is_closure: true, ..Default::default() });
        let r = (|| -> CResult<(Vec<Binder>, Code)> {
            let mut binders = Vec::new();
            for (p, _) in &cl.params {
                binders.push(self.pattern(p)?);
            }
            let body = self.expr(&cl.body)?;
            Ok((binders, body))
        })();
        let ctx = self.fns.pop().unwrap();
        let (binders, body) = r?;
        let nparams = binders.len();
        let types: Vec<Option<Type>> = cl.params.iter().map(|(_, t)| t.clone()).collect();
        let prologue: Rc<dyn Fn(&mut Ctx, &mut Frame, Vec<Value>) -> R<()>> = Rc::new(move |c, f, args| {
            // closures passed tuples by iterator adapters accept them either way
            let args = if nparams > 1 && args.len() == 1 {
                match args[0].deref_all() {
                    Value::Tuple(t) => t.borrow().clone(),
                    _ => args,
                }
            } else {
                args
            };
            for (i, b) in binders.iter().enumerate() {
                let a = args.get(i).cloned().unwrap_or(Value::Unit);
                let a = match &types[i] {
                    Some(t) => coerce(a, t),
                    None => a,
                };
                let r = matches!(a, Value::Ref(_));
                if !b(c, f, a, r)? {
                    return panic("refutable pattern in closure argument", pos);
                }
            }
            Ok(())
        });
        let capture_slots: Vec<usize> = ctx.captures.iter().map(|c| c.1).collect();
        let sources: Vec<usize> = ctx.captures.iter().map(|c| c.2).collect();
        let code = Rc::new(FnCode {
            name: "{closure}".into(),
            nslots: ctx.nslots,
            capture_slots,
            nparams,
            prologue,
            body,
            self_kind: None,
            param_types: vec![],
            ret: cl.ret.clone(),
            pos,
        });
        Ok(code!(|_c, f| {
            let mut cells = Vec::with_capacity(sources.len());
            for &s in &sources {
                cells.push(match &f.slots[s] {
                    Value::Boxed(b) => b.clone(),
                    other => {
                        // not boxed (shouldn't happen): box it now so later uses share it
                        let b = Rc::new(RefCell::new(other.clone()));
                        f.slots[s] = Value::Boxed(b.clone());
                        b
                    }
                });
            }
            Ok(Value::Func(Rc::new(FuncVal::Code(code.clone(), cells))))
        }))
    }

    /// Code producing `<ty as Default>::default()`.
    fn default_code(&mut self, ty: &Type, pos: Pos, depth: usize) -> CResult<Code> {
        if depth > 12 {
            return Ok(code!(|_c, _f| panic("`Default` recursion is too deep", pos)));
        }
        Ok(match ty {
            Type::Tuple(ts) => {
                let cs = ts.iter().map(|t| self.default_code(t, pos, depth + 1)).collect::<CResult<Vec<_>>>()?;
                code!(|c, f| {
                    let mut v = Vec::with_capacity(cs.len());
                    for x in cs.iter() {
                        v.push(x(c, f)?);
                    }
                    Ok(if v.is_empty() { Value::Unit } else { Value::tuple(v) })
                })
            }
            Type::Ref(_, t) => self.default_code(t, pos, depth + 1)?,
            Type::Array(t, Some(n)) => {
                let inner = self.default_code(t, pos, depth + 1)?;
                let n = self.expr(n)?;
                code!(|c, f| {
                    let n = match n(c, f)? {
                        Value::Int(i, _) => i.max(0) as usize,
                        _ => 0,
                    };
                    let mut v = Vec::with_capacity(n);
                    for _ in 0..n {
                        v.push(inner(c, f)?);
                    }
                    Ok(Value::Array(Rc::new(RefCell::new(v))))
                })
            }
            Type::Path(p, args) => {
                let mut name = p.last().cloned().unwrap_or_default();
                if name == "Self" {
                    name = self.self_type.clone().unwrap_or(name);
                }
                if let Some(t) = self.aliases.get(&name) {
                    if t.name() != name {
                        return self.default_code(&t.clone(), pos, depth + 1);
                    }
                }
                if self.prog.types.borrow().contains_key(&name) {
                    let fname = format!("{name}::default");
                    let field_codes = match self.field_types.get(&name).cloned() {
                        Some(ft) => Some(ft.iter().map(|(_, t)| self.default_code(t, pos, depth + 1)).collect::<CResult<Vec<_>>>()?),
                        None => None,
                    };
                    return Ok(code!(|c, f| {
                        let user = c.prog.fns.borrow().get(&fname).and_then(|cell| cell.borrow().clone());
                        if let Some(code) = user {
                            return c.call_code(&code, &[], vec![], pos);
                        }
                        let ti = c.prog.types.borrow().get(&name).cloned();
                        match (ti, &field_codes) {
                            (Some(ti), Some(fc)) if !ti.is_enum => {
                                let mut vals = Vec::with_capacity(fc.len());
                                for x in fc.iter() {
                                    vals.push(x(c, f)?);
                                }
                                Ok(Value::Struct(Rc::new(StructVal { ty: ti, fields: RefCell::new(vals) })))
                            }
                            _ => panic(format!("the trait `Default` is not implemented for `{name}`"), pos),
                        }
                    }));
                }
                let inner = match args.first() {
                    Some(t) if matches!(name.as_str(), "Box" | "Rc" | "Arc" | "RefCell" | "Cell" | "Mutex" | "RwLock") => Some(self.default_code(t, pos, depth + 1)?),
                    _ => None,
                };
                let args = args.clone();
                code!(|c, f| {
                    Ok(match (name.as_str(), &inner) {
                        ("Option", _) => c.none(),
                        ("Box", Some(i)) => i(c, f)?,
                        ("Rc" | "Arc", Some(i)) => Value::RcBox(Rc::new(i(c, f)?)),
                        ("RefCell" | "Cell" | "Mutex" | "RwLock", Some(i)) => Value::Cell(Rc::new(RefCell::new(i(c, f)?))),
                        _ => super::stdlib::call_assoc(c, &name, "default", &args, vec![], pos)?,
                    })
                })
            }
            _ => code!(|_c, _f| Ok(Value::Unit)),
        })
    }

    fn struct_lit(&mut self, path: &[String], fields: &[(String, Expr)], base: Option<&Expr>, pos: Pos) -> CResult<Code> {
        let target = self.resolve_ctor(path, pos)?;
        let (names, ty_name): (Vec<String>, String) = match &target {
            Ctor::Struct(t) => (t.fields.clone(), t.name.clone()),
            Ctor::Variant(t, i) => (t.variants[*i].1.clone(), t.name.clone()),
        };
        let ftypes = self.field_types.get(&ty_name).cloned().unwrap_or_default();
        let mut slots: Vec<Option<(Code, Option<Type>)>> = (0..names.len()).map(|_| None).collect();
        for (n, e) in fields {
            let Some(i) = names.iter().position(|x| x == n) else { return cerr(format!("struct `{ty_name}` has no field named `{n}`"), e.pos) };
            let fty = ftypes.iter().find(|(x, _)| x == n).map(|(_, t)| t.clone());
            let e = match &fty {
                Some(t) => inject_hint(e, t),
                None => e.clone(),
            };
            slots[i] = Some((self.expr(&e)?, fty));
        }
        let base_code = match base {
            Some(b) => Some(self.expr(&inject_hint(b, &Type::Path(vec![ty_name.clone()], vec![])))?),
            None => {
                if let Some(missing) = slots.iter().position(|s| s.is_none()) {
                    return cerr(format!("missing field `{}` in initializer of `{ty_name}`", names[missing]), pos);
                }
                None
            }
        };
        let slots: Rc<[Option<(Code, Option<Type>)>]> = slots.into();
        Ok(code!(|c, f| {
            let base_v = match &base_code {
                Some(b) => Some(b(c, f)?.deref_all()),
                None => None,
            };
            let base_fields = match &base_v {
                Some(Value::Struct(s)) => s.fields.borrow().clone(),
                _ => vec![],
            };
            let mut vals = Vec::with_capacity(slots.len());
            for (i, s) in slots.iter().enumerate() {
                vals.push(match s {
                    Some((code, ty)) => {
                        let v = code(c, f)?.moved();
                        match ty {
                            Some(t) => coerce(v, t),
                            None => v,
                        }
                    }
                    None => base_fields.get(i).map(|v| v.deep_clone()).unwrap_or(Value::Unit),
                });
            }
            Ok(match &target {
                Ctor::Struct(t) => Value::Struct(Rc::new(StructVal { ty: t.clone(), fields: RefCell::new(vals) })),
                Ctor::Variant(t, i) => Value::Enum(Rc::new(EnumVal { ty: t.clone(), variant: *i, fields: RefCell::new(vals) })),
            })
        }))
    }

    // ------------------------------------------------------------ paths and calls

    fn path_expr(&mut self, path: &[String], generics: &[Type], pos: Pos) -> CResult<Code> {
        if path.len() == 1 {
            let name = &path[0];
            if let Some(v) = self.lookup(name) {
                return Ok(Self::read_var(v));
            }
        }
        let full: Vec<String> = path.iter().map(|s| if s == "Self" { self.self_type.clone().unwrap_or_else(|| "Self".into()) } else { s.clone() }).collect();
        let joined = full.join("::");
        // constants
        let const_key = if self.const_decls.contains_key(&joined) {
            Some(joined.clone())
        } else if full.len() >= 2 && self.const_decls.contains_key(&full[full.len() - 2..].join("::")) {
            Some(full[full.len() - 2..].join("::"))
        } else if self.const_decls.contains_key(full.last().unwrap()) && full.len() > 1 && !self.prog.types.borrow().contains_key(&full[full.len() - 2]) {
            Some(full.last().unwrap().clone())
        } else {
            None
        };
        if let Some(k) = const_key {
            let entry = self.const_code(&k)?;
            return Ok(code!(|c, _f| {
                if let Some(v) = entry.2.borrow().as_ref() {
                    return Ok(v.clone().moved());
                }
                let mut fr = Frame { slots: vec![Value::Unit; entry.1.max(1)] };
                let v = (entry.0)(c, &mut fr)?;
                *entry.2.borrow_mut() = Some(v.clone());
                Ok(v.moved())
            }));
        }
        // enum variants / unit structs
        if let Ok(ctor) = self.resolve_ctor(&full, pos) {
            match ctor {
                Ctor::Variant(t, i) => {
                    let (_, fields, is_tuple, _) = t.variants[i].clone();
                    if fields.is_empty() {
                        return Ok(code!(|_c, _f| Ok(Value::Enum(Rc::new(EnumVal { ty: t.clone(), variant: i, fields: RefCell::new(vec![]) })))));
                    }
                    if is_tuple {
                        // a tuple variant used as a function value, e.g. `.map(Some)`
                        let n = fields.len();
                        return Ok(code!(|_c, _f| {
                            let t = t.clone();
                            let nf: NativeFn = Rc::new(move |_c, args, pos| {
                                if args.len() != n {
                                    return panic(format!("expected {n} arguments"), pos);
                                }
                                Ok(Value::Enum(Rc::new(EnumVal { ty: t.clone(), variant: i, fields: RefCell::new(args) })))
                            });
                            Ok(Value::Func(Rc::new(FuncVal::Native("variant".into(), nf))))
                        }));
                    }
                }
                Ctor::Struct(t) => {
                    if t.fields.is_empty() {
                        return Ok(code!(|_c, _f| Ok(Value::Struct(Rc::new(StructVal { ty: t.clone(), fields: RefCell::new(vec![]) })))));
                    }
                    if t.is_tuple {
                        return Ok(code!(|_c, _f| {
                            let t = t.clone();
                            let nf: NativeFn = Rc::new(move |_c, args, _pos| Ok(Value::Struct(Rc::new(StructVal { ty: t.clone(), fields: RefCell::new(args) }))));
                            Ok(Value::Func(Rc::new(FuncVal::Native("ctor".into(), nf))))
                        }));
                    }
                }
            }
        }
        // functions as values
        if let Some(cell) = self.resolve_fn(&full) {
            return Ok(code!(|_c, _f| {
                let code = cell.borrow().clone();
                match code {
                    Some(code) => Ok(Value::Func(Rc::new(FuncVal::Code(code, vec![])))),
                    None => panic("function is not compiled", pos),
                }
            }));
        }
        if let Some(v) = builtin_const(&full) {
            return Ok(code!(|_c, _f| Ok(v.clone())));
        }
        // builtin associated functions used as values (`.map(String::from)`)
        if full.len() >= 2 && super::stdlib::has_assoc(&full[full.len() - 2], &full[full.len() - 1]) {
            let ty = full[full.len() - 2].clone();
            let name = full[full.len() - 1].clone();
            let generics = generics.to_vec();
            return Ok(code!(|_c, _f| {
                let (ty, name, generics) = (ty.clone(), name.clone(), generics.clone());
                let label = name.clone();
                let nf: NativeFn = Rc::new(move |c, args, pos| super::stdlib::call_assoc(c, &ty, &name, &generics, args, pos));
                Ok(Value::Func(Rc::new(FuncVal::Native(label.clone(), nf))))
            }));
        }
        if full.len() == 1 && super::web::has_fn(&full[0]) {
            let name = full[0].clone();
            return Ok(code!(|_c, _f| {
                let name = name.clone();
                let label = name.clone();
                let nf: NativeFn = Rc::new(move |c, args, pos| super::web::call_fn(c, &name, args, pos));
                Ok(Value::Func(Rc::new(FuncVal::Native(label.clone(), nf))))
            }));
        }
        cerr(format!("cannot find value `{joined}` in this scope"), pos)
    }

    fn resolve_fn(&self, full: &[String]) -> Option<Rc<RefCell<Option<Rc<FnCode>>>>> {
        let joined = full.join("::");
        if let Some(c) = self.fn_cell(&joined) {
            return Some(c);
        }
        // `crate::util::f`, `self::f`, `super::f`, `module::f`
        if full.len() >= 2 {
            let last2 = full[full.len() - 2..].join("::");
            if let Some(c) = self.fn_cell(&last2) {
                return Some(c);
            }
            let ty = &full[full.len() - 2];
            let first_lower = ty.chars().next().is_some_and(|c| c.is_lowercase());
            if first_lower || matches!(ty.as_str(), "crate" | "self" | "super") {
                if let Some(c) = self.fn_cell(full.last().unwrap()) {
                    return Some(c);
                }
            }
            // aliases: `type Grid = Board;`
            if let Some(t) = self.aliases.get(ty) {
                if let Some(c) = self.fn_cell(&format!("{}::{}", t.name(), full.last().unwrap())) {
                    return Some(c);
                }
            }
        }
        None
    }

    fn call(&mut self, callee: &Expr, args: &[Expr], pos: Pos) -> CResult<Code> {
        let cargs: Vec<Code> = args.iter().map(|a| self.expr(a)).collect::<CResult<_>>()?;
        let eval_args = move |c: &mut Ctx, f: &mut Frame, cargs: &[Code]| -> R<Vec<Value>> {
            let mut out = Vec::with_capacity(cargs.len());
            for a in cargs {
                out.push(a(c, f)?.moved());
            }
            Ok(out)
        };
        if let ExprKind::Path(path, generics) = &callee.kind {
            let is_local = path.len() == 1 && self.lookup(&path[0]).is_some();
            if !is_local {
                let full: Vec<String> = path.iter().map(|s| if s == "Self" { self.self_type.clone().unwrap_or_else(|| "Self".into()) } else { s.clone() }).collect();
                // user functions
                if let Some(cell) = self.resolve_fn(&full) {
                    let name = full.join("::");
                    return Ok(code!(|c, f| {
                        let args = eval_args(c, f, &cargs)?;
                        let code = cell.borrow().clone();
                        match code {
                            Some(code) => c.call_code(&code, &[], args, pos),
                            None => panic(format!("function `{name}` is declared but has no body"), pos),
                        }
                    }));
                }
                // `Type::default()` without a user `impl Default`
                if full.len() >= 2 && full[full.len() - 1] == "default" && cargs.is_empty() {
                    if full.len() == 2 && full[0] == "Default" {
                        return Ok(code!(|_c, _f| Ok(Value::Int(0, IntKind::Lit))));
                    }
                    return self.default_code(&Type::Path(full[..full.len() - 1].to_vec(), generics.clone()), pos, 0);
                }
                // tuple structs and variants
                if let Ok(ctor) = self.resolve_ctor(&full, pos) {
                    return Ok(match ctor {
                        Ctor::Variant(t, i) => {
                            let n = t.variants[i].1.len();
                            code!(|c, f| {
                                let args = eval_args(c, f, &cargs)?;
                                if args.len() != n {
                                    return panic(format!("this enum variant takes {n} arguments but {} were supplied", args.len()), pos);
                                }
                                Ok(Value::Enum(Rc::new(EnumVal { ty: t.clone(), variant: i, fields: RefCell::new(args) })))
                            })
                        }
                        Ctor::Struct(t) => {
                            let ftypes: Vec<Type> = self.field_types.get(&t.name).map(|v| v.iter().map(|(_, t)| t.clone()).collect()).unwrap_or_default();
                            code!(|c, f| {
                                let args = eval_args(c, f, &cargs)?;
                                let args = args.into_iter().enumerate().map(|(i, a)| match ftypes.get(i) {
                                    Some(t) => coerce(a, t),
                                    None => a,
                                }).collect();
                                Ok(Value::Struct(Rc::new(StructVal { ty: t.clone(), fields: RefCell::new(args) })))
                            })
                        }
                    });
                }
                // `Trait::method(x, ..)` / `Type::method(x, ..)` on types we don't know statically
                if full.len() >= 2 && (self.traits.contains_key(&full[full.len() - 2]) || full[full.len() - 2] == "Self" || full[0] == "T") {
                    let m = full.last().unwrap().clone();
                    let g = generics.clone();
                    return Ok(code!(|c, f| {
                        let mut args = eval_args(c, f, &cargs)?;
                        if args.is_empty() {
                            return panic(format!("cannot call `{m}` without a receiver"), pos);
                        }
                        let recv = args.remove(0);
                        call_method(c, recv, None, &m, &g, args, pos)
                    }));
                }
                // builtin associated functions: String::from, Vec::new, HashMap::new, ...
                if full.len() >= 2 {
                    let ty = full[full.len() - 2].clone();
                    let ty = self.aliases.get(&ty).map(|t| t.name().to_string()).unwrap_or(ty);
                    let name = full[full.len() - 1].clone();
                    if super::stdlib::has_assoc(&ty, &name) {
                        let mut generics = generics.clone();
                        if generics.is_empty() {
                            if let Some(t) = self.aliases.get(&full[full.len() - 2]) {
                                generics = t.args().to_vec();
                            }
                        }
                        return Ok(code!(|c, f| {
                            let args = eval_args(c, f, &cargs)?;
                            super::stdlib::call_assoc(c, &ty, &name, &generics, args, pos)
                        }));
                    }
                    // a user type with a method called through its path, e.g. `Point::distance(&a, &b)`
                    if self.prog.types.borrow().contains_key(&ty) {
                        let m = name.clone();
                        return Ok(code!(|c, f| {
                            let mut args = eval_args(c, f, &cargs)?;
                            if args.is_empty() {
                                return panic(format!("no function or associated item named `{m}` found"), pos);
                            }
                            let recv = args.remove(0);
                            call_method(c, recv, None, &m, &[], args, pos)
                        }));
                    }
                }
                if full.len() == 1 && super::web::has_fn(&full[0]) {
                    let name = full[0].clone();
                    return Ok(code!(|c, f| {
                        let args = eval_args(c, f, &cargs)?;
                        super::web::call_fn(c, &name, args, pos)
                    }));
                }
                if full.len() == 2 && full[0] == "web" && super::web::has_fn(&full[1]) {
                    let name = full[1].clone();
                    return Ok(code!(|c, f| {
                        let args = eval_args(c, f, &cargs)?;
                        super::web::call_fn(c, &name, args, pos)
                    }));
                }
                if path.len() == 1 && !self.const_decls.contains_key(&path[0]) {
                    return cerr(format!("cannot find function `{}` in this scope", path[0]), pos);
                }
            }
        }
        let cf = self.expr(callee)?;
        Ok(code!(|c, f| {
            let fv = cf(c, f)?;
            let args = eval_args(c, f, &cargs)?;
            c.call_value(&fv, args, pos)
        }))
    }

    fn method_call(&mut self, recv: &Expr, name: &str, generics: &[Type], args: &[Expr], pos: Pos) -> CResult<Code> {
        let cargs: Vec<Code> = args.iter().map(|a| self.expr(a)).collect::<CResult<_>>()?;
        let name = name.to_string();
        let generics: Vec<Type> = generics.iter().map(|t| self.expand_alias(t)).collect();
        if is_place_expr(recv) {
            let p = self.place(recv)?;
            Ok(code!(|c, f| {
                let pl = p(c, f)?;
                let v = pl.get();
                let mut args = Vec::with_capacity(cargs.len());
                for a in &cargs {
                    args.push(a(c, f)?.moved());
                }
                call_method(c, v, Some(pl), &name, &generics, args, pos)
            }))
        } else {
            let r = self.expr(recv)?;
            Ok(code!(|c, f| {
                let v = r(c, f)?;
                let mut args = Vec::with_capacity(cargs.len());
                for a in &cargs {
                    args.push(a(c, f)?.moved());
                }
                call_method(c, v, None, &name, &generics, args, pos)
            }))
        }
    }

    // ------------------------------------------------------------ macros

    fn macro_expr(&mut self, name: &str, args: &[Expr], pos: Pos) -> CResult<Code> {
        let cargs: Vec<Code> = args.iter().map(|a| self.expr(a)).collect::<CResult<_>>()?;
        Ok(match name {
            "vec" => code!(|c, f| {
                let mut out = Vec::with_capacity(cargs.len());
                for a in &cargs {
                    out.push(a(c, f)?.moved());
                }
                Ok(Value::vec(out))
            }),
            "vec_repeat" => code!(|c, f| {
                let v = cargs[0](c, f)?;
                let Value::Int(n, _) = cargs[1](c, f)?.deref_all() else { return panic("`vec![x; n]` needs an integer length", pos) };
                Ok(Value::vec((0..n.max(0)).map(|_| v.deep_clone()).collect()))
            }),
            "assert" => code!(|c, f| {
                if !cargs[0](c, f)?.truthy() {
                    let msg = match cargs.get(1) {
                        Some(m) => {
                            let mv = m(c, f)?;
                            display_of(c, &mv, pos)?
                        }
                        None => format!("assertion failed: {}", "condition"),
                    };
                    return panic(msg, pos);
                }
                Ok(Value::Unit)
            }),
            "assert_eq" | "assert_ne" => {
                let want_eq = name == "assert_eq";
                code!(|c, f| {
                    let a = cargs[0](c, f)?;
                    let b = cargs[1](c, f)?;
                    if user_eq(c, &a.deref_all(), &b.deref_all(), pos)? != want_eq {
                        let l = super::format::debug(c, &a, false, 0, pos)?;
                        let r = super::format::debug(c, &b, false, 0, pos)?;
                        let extra = match cargs.get(2) {
                            Some(m) => {
                                let mv = m(c, f)?;
                                format!(": {}", display_of(c, &mv, pos)?)
                            }
                            None => String::new(),
                        };
                        return panic(format!("assertion `left {} right` failed{extra}\n  left: {l}\n right: {r}", if want_eq { "==" } else { "!=" }), pos);
                    }
                    Ok(Value::Unit)
                })
            }
            "dbg" => code!(|c, f| {
                let mut last = Value::Unit;
                for a in &cargs {
                    let v = a(c, f)?;
                    let s = super::format::debug(c, &v, true, 0, pos)?;
                    let file = c.prog.file.clone();
                    c.print("log", format!("[{file}:{}:{}] {s}", pos.line, pos.col));
                    last = v;
                }
                Ok(last)
            }),
            other => return cerr(format!("cannot find macro `{other}` in this scope"), pos),
        })
    }

    fn format_macro(&mut self, name: &str, fmt: &str, args: &[Expr], named: &[(String, Expr)], pos: Pos) -> CResult<Code> {
        let pieces = format::parse(fmt).map_err(|m| CompileError { msg: m, pos })?;
        let is_write = name == "write" || name == "writeln";
        let (target, args) = if is_write && !args.is_empty() { (Some(self.expr(&args[0])?), &args[1..]) } else { (None, args) };
        let cargs: Vec<Code> = args.iter().map(|a| self.expr(a)).collect::<CResult<_>>()?;
        // explicit named args, then implicit `{name}` captures of variables
        let mut cnamed: Vec<(String, Code)> = named.iter().map(|(n, e)| Ok((n.clone(), self.expr(e)?))).collect::<CResult<_>>()?;
        let mut implicit = HashSet::new();
        for p in &pieces {
            if let Piece::Arg(s) = p {
                if let ArgRef::Name(n) = &s.arg {
                    implicit.insert(n.clone());
                }
                for c in [&s.width, &s.precision].into_iter().flatten() {
                    if let Count::Arg(ArgRef::Name(n)) = c {
                        implicit.insert(n.clone());
                    }
                }
            }
        }
        for n in implicit {
            if !cnamed.iter().any(|(k, _)| *k == n) {
                let e = Expr { kind: ExprKind::Path(vec![n.clone()], vec![]), pos };
                cnamed.push((n, self.expr(&e)?));
            }
        }
        let max_index = pieces.iter().filter_map(|p| if let Piece::Arg(s) = p { if let ArgRef::Index(i) = s.arg { Some(i) } else { None } } else { None }).max();
        if let Some(m) = max_index {
            if m >= cargs.len() {
                return cerr(format!("{} positional argument{} in format string, but there {} {} argument{}", m + 1, if m == 0 { "" } else { "s" }, if cargs.len() == 1 { "is" } else { "are" }, cargs.len(), if cargs.len() == 1 { "" } else { "s" }), pos);
            }
        }
        let pieces: Rc<[Piece]> = pieces.into();
        let name = name.to_string();
        Ok(code!(|c, f| {
            let mut vals = Vec::with_capacity(cargs.len());
            for a in &cargs {
                vals.push(a(c, f)?);
            }
            let mut nvals = Vec::with_capacity(cnamed.len());
            for (n, a) in &cnamed {
                nvals.push((n.clone(), a(c, f)?));
            }
            let mut s = super::format::run(c, &pieces, &vals, &nvals, pos)?;
            match name.as_str() {
                "println" => {
                    c.print("log", s);
                    Ok(Value::Unit)
                }
                "print" => {
                    c.print("print", s);
                    Ok(Value::Unit)
                }
                "eprintln" | "eprint" => {
                    c.print("error", s);
                    Ok(Value::Unit)
                }
                "format" | "format_args" => Ok(Value::str(s)),
                "panic" => panic(if s.is_empty() { "explicit panic".to_string() } else { s }, pos),
                "unreachable" => panic(if s.is_empty() { "internal error: entered unreachable code".to_string() } else { format!("internal error: entered unreachable code: {s}") }, pos),
                "todo" => panic(if s.is_empty() { "not yet implemented".to_string() } else { format!("not yet implemented: {s}") }, pos),
                "unimplemented" => panic(if s.is_empty() { "not implemented".to_string() } else { format!("not implemented: {s}") }, pos),
                _ => {
                    // write! / writeln!
                    if name == "writeln" {
                        s.push('\n');
                    }
                    let t = match &target {
                        Some(t) => t(c, f)?,
                        None => return panic("write! needs a destination", pos),
                    };
                    match t.deref_all() {
                        Value::Formatter(buf, _) => buf.borrow_mut().push_str(&s),
                        Value::Str(buf) => buf.borrow_mut().push_str(&s),
                        other => return panic(format!("cannot write to `{}`", type_name(&other)), pos),
                    }
                    Ok(c.ok(Value::Unit))
                }
            }
        }))
    }
}

fn display_of(c: &mut Ctx, v: &Value, pos: Pos) -> R<String> {
    super::format::display(c, v, pos)
}

#[derive(Clone)]
enum Ctor {
    Variant(Rc<TypeInfo>, usize),
    Struct(Rc<TypeInfo>),
}

fn deref_for_match(v: Value, by_ref: bool) -> (Value, bool) {
    match v {
        Value::Ref(p) => (p.get().deref_all(), true),
        Value::Boxed(b) => (b.borrow().clone().deref_all(), by_ref),
        Value::RcBox(b) => ((*b).clone().deref_all(), by_ref),
        v => (v, by_ref),
    }
}

#[allow(clippy::too_many_arguments)]
fn match_seq(c: &mut Ctx, f: &mut Frame, items: &[Value], before: &[Binder], after: &[Binder], rest: Option<&Binder>, by_ref: bool, place: impl Fn(usize) -> Place) -> R<bool> {
    let n = items.len();
    if rest.is_none() {
        if n != before.len() {
            return Ok(false);
        }
    } else if n < before.len() + after.len() {
        return Ok(false);
    }
    let item = |i: usize| if by_ref { Value::Ref(Rc::new(place(i))) } else { items[i].clone() };
    for (i, b) in before.iter().enumerate() {
        if !b(c, f, item(i), by_ref)? {
            return Ok(false);
        }
    }
    let start_after = n - after.len();
    for (j, b) in after.iter().enumerate() {
        if !b(c, f, item(start_after + j), by_ref)? {
            return Ok(false);
        }
    }
    if let Some(r) = rest {
        let slice = Value::vec(items[before.len()..start_after].to_vec());
        if !r(c, f, slice, by_ref)? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn lit_value(l: &Lit, neg: bool) -> Value {
    match l {
        Lit::Int(v, suffix) => {
            let k = suffix.as_deref().and_then(IntKind::from_name).unwrap_or(IntKind::Lit);
            if matches!(suffix.as_deref(), Some("f32") | Some("f64")) {
                let f = *v as f64;
                return Value::Float(if neg { -f } else { f }, suffix.as_deref() == Some("f32"));
            }
            let v = *v as i128;
            Value::Int(if neg { -v } else { v }, k)
        }
        Lit::Float(v, suffix) => Value::Float(if neg { -v } else { *v }, suffix.as_deref() == Some("f32")),
        Lit::Str(s) => Value::str(s.clone()),
        Lit::ByteStr(b) => Value::Array(Rc::new(RefCell::new(b.iter().map(|x| Value::Int(*x as i128, IntKind::U8)).collect()))),
        Lit::Char(c) => Value::Char(*c),
        Lit::Byte(b) => Value::Int(*b as i128, IntKind::U8),
        Lit::Bool(b) => Value::Bool(*b),
    }
}

/// `i32::MAX`, `f64::consts::PI`, `u8::MIN`, ...
pub fn builtin_const(path: &[String]) -> Option<Value> {
    let n = path.len();
    if n < 2 {
        return None;
    }
    let (ty, name) = (path[n - 2].as_str(), path[n - 1].as_str());
    if let Some(k) = IntKind::from_name(ty) {
        let (lo, hi) = k.range();
        return match name {
            "MAX" => Some(Value::Int(if k == IntKind::U128 { i128::MAX } else { hi }, k)),
            "MIN" => Some(Value::Int(lo, k)),
            "BITS" => Some(Value::Int(k.bits() as i128, IntKind::U32)),
            _ => None,
        };
    }
    let f32 = ty == "f32" || (n >= 3 && path[n - 3] == "f32");
    let fv = |v: f64| Some(Value::Float(v, f32));
    match (ty, name) {
        ("f64" | "f32", "MAX") => fv(if f32 { f32::MAX as f64 } else { f64::MAX }),
        ("f64" | "f32", "MIN") => fv(if f32 { f32::MIN as f64 } else { f64::MIN }),
        ("f64" | "f32", "EPSILON") => fv(if f32 { f32::EPSILON as f64 } else { f64::EPSILON }),
        ("f64" | "f32", "INFINITY") => fv(f64::INFINITY),
        ("f64" | "f32", "NEG_INFINITY") => fv(f64::NEG_INFINITY),
        ("f64" | "f32", "NAN") => fv(f64::NAN),
        ("f64" | "f32", "MIN_POSITIVE") => fv(f64::MIN_POSITIVE),
        ("consts", "PI") => fv(std::f64::consts::PI),
        ("consts", "TAU") => fv(std::f64::consts::TAU),
        ("consts", "E") => fv(std::f64::consts::E),
        ("consts", "SQRT_2") => fv(std::f64::consts::SQRT_2),
        ("consts", "LN_2") => fv(std::f64::consts::LN_2),
        ("consts", "LN_10") => fv(std::f64::consts::LN_10),
        ("consts", "FRAC_PI_2") => fv(std::f64::consts::FRAC_PI_2),
        ("consts", "FRAC_PI_4") => fv(std::f64::consts::FRAC_PI_4),
        ("consts", "FRAC_1_SQRT_2") => fv(std::f64::consts::FRAC_1_SQRT_2),
        ("char", "MAX") => Some(Value::Char(char::MAX)),
        _ => None,
    }
}

fn flatten(items: &[Item], out: &mut Vec<Item>) {
    for it in items {
        match it {
            Item::Mod { items, .. } => flatten(items, out),
            other => out.push(other.clone()),
        }
    }
}

fn collect_nested(it: &Item, out: &mut Vec<Item>) {
    let walk_block = |b: &Block, out: &mut Vec<Item>| {
        fn walk(b: &Block, out: &mut Vec<Item>) {
            for s in &b.stmts {
                if let Stmt::Item(i) = s {
                    match i {
                        Item::Mod { items, .. } => {
                            let mut f = Vec::new();
                            flatten(items, &mut f);
                            out.extend(f);
                        }
                        other => out.push(other.clone()),
                    }
                    if let Item::Fn(d) = i {
                        if let Some(body) = &d.body {
                            walk(body, out);
                        }
                    }
                }
            }
        }
        walk(b, out)
    };
    match it {
        Item::Fn(d) => {
            if let Some(b) = &d.body {
                walk_block(b, out);
            }
        }
        Item::Impl { items, .. } => {
            for i in items {
                if let Item::Fn(d) = i {
                    if let Some(b) = &d.body {
                        walk_block(b, out);
                    }
                }
            }
        }
        _ => {}
    }
}
