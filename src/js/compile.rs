//! Closure compiler for JavaScript.
//!
//! Scripts are compiled once into trees of Rust closures instead of being re-walked as syntax
//! on every run. The main win is variables: each is resolved at compile time to a slot in its
//! function's frame, or to a shared cell when an inner function captures it. Lookups by name
//! remain only for globals (and module scope).
//!
//! Compiled functions are ordinary `Func::Closure` objects with `compiled` set, so calls,
//! classes, generators and async functions go through the interpreter's existing machinery.
//! Code the compiler doesn't handle yet makes `compile_script` fail, and the caller runs the
//! script with the tree-walking interpreter instead.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use super::ast::*;
use super::coro::Yield;
use super::interp::{callee_desc, is_anon_fn, strict_equals, throw, Env, Flow, Interp, R};
use super::value::*;

pub type Cell = Rc<RefCell<Value>>;
pub type ECode = Rc<dyn Fn(&mut Interp, &mut Frame) -> R<Value>>;
pub type SCode = Rc<dyn Fn(&mut Interp, &mut Frame) -> R<Flow>>;
type Binder = Rc<dyn Fn(&mut Interp, &mut Frame, Value) -> R<()>>;

fn cell(v: Value) -> Cell {
    Rc::new(RefCell::new(v))
}

/// The activation record of a compiled function (or script).
pub struct Frame {
    pub slots: Vec<Value>,
    pub cells: Vec<Cell>,
    pub caps: Rc<[Cell]>,
    pub this: Value,
    pub func: Option<Obj>,
    pub new_target: Value,
    pub home: Option<Obj>,
    /// Scope for names not resolved at compile time (globals, module scope).
    pub env: Env,
    pub args: Vec<Value>,
}

/// Where a captured variable comes from in the frame that creates the closure.
#[derive(Clone, Copy, Debug)]
pub enum CapSrc {
    Cell(usize),
    Cap(usize),
}

/// A compiled function body.
pub struct CFunc {
    pub nslots: usize,
    pub ncells: usize,
    pub caps: Vec<CapSrc>,
    /// Binds parameters, `arguments`, hoisted functions.
    pub prologue: Rc<dyn Fn(&mut Interp, &mut Frame) -> R<()>>,
    pub body: SCode,
    /// Where `this` lives (constructors read it back at the end).
    pub this_loc: Loc,
}

#[derive(Clone, Debug)]
pub enum Loc {
    Slot(usize),
    Cell(usize),
    Cap(usize),
    /// Looked up by name in the frame's env (globals, module scope).
    Env(Name),
    This,
    NewTarget,
    FuncObj,
    Home,
}

#[derive(Debug)]
pub struct Unsupported(pub String);

type CResult<T> = Result<T, Unsupported>;

fn unsupported<T>(what: &str) -> CResult<T> {
    Err(Unsupported(what.to_string()))
}

// ---------------------------------------------------------------- capture analysis

const P_THIS: &str = "%this";
const P_NT: &str = "%nt";
const P_FUNC: &str = "%func";
const P_HOME: &str = "%home";

/// Names that nested functions refer to: variables with these names must live in cells.
#[derive(Default)]
struct Captured {
    names: HashSet<Name>,
}

impl Captured {
    fn of_body(stmts: &[Stmt], params: &[Param], rest: Option<&Pattern>) -> HashSet<Name> {
        let mut c = Captured::default();
        for p in params {
            c.pattern(&p.pattern, None);
            if let Some(d) = &p.default {
                c.expr(d, None);
            }
        }
        if let Some(r) = rest {
            c.pattern(r, None);
        }
        for s in stmts {
            c.stmt(s, None);
        }
        c.names
    }

    /// `inner`: Some(pseudo_ok) while inside a nested function; pseudo names (`this` etc.)
    /// only count while every function between here and the reference is an arrow.
    fn name(&mut self, n: &str, inner: Option<bool>) {
        if inner.is_some() {
            self.names.insert(Rc::from(n));
        }
    }

    fn pseudo(&mut self, n: &str, inner: Option<bool>) {
        if inner == Some(true) {
            self.names.insert(Rc::from(n));
        }
    }

    fn func(&mut self, f: &FuncDef, inner: Option<bool>) {
        let inner = Some(f.is_arrow && inner != Some(false));
        for p in &f.params {
            self.pattern(&p.pattern, inner);
            if let Some(d) = &p.default {
                self.expr(d, inner);
            }
        }
        if let Some(r) = &f.rest {
            self.pattern(r, inner);
        }
        match &f.body {
            FuncBody::Block(b) => {
                for s in b {
                    self.stmt(s, inner);
                }
            }
            FuncBody::Expr(e) => self.expr(e, inner),
        }
        if let Some(n) = &f.name {
            self.name(n, inner);
        }
    }

    fn class(&mut self, c: &ClassDef, inner: Option<bool>) {
        if let Some(n) = &c.name {
            self.name(n, inner);
        }
        if let Some(s) = &c.superclass {
            self.expr(s, inner);
        }
        if let Some(f) = &c.constructor {
            self.func(f, inner.or(Some(false)));
        }
        for m in &c.members {
            match m {
                ClassMember::Method { key, func, .. } => {
                    self.key(key, inner);
                    self.func(func, inner.or(Some(false)));
                }
                ClassMember::Field { key, value, .. } => {
                    self.key(key, inner);
                    if let Some(v) = value {
                        self.expr(v, Some(false));
                    }
                }
                ClassMember::StaticBlock(b) => {
                    for s in b {
                        self.stmt(s, Some(false));
                    }
                }
            }
        }
    }

    fn key(&mut self, k: &Key, inner: Option<bool>) {
        if let Key::Computed(e) = k {
            self.expr(e, inner);
        }
    }

    fn pattern(&mut self, p: &Pattern, inner: Option<bool>) {
        match p {
            Pattern::Ident(n) => self.name(n, inner),
            Pattern::Object { props, rest } => {
                for (k, sub, d) in props {
                    self.key(k, inner);
                    self.pattern(sub, inner);
                    if let Some(d) = d {
                        self.expr(d, inner);
                    }
                }
                if let Some(r) = rest {
                    self.pattern(r, inner);
                }
            }
            Pattern::Array { elems, rest } => {
                for (sub, d) in elems.iter().flatten() {
                    self.pattern(sub, inner);
                    if let Some(d) = d {
                        self.expr(d, inner);
                    }
                }
                if let Some(r) = rest {
                    self.pattern(r, inner);
                }
            }
            Pattern::Expr(e) => self.expr(e, inner),
        }
    }

    fn stmt(&mut self, s: &Stmt, inner: Option<bool>) {
        match &s.kind {
            StmtKind::Expr(e) | StmtKind::Throw(e) => self.expr(e, inner),
            StmtKind::Decl(_, ds) => {
                for (p, i) in ds {
                    self.pattern(p, inner);
                    if let Some(i) = i {
                        self.expr(i, inner);
                    }
                }
            }
            StmtKind::Function(f) => self.func(f, inner.or(Some(false)).map(|_| false).or(Some(false))),
            StmtKind::Class(c) => self.class(c, inner),
            StmtKind::Return(e) => {
                if let Some(e) = e {
                    self.expr(e, inner);
                }
            }
            StmtKind::If(t, a, b) => {
                self.expr(t, inner);
                self.stmt(a, inner);
                if let Some(b) = b {
                    self.stmt(b, inner);
                }
            }
            StmtKind::Block(b) => {
                for s in &b.body {
                    self.stmt(s, inner);
                }
            }
            StmtKind::For { init, test, update, body, .. } => {
                match init {
                    Some(ForInit::Decl(_, ds)) => {
                        for (p, i) in ds {
                            self.pattern(p, inner);
                            if let Some(i) = i {
                                self.expr(i, inner);
                            }
                        }
                    }
                    Some(ForInit::Expr(e)) => self.expr(e, inner),
                    None => {}
                }
                if let Some(t) = test {
                    self.expr(t, inner);
                }
                if let Some(u) = update {
                    self.expr(u, inner);
                }
                self.stmt(body, inner);
            }
            StmtKind::ForIn { target, obj: e, body } | StmtKind::ForOf { target, iter: e, body, .. } => {
                match target {
                    ForTarget::Decl(_, p) | ForTarget::Pattern(p) => self.pattern(p, inner),
                }
                self.expr(e, inner);
                self.stmt(body, inner);
            }
            StmtKind::While(t, b) | StmtKind::DoWhile(b, t) => {
                self.expr(t, inner);
                self.stmt(b, inner);
            }
            StmtKind::Try { block, param, handler, finalizer } => {
                for s in &block.body {
                    self.stmt(s, inner);
                }
                if let Some(p) = param {
                    self.pattern(p, inner);
                }
                if let Some(h) = handler {
                    for s in &h.body {
                        self.stmt(s, inner);
                    }
                }
                if let Some(f) = finalizer {
                    for s in &f.body {
                        self.stmt(s, inner);
                    }
                }
            }
            StmtKind::Switch(d, cases, _) => {
                self.expr(d, inner);
                for c in cases {
                    if let Some(t) = &c.test {
                        self.expr(t, inner);
                    }
                    for s in &c.body {
                        self.stmt(s, inner);
                    }
                }
            }
            StmtKind::Labeled(_, b) => self.stmt(b, inner),
            StmtKind::Break(_) | StmtKind::Continue(_) | StmtKind::Empty | StmtKind::Debugger => {}
        }
    }

    fn expr(&mut self, e: &Expr, inner: Option<bool>) {
        match e {
            Expr::Ident(n) => self.name(n, inner),
            Expr::This => self.pseudo(P_THIS, inner),
            Expr::NewTarget => self.pseudo(P_NT, inner),
            Expr::Super => {
                self.pseudo(P_HOME, inner);
                self.pseudo(P_THIS, inner);
                self.pseudo(P_FUNC, inner);
                self.pseudo(P_NT, inner);
            }
            Expr::Num(_) | Expr::Str(_) | Expr::BigInt(_) | Expr::Regex { .. } | Expr::Null | Expr::Bool(_) => {}
            Expr::Template { exprs, .. } => exprs.iter().for_each(|x| self.expr(x, inner)),
            Expr::TaggedTemplate { tag, exprs, .. } => {
                self.expr(tag, inner);
                exprs.iter().for_each(|x| self.expr(x, inner));
            }
            Expr::Array(items) => items.iter().for_each(|a| self.elem(a, inner)),
            Expr::Object(props) => {
                for p in props {
                    match p {
                        ObjProp::KeyValue(k, v) => {
                            self.key(k, inner);
                            self.expr(v, inner);
                        }
                        ObjProp::Shorthand(n) => self.name(n, inner),
                        ObjProp::Method(k, _, f) => {
                            self.key(k, inner);
                            self.func(f, inner.or(Some(false)).map(|_| false).or(Some(false)));
                        }
                        ObjProp::Spread(e) => self.expr(e, inner),
                    }
                }
            }
            Expr::Function(f) => {
                // a nested function: everything it names is captured
                let i = if f.is_arrow { Some(inner != Some(false)) } else { Some(false) };
                self.func(f, i);
            }
            Expr::Class(c) => self.class(c, inner),
            Expr::Unary(_, a) | Expr::Await(a) | Expr::OptChain(a) => self.expr(a, inner),
            Expr::Update { target, .. } => self.expr(target, inner),
            Expr::Binary(_, a, b) | Expr::Logical(_, a, b) => {
                self.expr(a, inner);
                self.expr(b, inner);
            }
            Expr::Assign(_, p, v) => {
                self.pattern(p, inner);
                self.expr(v, inner);
            }
            Expr::Cond(a, b, c) => {
                self.expr(a, inner);
                self.expr(b, inner);
                self.expr(c, inner);
            }
            Expr::Call { callee, args, .. } | Expr::New { callee, args } => {
                self.expr(callee, inner);
                args.iter().for_each(|a| self.elem(a, inner));
            }
            Expr::Member { obj, prop, .. } => {
                self.expr(obj, inner);
                self.key(prop, inner);
            }
            Expr::Seq(v) => v.iter().for_each(|x| self.expr(x, inner)),
            Expr::Yield { arg, .. } => {
                if let Some(a) = arg {
                    self.expr(a, inner);
                }
            }
            Expr::PrivateIn(_, o) => self.expr(o, inner),
        }
    }

    fn elem(&mut self, a: &ArrayElem, inner: Option<bool>) {
        match a {
            ArrayElem::Item(e) | ArrayElem::Spread(e) => self.expr(e, inner),
            ArrayElem::Hole => {}
        }
    }
}

// ---------------------------------------------------------------- compiler state

struct Var {
    name: Name,
    loc: Loc,
    konst: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// Top level of a classic script: its declarations are globals.
    Script,
    Function,
    Arrow,
}

struct FnCtx {
    kind: Kind,
    scopes: Vec<Vec<Var>>,
    nslots: usize,
    ncells: usize,
    caps: Vec<(Name, CapSrc)>,
    boxed: HashSet<Name>,
    /// Cells declared by each open scope (fresh ones are made on every entry).
    scope_cells: Vec<Vec<usize>>,
}

impl FnCtx {
    fn new(kind: Kind, boxed: HashSet<Name>) -> FnCtx {
        FnCtx { kind, scopes: vec![vec![]], nslots: 0, ncells: 0, caps: vec![], boxed, scope_cells: vec![vec![]] }
    }
}

pub struct Compiler {
    fns: Vec<FnCtx>,
    /// Whether compiled code runs as a module (top-level `this` is undefined).
    module: bool,
}

/// Names declared by a pattern.
fn pattern_names(p: &Pattern, out: &mut Vec<Name>) {
    match p {
        Pattern::Ident(n) => out.push(n.clone()),
        Pattern::Object { props, rest } => {
            for (_, sub, _) in props {
                pattern_names(sub, out);
            }
            if let Some(r) = rest {
                pattern_names(r, out);
            }
        }
        Pattern::Array { elems, rest } => {
            for (sub, _) in elems.iter().flatten() {
                pattern_names(sub, out);
            }
            if let Some(r) = rest {
                pattern_names(r, out);
            }
        }
        Pattern::Expr(_) => {}
    }
}

/// Lexical declarations at the top of a statement list: (name, is_const).
fn lexical_names(stmts: &[Stmt]) -> Vec<(Name, bool)> {
    let mut out = Vec::new();
    for s in stmts {
        match &s.kind {
            StmtKind::Decl(DeclKind::Let | DeclKind::Const, ds) => {
                let konst = matches!(&s.kind, StmtKind::Decl(DeclKind::Const, _));
                for (p, _) in ds {
                    let mut ns = Vec::new();
                    pattern_names(p, &mut ns);
                    out.extend(ns.into_iter().map(|n| (n, konst)));
                }
            }
            StmtKind::Class(c) => {
                if let Some(n) = &c.name {
                    out.push((n.clone(), false));
                }
            }
            StmtKind::Function(f) => {
                if let Some(n) = &f.name {
                    out.push((n.clone(), false));
                }
            }
            _ => {}
        }
    }
    out
}

macro_rules! ecode {
    (|$it:ident, $f:ident| $body:expr) => {
        Rc::new(move |$it: &mut Interp, $f: &mut Frame| -> R<Value> { $body }) as ECode
    };
}

macro_rules! scode {
    (|$it:ident, $f:ident| $body:expr) => {
        Rc::new(move |$it: &mut Interp, $f: &mut Frame| -> R<Flow> { $body }) as SCode
    };
}

// ---------------------------------------------------------------- variable access

fn read_loc(it: &mut Interp, f: &mut Frame, loc: &Loc) -> R<Value> {
    Ok(match loc {
        Loc::Slot(i) => f.slots[*i].clone(),
        Loc::Cell(i) => f.cells[*i].borrow().clone(),
        Loc::Cap(i) => f.caps[*i].borrow().clone(),
        Loc::Env(n) => {
            let env = f.env.clone();
            return it.lookup(n, &env);
        }
        Loc::This => f.this.clone(),
        Loc::NewTarget => f.new_target.clone(),
        Loc::FuncObj => f.func.clone().map(Value::Obj).unwrap_or(Value::Undefined),
        Loc::Home => f.home.clone().map(Value::Obj).unwrap_or(Value::Undefined),
    })
}

fn write_loc(it: &mut Interp, f: &mut Frame, loc: &Loc, v: Value) -> R<()> {
    match loc {
        Loc::Slot(i) => f.slots[*i] = v,
        Loc::Cell(i) => *f.cells[*i].borrow_mut() = v,
        Loc::Cap(i) => *f.caps[*i].borrow_mut() = v,
        Loc::Env(n) => {
            let env = f.env.clone();
            return it.assign_var(n, v, &env);
        }
        Loc::This => f.this = v,
        Loc::NewTarget | Loc::FuncObj | Loc::Home => {}
    }
    Ok(())
}

impl Compiler {
    pub fn new(module: bool) -> Compiler {
        Compiler { fns: vec![], module }
    }

    fn ctx(&mut self) -> &mut FnCtx {
        self.fns.last_mut().unwrap()
    }

    /// Declare `name` in the innermost scope of the current function.
    fn declare(&mut self, name: &Name, konst: bool) -> Loc {
        let script_top = {
            let c = self.ctx();
            c.kind == Kind::Script && c.scopes.len() == 1
        };
        let loc = if script_top {
            Loc::Env(name.clone())
        } else {
            let c = self.ctx();
            if c.boxed.contains(name) {
                c.ncells += 1;
                let i = c.ncells - 1;
                c.scope_cells.last_mut().unwrap().push(i);
                Loc::Cell(i)
            } else {
                c.nslots += 1;
                Loc::Slot(c.nslots - 1)
            }
        };
        let c = self.ctx();
        let scope = c.scopes.last_mut().unwrap();
        match scope.iter_mut().find(|v| &v.name == name) {
            Some(v) => {
                v.konst = konst;
                return v.loc.clone();
            }
            None => scope.push(Var { name: name.clone(), loc: loc.clone(), konst }),
        }
        loc
    }

    /// Bind a name to a fixed location (pseudo variables).
    fn declare_at(&mut self, name: &str, loc: Loc) {
        let c = self.ctx();
        c.scopes.last_mut().unwrap().push(Var { name: Rc::from(name), loc, konst: true });
    }

    fn push_scope(&mut self) {
        let c = self.ctx();
        c.scopes.push(vec![]);
        c.scope_cells.push(vec![]);
    }

    /// Pop a scope; returns the cells it declared (to be made fresh on entry).
    fn pop_scope(&mut self) -> Vec<usize> {
        let c = self.ctx();
        c.scopes.pop();
        c.scope_cells.pop().unwrap_or_default()
    }

    /// Resolve `name` as seen from function `depth`, capturing it from enclosing functions.
    fn resolve_in(&mut self, depth: usize, name: &str) -> CResult<Option<(Loc, bool)>> {
        {
            let c = &self.fns[depth];
            for scope in c.scopes.iter().rev() {
                if let Some(v) = scope.iter().rev().find(|v| &*v.name == name) {
                    return Ok(Some((v.loc.clone(), v.konst)));
                }
            }
            if let Some(i) = c.caps.iter().position(|(n, _)| &**n == name) {
                return Ok(Some((Loc::Cap(i), false)));
            }
            if depth == 0 {
                return Ok(None);
            }
        }
        let Some((outer, konst)) = self.resolve_in(depth - 1, name)? else { return Ok(None) };
        let src = match outer {
            Loc::Cell(i) => CapSrc::Cell(i),
            Loc::Cap(i) => CapSrc::Cap(i),
            Loc::Env(n) => return Ok(Some((Loc::Env(n), konst))),
            other => return Err(Unsupported(format!("capture of unboxed {name} ({other:?})"))),
        };
        let c = &mut self.fns[depth];
        c.caps.push((Rc::from(name), src));
        Ok(Some((Loc::Cap(c.caps.len() - 1), konst)))
    }

    fn resolve(&mut self, name: &str) -> CResult<(Loc, bool)> {
        let d = self.fns.len() - 1;
        Ok(self.resolve_in(d, name)?.unwrap_or_else(|| (Loc::Env(Rc::from(name)), false)))
    }

    /// Where `this` / `new.target` / ... live, or `None` at the top level of a script.
    fn resolve_pseudo(&mut self, name: &str) -> CResult<Option<Loc>> {
        let d = self.fns.len() - 1;
        Ok(match self.resolve_in(d, name)? {
            Some((Loc::Env(_), _)) | None => None,
            Some((l, _)) => Some(l),
        })
    }
}

// ---------------------------------------------------------------- runtime helpers

fn is_short(it: &Interp, v: &Value) -> bool {
    matches!((v, &it.short_circuit()), (Value::Obj(a), Value::Obj(b)) if a.ptr_eq(b))
}

/// The `arguments` object (an array-like with the array iterator).
fn make_arguments(it: &mut Interp, args: &[Value]) -> R<Value> {
    let ao = Obj::new(ObjKind::Array(args.to_vec()), Some(it.p.object.clone()), "Arguments");
    let iter = it.get_from(&it.p.array.clone(), &PropKey::Sym(it.sym_iterator.clone()), &Value::Undefined)?;
    ao.borrow_mut().props.insert(PropKey::Sym(it.sym_iterator.clone()), Prop::hidden(iter));
    Ok(Value::Obj(ao))
}

/// Create a compiled function object, capturing its variables from the frame `f`.
fn make_fn(it: &mut Interp, f: &Frame, def: &Rc<FuncDef>, cf: &Rc<CFunc>, home: Option<Obj>) -> Value {
    let caps: Rc<[Cell]> = cf
        .caps
        .iter()
        .map(|s| match s {
            CapSrc::Cell(i) => f.cells[*i].clone(),
            CapSrc::Cap(i) => f.caps[*i].clone(),
        })
        .collect();
    it.make_compiled_function(def.clone(), cf.clone(), caps, f.env.clone(), home)
}

enum KeyCode {
    Static(PropKey),
    Dynamic(ECode),
}

impl KeyCode {
    fn get(&self, it: &mut Interp, f: &mut Frame) -> R<PropKey> {
        match self {
            KeyCode::Static(k) => Ok(k.clone()),
            KeyCode::Dynamic(c) => {
                let v = c(it, f)?;
                it.to_key(&v)
            }
        }
    }
}

type CalleeCode = Rc<dyn Fn(&mut Interp, &mut Frame) -> R<(Value, Value)>>;
type ArgsCode = Rc<dyn Fn(&mut Interp, &mut Frame) -> R<Vec<Value>>>;

/// The operation on two numbers, for operators that have a purely numeric fast path.
fn num_op(op: BinOp) -> Option<fn(f64, f64) -> f64> {
    use super::interp::{js_mod, js_pow};
    Some(match op {
        BinOp::Add => |x, y| x + y,
        BinOp::Sub => |x, y| x - y,
        BinOp::Mul => |x, y| x * y,
        BinOp::Div => |x, y| x / y,
        BinOp::Mod => js_mod,
        BinOp::Exp => js_pow,
        BinOp::BitAnd => |x, y| (to_int32(x) & to_int32(y)) as f64,
        BinOp::BitOr => |x, y| (to_int32(x) | to_int32(y)) as f64,
        BinOp::BitXor => |x, y| (to_int32(x) ^ to_int32(y)) as f64,
        BinOp::Shl => |x, y| to_int32(x).wrapping_shl(to_uint32(y) & 31) as f64,
        BinOp::Shr => |x, y| (to_int32(x) >> (to_uint32(y) & 31)) as f64,
        BinOp::UShr => |x, y| (to_uint32(x) >> (to_uint32(y) & 31)) as f64,
        _ => return None,
    })
}

/// Store a number in a slot, updating it in place when it already holds one.
#[inline]
fn store_num(slot: &mut Value, n: f64) {
    match slot {
        Value::Num(x) => *x = n,
        other => *other = Value::Num(n),
    }
}

/// An operand of a numeric operator: locals and literals are read directly.
enum Opnd {
    Slot(usize),
    Num(f64),
    Code(ECode),
}

impl Opnd {
    #[inline(always)]
    fn get(&self, it: &mut Interp, f: &mut Frame) -> R<Value> {
        match self {
            Opnd::Slot(i) => Ok(f.slots[*i].clone()),
            Opnd::Num(n) => Ok(Value::Num(*n)),
            Opnd::Code(c) => c(it, f),
        }
    }
}

/// Numeric fast paths for binary operators, falling back to the full semantics. Operands that
/// are locals or literals get closures that read them without a call.
macro_rules! numop {
    ($lo:expr, $ro:expr, $op:expr, |$x:ident, $y:ident| $fast:expr) => {{
        let op = $op;
        match ($lo, $ro) {
            (Opnd::Slot(a), Opnd::Num($y)) => ecode!(|it, f| {
                if let Value::Num($x) = f.slots[a] {
                    return Ok($fast);
                }
                let av = f.slots[a].clone();
                it.binary(op, &av, &Value::Num($y))
            }),
            (Opnd::Slot(a), Opnd::Slot(b)) => ecode!(|it, f| {
                if let (Value::Num($x), Value::Num($y)) = (&f.slots[a], &f.slots[b]) {
                    let ($x, $y) = (*$x, *$y);
                    return Ok($fast);
                }
                let (av, bv) = (f.slots[a].clone(), f.slots[b].clone());
                it.binary(op, &av, &bv)
            }),
            (Opnd::Code(l), Opnd::Num($y)) => ecode!(|it, f| {
                let av = l(it, f)?;
                if let Value::Num($x) = av {
                    return Ok($fast);
                }
                it.binary(op, &av, &Value::Num($y))
            }),
            (Opnd::Code(l), Opnd::Slot(b)) => ecode!(|it, f| {
                let av = l(it, f)?;
                if let (Value::Num($x), Value::Num($y)) = (&av, &f.slots[b]) {
                    let ($x, $y) = (*$x, *$y);
                    return Ok($fast);
                }
                let bv = f.slots[b].clone();
                it.binary(op, &av, &bv)
            }),
            (Opnd::Slot(a), Opnd::Code(r)) => ecode!(|it, f| {
                // the left operand is read before the right one runs
                let av = f.slots[a].clone();
                let bv = r(it, f)?;
                if let (Value::Num($x), Value::Num($y)) = (&av, &bv) {
                    let ($x, $y) = (*$x, *$y);
                    return Ok($fast);
                }
                it.binary(op, &av, &bv)
            }),
            (lo, ro) => ecode!(|it, f| {
                let av = lo.get(it, f)?;
                let bv = ro.get(it, f)?;
                if let (Value::Num($x), Value::Num($y)) = (&av, &bv) {
                    let ($x, $y) = (*$x, *$y);
                    return Ok($fast);
                }
                it.binary(op, &av, &bv)
            }),
        }
    }};
}

impl Compiler {
    // ------------------------------------------------------------ scripts & functions

    /// Compile a classic script's top level.
    pub fn compile_script(stmts: &[Stmt], module: bool) -> CResult<Rc<CFunc>> {
        let mut c = Compiler::new(module);
        let boxed = Captured::of_body(stmts, &[], None);
        c.fns.push(FnCtx::new(Kind::Script, boxed));
        // `this` at the top level (arrows may capture it)
        let this_loc = if c.ctx().boxed.contains(P_THIS) {
            c.ctx().ncells += 1;
            Loc::Cell(c.ctx().ncells - 1)
        } else {
            Loc::This
        };
        c.declare_at(P_THIS, this_loc.clone());
        let lex = lexical_names(stmts);
        for (n, k) in &lex {
            c.declare(n, *k);
        }
        let hoist = c.hoist_functions(stmts)?;
        let body = c.stmt_list(stmts, true)?;
        let ctx = c.fns.pop().unwrap();
        let tl = this_loc.clone();
        let prologue: Rc<dyn Fn(&mut Interp, &mut Frame) -> R<()>> = Rc::new(move |it, f| {
            if let Loc::Cell(i) = tl {
                *f.cells[i].borrow_mut() = f.this.clone();
            }
            hoist(it, f)
        });
        Ok(Rc::new(CFunc { nslots: ctx.nslots, ncells: ctx.ncells, caps: vec![], prologue, body, this_loc }))
    }

    /// Compile a function. `self_name` binds a named function expression's own name.
    fn function(&mut self, def: &Rc<FuncDef>, self_name: Option<&Name>) -> CResult<Rc<CFunc>> {
        let boxed = match &def.body {
            FuncBody::Block(b) => Captured::of_body(b, &def.params, def.rest.as_ref()),
            FuncBody::Expr(e) => {
                let s = [Stmt { kind: StmtKind::Expr(e.clone()), line: def.line }];
                Captured::of_body(&s, &def.params, def.rest.as_ref())
            }
        };
        let kind = if def.is_arrow { Kind::Arrow } else { Kind::Function };
        self.fns.push(FnCtx::new(kind, boxed));
        let r = self.function_inner(def, self_name);
        let ctx = self.fns.pop().unwrap();
        let (prologue, body, this_loc) = r?;
        Ok(Rc::new(CFunc { nslots: ctx.nslots, ncells: ctx.ncells, caps: ctx.caps.iter().map(|c| c.1).collect(), prologue, body, this_loc }))
    }

    #[allow(clippy::type_complexity)]
    fn function_inner(&mut self, def: &Rc<FuncDef>, self_name: Option<&Name>) -> CResult<(Rc<dyn Fn(&mut Interp, &mut Frame) -> R<()>>, SCode, Loc)> {
        let arrow = def.is_arrow;
        // pseudo variables of non-arrow functions; boxed ones are copied into cells on entry
        let mut pseudo_cells: Vec<(usize, Loc)> = Vec::new();
        let mut this_loc = Loc::This;
        if !arrow {
            for (name, base) in [(P_THIS, Loc::This), (P_NT, Loc::NewTarget), (P_FUNC, Loc::FuncObj), (P_HOME, Loc::Home)] {
                let loc = if self.ctx().boxed.contains(name) {
                    self.ctx().ncells += 1;
                    let i = self.ctx().ncells - 1;
                    pseudo_cells.push((i, base));
                    Loc::Cell(i)
                } else {
                    base
                };
                if name == P_THIS {
                    this_loc = loc.clone();
                }
                self.declare_at(name, loc);
            }
            if let Some(n) = self_name {
                // a named function expression sees itself (in a cell if an inner function does)
                let loc = if self.ctx().boxed.contains(n) {
                    self.ctx().ncells += 1;
                    let i = self.ctx().ncells - 1;
                    pseudo_cells.push((i, Loc::FuncObj));
                    Loc::Cell(i)
                } else {
                    Loc::FuncObj
                };
                self.declare_at(n, loc);
            }
        }
        // parameters and the body get their own scope, so they can shadow the self-name
        self.push_scope();
        // parameters
        let mut param_init: Vec<Rc<dyn Fn(&mut Interp, &mut Frame) -> R<()>>> = Vec::new();
        if def.uses_arguments && !arrow {
            let loc = self.declare(&Rc::from("arguments"), false);
            param_init.push(Rc::new(move |it, f| {
                let a = make_arguments(it, &f.args)?;
                write_loc(it, f, &loc, a)
            }));
        }
        for (i, p) in def.params.iter().enumerate() {
            let mut names = Vec::new();
            pattern_names(&p.pattern, &mut names);
            for n in &names {
                self.declare(n, false);
            }
            let default = match &p.default {
                Some(d) => Some(self.expr(d)?),
                None => None,
            };
            match (&p.pattern, default) {
                (Pattern::Ident(n), None) => {
                    let (loc, _) = self.resolve(n)?;
                    param_init.push(Rc::new(move |it, f| {
                        let v = f.args.get(i).cloned().unwrap_or(Value::Undefined);
                        write_loc(it, f, &loc, v)
                    }));
                }
                (pat, default) => {
                    let b = self.binder(pat, Some(DeclKind::Let))?;
                    param_init.push(Rc::new(move |it, f| {
                        let mut v = f.args.get(i).cloned().unwrap_or(Value::Undefined);
                        if v.is_undefined() {
                            if let Some(d) = &default {
                                v = d(it, f)?;
                            }
                        }
                        b(it, f, v)
                    }));
                }
            }
        }
        if let Some(r) = &def.rest {
            let mut names = Vec::new();
            pattern_names(r, &mut names);
            for n in &names {
                self.declare(n, false);
            }
            let b = self.binder(r, Some(DeclKind::Let))?;
            let n = def.params.len();
            param_init.push(Rc::new(move |it, f| {
                let rest = if f.args.len() > n { f.args[n..].to_vec() } else { vec![] };
                let arr = it.new_array(rest);
                b(it, f, arr)
            }));
        }
        for n in &def.var_names {
            let known = self.ctx().scopes.last().unwrap().iter().any(|v| &v.name == n);
            if !known {
                self.declare(n, false);
            }
        }
        let (hoist, body): (Rc<dyn Fn(&mut Interp, &mut Frame) -> R<()>>, SCode) = match &def.body {
            FuncBody::Block(stmts) => {
                for (n, k) in lexical_names(stmts) {
                    self.declare(&n, k);
                }
                let hoist = self.hoist_functions(stmts)?;
                (hoist, self.stmt_list(stmts, false)?)
            }
            FuncBody::Expr(e) => {
                let c = self.expr(e)?;
                (Rc::new(|_, _| Ok(())), scode!(|it, f| Ok(Flow::Return(c(it, f)?))))
            }
        };
        let param_init: Rc<[Rc<dyn Fn(&mut Interp, &mut Frame) -> R<()>>]> = param_init.into();
        let prologue: Rc<dyn Fn(&mut Interp, &mut Frame) -> R<()>> = Rc::new(move |it, f| {
            for (i, base) in pseudo_cells.iter() {
                let v = read_loc(it, f, base)?;
                *f.cells[*i].borrow_mut() = v;
            }
            for p in param_init.iter() {
                p(it, f)?;
            }
            hoist(it, f)
        });
        Ok((prologue, body, this_loc))
    }

    /// Function declarations at the top of a statement list, created on entry.
    fn hoist_functions(&mut self, stmts: &[Stmt]) -> CResult<Rc<dyn Fn(&mut Interp, &mut Frame) -> R<()>>> {
        let mut items: Vec<(Loc, Rc<FuncDef>, Rc<CFunc>)> = Vec::new();
        for s in stmts {
            if let StmtKind::Function(def) = &s.kind {
                let Some(n) = &def.name else { continue };
                let (loc, _) = self.resolve(n)?;
                let cf = self.function(def, None)?;
                items.push((loc, def.clone(), cf));
            }
        }
        if items.is_empty() {
            return Ok(Rc::new(|_, _| Ok(())));
        }
        Ok(Rc::new(move |it, f| {
            for (loc, def, cf) in &items {
                let v = make_fn(it, f, def, cf, None);
                write_loc(it, f, loc, v)?;
            }
            Ok(())
        }))
    }

    // ------------------------------------------------------------ statements

    /// A statement list in the current scope (declarations already made by the caller).
    fn stmt_list(&mut self, stmts: &[Stmt], top: bool) -> CResult<SCode> {
        let codes: Vec<(u32, SCode)> = stmts.iter().map(|s| Ok((s.line, self.stmt_inner(s, top, None)?))).collect::<CResult<_>>()?;
        Ok(match codes.len() {
            0 => scode!(|_it, _f| Ok(Flow::Normal)),
            1 => {
                let (line, c) = codes.into_iter().next().unwrap();
                scode!(|it, f| {
                    it.cur_line = line;
                    c(it, f)
                })
            }
            _ => {
                let codes: Rc<[(u32, SCode)]> = codes.into();
                scode!(|it, f| {
                    for (line, c) in codes.iter() {
                        it.cur_line = *line;
                        match c(it, f)? {
                            Flow::Normal => {}
                            other => return Ok(other),
                        }
                    }
                    Ok(Flow::Normal)
                })
            }
        })
    }

    /// A block with its own scope: lexical declarations, fresh cells and hoisted functions.
    fn block(&mut self, stmts: &[Stmt], scoped: bool, top: bool) -> CResult<SCode> {
        if !scoped {
            return self.stmt_list(stmts, top);
        }
        self.push_scope();
        let r = (|| {
            for (n, k) in lexical_names(stmts) {
                self.declare(&n, k);
            }
            let hoist = self.hoist_functions(stmts)?;
            let body = self.stmt_list(stmts, top)?;
            Ok((hoist, body))
        })();
        let fresh = self.pop_scope();
        let (hoist, body) = r?;
        Ok(scode!(|it, f| {
            for &i in &fresh {
                f.cells[i] = cell(Value::Undefined);
            }
            hoist(it, f)?;
            body(it, f)
        }))
    }

    fn stmt(&mut self, s: &Stmt, top: bool) -> CResult<SCode> {
        let line = s.line;
        let code = self.stmt_inner(s, top, None)?;
        Ok(scode!(|it, f| {
            it.cur_line = line;
            code(it, f)
        }))
    }

    fn stmt_inner(&mut self, s: &Stmt, top: bool, label: Option<&Name>) -> CResult<SCode> {
        Ok(match &s.kind {
            StmtKind::Expr(e) => {
                let c = self.expr(e)?;
                if top {
                    scode!(|it, f| {
                        it.completion = c(it, f)?;
                        Ok(Flow::Normal)
                    })
                } else {
                    scode!(|it, f| {
                        c(it, f)?;
                        Ok(Flow::Normal)
                    })
                }
            }
            StmtKind::Decl(kind, decls) => {
                let mut parts: Vec<(Binder, Option<ECode>)> = Vec::new();
                for (p, init) in decls {
                    if *kind == DeclKind::Var {
                        // `var` names were declared at function (or global) level
                        let mut ns = Vec::new();
                        pattern_names(p, &mut ns);
                        for n in &ns {
                            if self.resolve_in(self.fns.len() - 1, n)?.is_none() && self.ctx().kind != Kind::Script {
                                self.declare(n, false);
                            }
                        }
                    }
                    let b = self.binder(p, Some(*kind))?;
                    let init = match init {
                        Some(e) => {
                            let c = self.expr(e)?;
                            Some(match (p, is_anon_fn(e)) {
                                (Pattern::Ident(n), true) => {
                                    let key = PropKey::Str(n.clone());
                                    ecode!(|it, f| {
                                        let v = c(it, f)?;
                                        it.maybe_name(&v, &key);
                                        Ok(v)
                                    })
                                }
                                _ => c,
                            })
                        }
                        None if *kind == DeclKind::Var => continue,
                        None => None,
                    };
                    parts.push((b, init));
                }
                scode!(|it, f| {
                    for (b, init) in &parts {
                        let v = match init {
                            Some(c) => c(it, f)?,
                            None => Value::Undefined,
                        };
                        b(it, f, v)?;
                    }
                    Ok(Flow::Normal)
                })
            }
            StmtKind::Function(_) => scode!(|_it, _f| Ok(Flow::Normal)),
            StmtKind::Class(c) => {
                let cc = self.class(c)?;
                let n = c.name.clone().ok_or_else(|| Unsupported("anonymous class declaration".into()))?;
                let (loc, _) = self.resolve(&n)?;
                let decl = matches!(loc, Loc::Env(_));
                scode!(|it, f| {
                    let v = cc(it, f)?;
                    if decl {
                        f.env.declare(n.clone(), v, true);
                    } else {
                        write_loc(it, f, &loc, v)?;
                    }
                    Ok(Flow::Normal)
                })
            }
            StmtKind::Return(e) => match e {
                Some(e) => {
                    let c = self.expr(e)?;
                    scode!(|it, f| Ok(Flow::Return(c(it, f)?)))
                }
                None => scode!(|_it, _f| Ok(Flow::Return(Value::Undefined))),
            },
            StmtKind::If(t, a, b) => {
                let t = self.cond(t)?;
                let a = self.stmt(a, top)?;
                match b {
                    Some(b) => {
                        let b = self.stmt(b, top)?;
                        scode!(|it, f| if t(it, f)? { a(it, f) } else { b(it, f) })
                    }
                    None => scode!(|it, f| if t(it, f)? { a(it, f) } else { Ok(Flow::Normal) }),
                }
            }
            StmtKind::Block(b) => self.block(&b.body, b.scoped, top)?,
            StmtKind::Empty | StmtKind::Debugger => scode!(|_it, _f| Ok(Flow::Normal)),
            StmtKind::Throw(e) => {
                let c = self.expr(e)?;
                scode!(|it, f| Err(c(it, f)?))
            }
            StmtKind::Break(l) => {
                let l = l.clone();
                scode!(|_it, _f| Ok(Flow::Break(l.clone())))
            }
            StmtKind::Continue(l) => {
                let l = l.clone();
                scode!(|_it, _f| Ok(Flow::Continue(l.clone())))
            }
            StmtKind::Labeled(name, body) => {
                let is_loop = matches!(body.kind, StmtKind::While(..) | StmtKind::DoWhile(..) | StmtKind::For { .. } | StmtKind::ForIn { .. } | StmtKind::ForOf { .. });
                let inner = if is_loop { self.stmt_inner(body, top, Some(name))? } else { self.stmt(body, top)? };
                let name = name.clone();
                scode!(|it, f| match inner(it, f)? {
                    Flow::Break(Some(l)) if l == name => Ok(Flow::Normal),
                    other => Ok(other),
                })
            }
            StmtKind::While(..) | StmtKind::DoWhile(..) | StmtKind::For { .. } | StmtKind::ForIn { .. } | StmtKind::ForOf { .. } => self.loop_stmt(s, top, label)?,
            StmtKind::Try { block, param, handler, finalizer } => {
                let b = self.block(&block.body, block.scoped, top)?;
                let h = match handler {
                    Some(h) => {
                        self.push_scope();
                        let r = (|| {
                            let pb = match param {
                                Some(p) => {
                                    let mut ns = Vec::new();
                                    pattern_names(p, &mut ns);
                                    for n in &ns {
                                        self.declare(n, false);
                                    }
                                    Some(self.binder(p, Some(DeclKind::Let))?)
                                }
                                None => None,
                            };
                            for (n, k) in lexical_names(&h.body) {
                                self.declare(&n, k);
                            }
                            let hoist = self.hoist_functions(&h.body)?;
                            let body = self.stmt_list(&h.body, top)?;
                            Ok((pb, hoist, body))
                        })();
                        let fresh = self.pop_scope();
                        let (pb, hoist, body) = r?;
                        Some((fresh, pb, hoist, body))
                    }
                    None => None,
                };
                let fin = match finalizer {
                    Some(fb) => Some(self.block(&fb.body, fb.scoped, top)?),
                    None => None,
                };
                scode!(|it, f| {
                    let mut r = b(it, f);
                    if let (Err(e), Some((fresh, pb, hoist, body))) = (&r, &h) {
                        if !it.is_marker(e) {
                            let e = e.clone();
                            r = (|| {
                                for &i in fresh {
                                    f.cells[i] = cell(Value::Undefined);
                                }
                                if let Some(pb) = pb {
                                    pb(it, f, e)?;
                                }
                                hoist(it, f)?;
                                body(it, f)
                            })();
                        }
                    }
                    match &fin {
                        Some(fc) => match fc(it, f)? {
                            Flow::Normal => r,
                            other => Ok(other),
                        },
                        None => r,
                    }
                })
            }
            StmtKind::Switch(disc, cases, scoped) => {
                let d = self.expr(disc)?;
                if *scoped {
                    self.push_scope();
                }
                let r = (|| {
                    let mut all: Vec<Stmt> = Vec::new();
                    if *scoped {
                        for c in cases {
                            all.extend(c.body.iter().cloned());
                        }
                        for (n, k) in lexical_names(&all) {
                            self.declare(&n, k);
                        }
                    }
                    let hoist = if *scoped { Some(self.hoist_functions(&all)?) } else { None };
                    let mut cs: Vec<(Option<ECode>, SCode)> = Vec::new();
                    for c in cases {
                        let t = match &c.test {
                            Some(t) => Some(self.expr(t)?),
                            None => None,
                        };
                        cs.push((t, self.stmt_list(&c.body, top)?));
                    }
                    Ok((hoist, cs))
                })();
                let fresh = if *scoped { self.pop_scope() } else { vec![] };
                let (hoist, cs) = r?;
                scode!(|it, f| {
                    let dv = d(it, f)?;
                    for &i in &fresh {
                        f.cells[i] = cell(Value::Undefined);
                    }
                    if let Some(h) = &hoist {
                        h(it, f)?;
                    }
                    let mut start = None;
                    for (i, (t, _)) in cs.iter().enumerate() {
                        if let Some(t) = t {
                            let tv = t(it, f)?;
                            if strict_equals(&dv, &tv) {
                                start = Some(i);
                                break;
                            }
                        }
                    }
                    if start.is_none() {
                        start = cs.iter().position(|(t, _)| t.is_none());
                    }
                    if let Some(s) = start {
                        for (_, body) in &cs[s..] {
                            match body(it, f)? {
                                Flow::Normal => {}
                                Flow::Break(None) => return Ok(Flow::Normal),
                                other => return Ok(other),
                            }
                        }
                    }
                    Ok(Flow::Normal)
                })
            }
        })
    }

    /// A condition, evaluated straight to a boolean.
    fn cond(&mut self, e: &Expr) -> CResult<Rc<dyn Fn(&mut Interp, &mut Frame) -> R<bool>>> {
        // comparisons of numbers are the common loop test: skip the boxed boolean
        if let Expr::Binary(op @ (BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge), l, r) = e {
            let (l, r, op) = (self.expr(l)?, self.expr(r)?, *op);
            return Ok(Rc::new(move |it, f| {
                let a = l(it, f)?;
                let b = r(it, f)?;
                if let (Value::Num(x), Value::Num(y)) = (&a, &b) {
                    return Ok(match op {
                        BinOp::Lt => x < y,
                        BinOp::Gt => x > y,
                        BinOp::Le => x <= y,
                        _ => x >= y,
                    });
                }
                Ok(it.binary(op, &a, &b)?.truthy())
            }));
        }
        let c = self.expr(e)?;
        Ok(Rc::new(move |it, f| Ok(c(it, f)?.truthy())))
    }
}

/// What a loop does after its body ran.
enum Next {
    Go,
    Stop,
    Exit(Flow),
}

fn loop_next(fl: Flow, label: &Option<Name>) -> Next {
    match fl {
        Flow::Normal | Flow::Continue(None) => Next::Go,
        Flow::Break(None) => Next::Stop,
        Flow::Break(Some(l)) => {
            if label.as_ref() == Some(&l) {
                Next::Stop
            } else {
                Next::Exit(Flow::Break(Some(l)))
            }
        }
        Flow::Continue(Some(l)) => {
            if label.as_ref() == Some(&l) {
                Next::Go
            } else {
                Next::Exit(Flow::Continue(Some(l)))
            }
        }
        Flow::Return(v) => Next::Exit(Flow::Return(v)),
    }
}

impl Compiler {
    fn loop_stmt(&mut self, s: &Stmt, top: bool, label: Option<&Name>) -> CResult<SCode> {
        let label: Option<Name> = label.cloned();
        Ok(match &s.kind {
            StmtKind::While(test, body) => {
                let t = self.cond(test)?;
                let b = self.stmt(body, top)?;
                scode!(|it, f| {
                    while t(it, f)? {
                        it.check_deadline()?;
                        match loop_next(b(it, f)?, &label) {
                            Next::Go => {}
                            Next::Stop => break,
                            Next::Exit(fl) => return Ok(fl),
                        }
                    }
                    Ok(Flow::Normal)
                })
            }
            StmtKind::DoWhile(body, test) => {
                let t = self.cond(test)?;
                let b = self.stmt(body, top)?;
                scode!(|it, f| {
                    loop {
                        it.check_deadline()?;
                        match loop_next(b(it, f)?, &label) {
                            Next::Go => {}
                            Next::Stop => break,
                            Next::Exit(fl) => return Ok(fl),
                        }
                        if !t(it, f)? {
                            break;
                        }
                    }
                    Ok(Flow::Normal)
                })
            }
            StmtKind::For { init, test, update, body, .. } => {
                self.push_scope();
                let r = (|| {
                    let init: Option<SCode> = match init {
                        Some(ForInit::Decl(kind, decls)) => {
                            let st = Stmt { kind: StmtKind::Decl(*kind, decls.clone()), line: s.line };
                            if *kind != DeclKind::Var {
                                for (n, k) in lexical_names(std::slice::from_ref(&st)) {
                                    self.declare(&n, k);
                                }
                            }
                            Some(self.stmt_inner(&st, false, None)?)
                        }
                        Some(ForInit::Expr(e)) => {
                            let c = self.expr(e)?;
                            Some(scode!(|it, f| {
                                c(it, f)?;
                                Ok(Flow::Normal)
                            }))
                        }
                        None => None,
                    };
                    let test = match test {
                        Some(t) => Some(self.cond(t)?),
                        None => None,
                    };
                    let update = match update {
                        Some(u) => Some(self.expr(u)?),
                        None => None,
                    };
                    let body = self.stmt(body, top)?;
                    Ok((init, test, update, body))
                })();
                // cells of `let` loop variables get a fresh copy per iteration (closures in the
                // body see that iteration's value)
                let fresh = self.pop_scope();
                let (init, test, update, body) = r?;
                scode!(|it, f| {
                    for &i in &fresh {
                        f.cells[i] = cell(Value::Undefined);
                    }
                    if let Some(i) = &init {
                        i(it, f)?;
                    }
                    loop {
                        if let Some(t) = &test {
                            if !t(it, f)? {
                                break;
                            }
                        }
                        it.check_deadline()?;
                        match loop_next(body(it, f)?, &label) {
                            Next::Go => {}
                            Next::Stop => break,
                            Next::Exit(fl) => return Ok(fl),
                        }
                        for &i in &fresh {
                            let v = f.cells[i].borrow().clone();
                            f.cells[i] = cell(v);
                        }
                        if let Some(u) = &update {
                            u(it, f)?;
                        }
                    }
                    Ok(Flow::Normal)
                })
            }
            StmtKind::ForIn { target, obj, body } => {
                let o = self.expr(obj)?;
                let (fresh, bind, body) = self.for_target(target, body, top)?;
                scode!(|it, f| {
                    let ov = o(it, f)?;
                    if ov.is_nullish() {
                        return Ok(Flow::Normal);
                    }
                    let o = it.to_object(&ov)?;
                    let mut keys: Vec<PropKey> = Vec::new();
                    let mut cur = Some(o.clone());
                    let mut seen = std::collections::HashSet::new();
                    while let Some(c) = cur {
                        for k in it.own_keys(&c, false) {
                            if seen.insert(k.clone()) {
                                keys.push(k);
                            }
                        }
                        for (k, _) in c.borrow().props.iter() {
                            seen.insert(k.clone());
                        }
                        cur = c.proto();
                        if cur.as_ref().is_some_and(|p| p.ptr_eq(&it.p.object)) {
                            break;
                        }
                    }
                    for k in keys {
                        if !it.has_property(&o, &k) {
                            continue;
                        }
                        it.check_deadline()?;
                        for &i in &fresh {
                            f.cells[i] = cell(Value::Undefined);
                        }
                        bind(it, f, k.to_value())?;
                        match loop_next(body(it, f)?, &label) {
                            Next::Go => {}
                            Next::Stop => break,
                            Next::Exit(fl) => return Ok(fl),
                        }
                    }
                    Ok(Flow::Normal)
                })
            }
            StmtKind::ForOf { target, iter, body, is_await } => {
                let src = self.expr(iter)?;
                let (fresh, bind, body) = self.for_target(target, body, top)?;
                let is_await = *is_await;
                scode!(|it, f| {
                    let v = src(it, f)?;
                    let mut iter = it.get_iterator(&v)?;
                    loop {
                        it.check_deadline()?;
                        let Some(mut item) = it.iter_step(&mut iter)? else { break };
                        if is_await {
                            item = it.await_value(item)?;
                        }
                        for &i in &fresh {
                            f.cells[i] = cell(Value::Undefined);
                        }
                        if let Err(e) = bind(it, f, item) {
                            it.iter_close(&iter);
                            return Err(e);
                        }
                        let fl = match body(it, f) {
                            Ok(fl) => fl,
                            Err(e) => {
                                it.iter_close(&iter);
                                return Err(e);
                            }
                        };
                        match loop_next(fl, &label) {
                            Next::Go => {}
                            Next::Stop => {
                                it.iter_close(&iter);
                                break;
                            }
                            Next::Exit(fl) => {
                                it.iter_close(&iter);
                                return Ok(fl);
                            }
                        }
                    }
                    Ok(Flow::Normal)
                })
            }
            _ => unreachable!("loop_stmt on a non-loop"),
        })
    }

    /// The per-iteration scope of a for-in/for-of target and its body.
    fn for_target(&mut self, target: &ForTarget, body: &Stmt, top: bool) -> CResult<(Vec<usize>, Binder, SCode)> {
        self.push_scope();
        let r = (|| match target {
            ForTarget::Decl(kind, p) => {
                let mut ns = Vec::new();
                pattern_names(p, &mut ns);
                if *kind != DeclKind::Var {
                    for n in &ns {
                        self.declare(n, *kind == DeclKind::Const);
                    }
                }
                let b = self.binder(p, Some(*kind))?;
                Ok((b, self.stmt(body, top)?))
            }
            ForTarget::Pattern(p) => {
                let b = self.binder(p, None)?;
                Ok((b, self.stmt(body, top)?))
            }
        })();
        let fresh = self.pop_scope();
        let (b, body) = r?;
        Ok((fresh, b, body))
    }

    // ------------------------------------------------------------ patterns

    /// Code that binds a value to a pattern (declaring it when `decl` is set).
    fn binder(&mut self, p: &Pattern, decl: Option<DeclKind>) -> CResult<Binder> {
        Ok(match p {
            Pattern::Ident(n) => {
                let (loc, konst) = self.resolve(n)?;
                let n = n.clone();
                match (&loc, decl) {
                    (Loc::Env(_), Some(k @ (DeclKind::Let | DeclKind::Const))) => {
                        let mutable = k != DeclKind::Const;
                        Rc::new(move |_it, f, v| {
                            f.env.declare(n.clone(), v, mutable);
                            Ok(())
                        })
                    }
                    _ if konst && decl.is_none() => Rc::new(move |it, _f, _v| throw!(it, "TypeError", "Assignment to constant variable.")),
                    _ => Rc::new(move |it, f, v| write_loc(it, f, &loc, v)),
                }
            }
            Pattern::Expr(e) => match &**e {
                Expr::Ident(n) => self.binder(&Pattern::Ident(n.clone()), None)?,
                Expr::Member { obj, prop, .. } => {
                    let key = self.key(prop)?;
                    if let Expr::Super = **obj {
                        let tl = self.this_code()?;
                        Rc::new(move |it, f, v| {
                            let this = tl(it, f)?;
                            let k = key.get(it, f)?;
                            it.set(&this, k, v)
                        })
                    } else {
                        let o = self.expr(obj)?;
                        Rc::new(move |it, f, v| {
                            let ov = o(it, f)?;
                            let k = key.get(it, f)?;
                            it.set(&ov, k, v)
                        })
                    }
                }
                _ => return unsupported("assignment target"),
            },
            Pattern::Object { props, rest } => {
                let mut parts: Vec<(KeyCode, Binder, Option<ECode>, Option<PropKey>)> = Vec::new();
                for (k, sub, d) in props {
                    let key = self.key(k)?;
                    let b = self.binder(sub, decl)?;
                    let d = match d {
                        Some(d) => Some(self.expr(d)?),
                        None => None,
                    };
                    let name_for = match (sub, d.is_some()) {
                        (Pattern::Ident(n), true) => Some(PropKey::Str(n.clone())),
                        _ => None,
                    };
                    parts.push((key, b, d, name_for));
                }
                let rest = match rest {
                    Some(r) => Some(self.binder(r, decl)?),
                    None => None,
                };
                Rc::new(move |it, f, v| {
                    if v.is_nullish() {
                        throw!(it, "TypeError", "Cannot destructure '{}' as it is {}.", it.short_desc(&v), if v.is_undefined() { "undefined" } else { "null" });
                    }
                    let mut used = Vec::new();
                    for (key, b, d, name_for) in &parts {
                        let k = key.get(it, f)?;
                        let mut val = it.get(&v, &k)?;
                        if val.is_undefined() {
                            if let Some(d) = d {
                                val = d(it, f)?;
                                if let Some(n) = name_for {
                                    it.maybe_name(&val, n);
                                }
                            }
                        }
                        used.push(k);
                        b(it, f, val)?;
                    }
                    if let Some(r) = &rest {
                        let o = it.to_object(&v)?;
                        let out = it.new_object();
                        for k in it.own_keys(&o, false) {
                            if !used.contains(&k) {
                                let val = it.get(&v, &k)?;
                                out.borrow_mut().props.insert(k, Prop::data(val));
                            }
                        }
                        r(it, f, Value::Obj(out))?;
                    }
                    Ok(())
                })
            }
            Pattern::Array { elems, rest } => {
                let mut parts: Vec<Option<(Binder, Option<ECode>)>> = Vec::new();
                for el in elems {
                    parts.push(match el {
                        Some((sub, d)) => {
                            let b = self.binder(sub, decl)?;
                            let d = match d {
                                Some(d) => Some(self.expr(d)?),
                                None => None,
                            };
                            Some((b, d))
                        }
                        None => None,
                    });
                }
                let rest = match rest {
                    Some(r) => Some(self.binder(r, decl)?),
                    None => None,
                };
                Rc::new(move |it, f, v| {
                    let mut iter = it.get_iterator(&v)?;
                    let mut done = false;
                    for part in &parts {
                        let item = if done {
                            None
                        } else {
                            let x = it.iter_step(&mut iter)?;
                            if x.is_none() {
                                done = true;
                            }
                            x
                        };
                        if let Some((b, d)) = part {
                            let mut val = item.unwrap_or(Value::Undefined);
                            if val.is_undefined() {
                                if let Some(d) = d {
                                    val = d(it, f)?;
                                }
                            }
                            b(it, f, val)?;
                        }
                    }
                    if let Some(r) = &rest {
                        let mut items = Vec::new();
                        if !done {
                            while let Some(x) = it.iter_step(&mut iter)? {
                                items.push(x);
                            }
                        }
                        let arr = it.new_array(items);
                        r(it, f, arr)?;
                    } else if !done {
                        it.iter_close(&iter);
                    }
                    Ok(())
                })
            }
        })
    }

    fn key(&mut self, k: &Key) -> CResult<KeyCode> {
        Ok(match k {
            Key::Name(n) => KeyCode::Static(PropKey::Str(n.clone())),
            Key::Private(n) => KeyCode::Static(PropKey::Str(Rc::from(format!("#{n}").as_str()))),
            Key::Computed(e) => KeyCode::Dynamic(self.expr(e)?),
        })
    }

    // ------------------------------------------------------------ expressions

    fn loc_reader(loc: Loc) -> ECode {
        match loc {
            Loc::Slot(i) => ecode!(|_it, f| Ok(f.slots[i].clone())),
            Loc::Cell(i) => ecode!(|_it, f| Ok(f.cells[i].borrow().clone())),
            Loc::Cap(i) => ecode!(|_it, f| Ok(f.caps[i].borrow().clone())),
            Loc::Env(n) => ecode!(|it, f| {
                let env = f.env.clone();
                it.lookup(&n, &env)
            }),
            other => ecode!(|it, f| read_loc(it, f, &other)),
        }
    }

    /// `this`, with the check for derived constructors before `super()`.
    fn this_code(&mut self) -> CResult<ECode> {
        let loc = self.resolve_pseudo(P_THIS)?;
        let module = self.module;
        Ok(match loc {
            Some(loc) => ecode!(|it, f| {
                let v = read_loc(it, f, &loc)?;
                if let Value::Obj(o) = &v {
                    if o.ptr_eq(&it.uninit) {
                        throw!(it, "ReferenceError", "Must call super constructor in derived class before accessing 'this' or returning from derived constructor");
                    }
                }
                Ok(v)
            }),
            None if module => ecode!(|_it, _f| Ok(Value::Undefined)),
            None => ecode!(|it, _f| Ok(Value::Obj(it.global.clone()))),
        })
    }

    fn args(&mut self, args: &[ArrayElem]) -> CResult<ArgsCode> {
        let spread = args.iter().any(|a| matches!(a, ArrayElem::Spread(_)));
        let mut codes: Vec<(bool, ECode)> = Vec::new();
        for a in args {
            match a {
                ArrayElem::Item(e) => codes.push((false, self.expr(e)?)),
                ArrayElem::Spread(e) => codes.push((true, self.expr(e)?)),
                ArrayElem::Hole => codes.push((false, ecode!(|_it, _f| Ok(Value::Undefined)))),
            }
        }
        Ok(if !spread {
            let codes: Rc<[ECode]> = codes.into_iter().map(|c| c.1).collect();
            Rc::new(move |it, f| {
                let mut out = Vec::with_capacity(codes.len());
                for c in codes.iter() {
                    out.push(c(it, f)?);
                }
                Ok(out)
            })
        } else {
            Rc::new(move |it, f| {
                let mut out = Vec::new();
                for (s, c) in &codes {
                    let v = c(it, f)?;
                    if *s {
                        out.extend(it.iterate_to_vec(&v)?);
                    } else {
                        out.push(v);
                    }
                }
                Ok(out)
            })
        })
    }

    fn callee(&mut self, callee: &Expr) -> CResult<CalleeCode> {
        Ok(match callee {
            Expr::Member { obj, prop, optional } => {
                let key = self.key(prop)?;
                if let Expr::Super = **obj {
                    let home = self.resolve_pseudo(P_HOME)?;
                    let tc = self.this_code()?;
                    Rc::new(move |it, f| {
                        let this = tc(it, f)?;
                        let k = key.get(it, f)?;
                        let h = match &home {
                            Some(l) => read_loc(it, f, l)?,
                            None => Value::Undefined,
                        };
                        let func = match h.as_obj().and_then(|h| h.proto()) {
                            Some(p) => it.get_from(&p, &k, &this)?,
                            None => Value::Undefined,
                        };
                        Ok((func, this))
                    })
                } else {
                    let o = self.expr(obj)?;
                    let optional = *optional;
                    Rc::new(move |it, f| {
                        let ov = o(it, f)?;
                        if optional && ov.is_nullish() {
                            return Err(it.short_circuit());
                        }
                        let k = key.get(it, f)?;
                        let func = it.get(&ov, &k)?;
                        Ok((func, ov))
                    })
                }
            }
            Expr::OptChain(inner) => {
                let c = self.callee(inner)?;
                Rc::new(move |it, f| match c(it, f) {
                    Err(e) if is_short(it, &e) => Ok((Value::Undefined, Value::Undefined)),
                    r => r,
                })
            }
            other => {
                let c = self.expr(other)?;
                Rc::new(move |it, f| Ok((c(it, f)?, Value::Undefined)))
            }
        })
    }

    fn super_call(&mut self, args: &[ArrayElem]) -> CResult<ECode> {
        let args = self.args(args)?;
        let (Some(func_l), Some(this_l)) = (self.resolve_pseudo(P_FUNC)?, self.resolve_pseudo(P_THIS)?) else { return unsupported("super() outside a constructor") };
        let nt_l = self.resolve_pseudo(P_NT)?;
        Ok(ecode!(|it, f| {
            let argv = args(it, f)?;
            let fo = read_loc(it, f, &func_l)?;
            let ci = match fo.as_obj().map(|o| match &o.borrow().kind {
                ObjKind::Function(Func::Closure(c)) => c.class.clone(),
                _ => None,
            }) {
                Some(Some(ci)) => ci,
                _ => throw!(it, "SyntaxError", "'super' keyword unexpected here"),
            };
            let cur = read_loc(it, f, &this_l)?;
            if !matches!(&cur, Value::Obj(o) if o.ptr_eq(&it.uninit)) {
                throw!(it, "ReferenceError", "Super constructor may only be called once");
            }
            let nt = match &nt_l {
                Some(l) => read_loc(it, f, l)?,
                None => Value::Undefined,
            };
            let parent = ci.parent.clone().unwrap_or(Value::Undefined);
            let result = if parent.is_undefined() { Value::Obj(it.object_for_new(&nt)?) } else { it.construct(&parent, &argv, Some(&nt))? };
            write_loc(it, f, &this_l, result.clone())?;
            if let Value::Obj(o) = &result {
                it.init_fields(&ci, o)?;
            }
            Ok(result)
        }))
    }

    fn expr(&mut self, e: &Expr) -> CResult<ECode> {
        Ok(match e {
            Expr::Num(n) => {
                let n = *n;
                ecode!(|_it, _f| Ok(Value::Num(n)))
            }
            Expr::Str(s) => {
                let s = s.clone();
                ecode!(|_it, _f| Ok(Value::Str(s.clone())))
            }
            Expr::BigInt(s) => {
                let b: i128 = s.parse().unwrap_or(0);
                ecode!(|_it, _f| Ok(Value::BigInt(b)))
            }
            Expr::Bool(b) => {
                let b = *b;
                ecode!(|_it, _f| Ok(Value::Bool(b)))
            }
            Expr::Null => ecode!(|_it, _f| Ok(Value::Null)),
            Expr::Ident(n) => {
                let (loc, _) = self.resolve(n)?;
                match (&loc, &**n) {
                    (Loc::Env(_), "undefined") => ecode!(|_it, _f| Ok(Value::Undefined)),
                    _ => Self::loc_reader(loc),
                }
            }
            Expr::This => self.this_code()?,
            Expr::NewTarget => match self.resolve_pseudo(P_NT)? {
                Some(l) => Self::loc_reader(l),
                None => ecode!(|_it, _f| Ok(Value::Undefined)),
            },
            Expr::Template { quasis, exprs } => {
                let quasis = quasis.clone();
                let exprs: Vec<ECode> = exprs.iter().map(|x| self.expr(x)).collect::<CResult<_>>()?;
                ecode!(|it, f| {
                    let mut s = String::new();
                    for (i, q) in quasis.iter().enumerate() {
                        s.push_str(q);
                        if let Some(c) = exprs.get(i) {
                            let v = c(it, f)?;
                            s.push_str(&it.to_str(&v)?);
                        }
                    }
                    Ok(Value::Str(Rc::from(s.as_str())))
                })
            }
            Expr::TaggedTemplate { tag, cooked, raw, exprs } => {
                let callee = self.callee(tag)?;
                let (cooked, raw) = (cooked.clone(), raw.clone());
                let exprs: Vec<ECode> = exprs.iter().map(|x| self.expr(x)).collect::<CResult<_>>()?;
                ecode!(|it, f| {
                    let (func, this) = callee(it, f)?;
                    let strings = it.new_array(cooked.iter().map(|s| Value::Str(s.clone())).collect());
                    let raws = it.new_array(raw.iter().map(|s| Value::Str(s.clone())).collect());
                    if let Value::Obj(o) = &strings {
                        o.set_hidden("raw", raws);
                    }
                    let mut args = vec![strings];
                    for c in &exprs {
                        args.push(c(it, f)?);
                    }
                    it.call(&func, this, &args)
                })
            }
            Expr::Regex { pattern, flags } => {
                let (p, fl) = (pattern.clone(), flags.clone());
                ecode!(|it, _f| super::builtins::regexp::new_regexp(it, &p, &fl))
            }
            Expr::Array(elems) => {
                let a = self.args(elems)?;
                ecode!(|it, f| {
                    let items = a(it, f)?;
                    Ok(it.new_array(items))
                })
            }
            Expr::Object(props) => self.object(props)?,
            Expr::Function(def) => {
                let self_name = if def.is_arrow { None } else { def.name.as_ref() };
                let cf = self.function(def, self_name)?;
                let def = def.clone();
                ecode!(|it, f| Ok(make_fn(it, f, &def, &cf, None)))
            }
            Expr::Class(c) => self.class(c)?,
            Expr::Unary(op, a) => self.unary(*op, a)?,
            Expr::Update { inc, prefix, target } => self.update(*inc, *prefix, target)?,
            Expr::Binary(op, l, r) => self.binary(*op, l, r)?,
            Expr::Logical(op, l, r) => {
                let (a, b) = (self.expr(l)?, self.expr(r)?);
                match op {
                    LogicOp::And => ecode!(|it, f| {
                        let v = a(it, f)?;
                        if v.truthy() { b(it, f) } else { Ok(v) }
                    }),
                    LogicOp::Or => ecode!(|it, f| {
                        let v = a(it, f)?;
                        if v.truthy() { Ok(v) } else { b(it, f) }
                    }),
                    LogicOp::Nullish => ecode!(|it, f| {
                        let v = a(it, f)?;
                        if v.is_nullish() { b(it, f) } else { Ok(v) }
                    }),
                }
            }
            Expr::Assign(op, target, value) => self.assign(*op, target, value)?,
            Expr::Cond(t, a, b) => {
                let (t, a, b) = (self.cond(t)?, self.expr(a)?, self.expr(b)?);
                ecode!(|it, f| if t(it, f)? { a(it, f) } else { b(it, f) })
            }
            Expr::Seq(v) => {
                let codes: Vec<ECode> = v.iter().map(|x| self.expr(x)).collect::<CResult<_>>()?;
                ecode!(|it, f| {
                    let mut last = Value::Undefined;
                    for c in &codes {
                        last = c(it, f)?;
                    }
                    Ok(last)
                })
            }
            Expr::Member { obj, prop, optional } => {
                if let Expr::Super = **obj {
                    let key = self.key(prop)?;
                    let home = self.resolve_pseudo(P_HOME)?;
                    let tc = self.this_code()?;
                    return Ok(ecode!(|it, f| {
                        let this = tc(it, f)?;
                        let k = key.get(it, f)?;
                        let h = match &home {
                            Some(l) => read_loc(it, f, l)?,
                            None => Value::Undefined,
                        };
                        match h.as_obj().and_then(|h| h.proto()) {
                            Some(p) => it.get_from(&p, &k, &this),
                            None => Ok(Value::Undefined),
                        }
                    }));
                }
                let o = self.expr(obj)?;
                let optional = *optional;
                match &**prop {
                    Key::Computed(ke) => {
                        let kc = self.expr(ke)?;
                        ecode!(|it, f| {
                            let ov = o(it, f)?;
                            if optional && ov.is_nullish() {
                                return Err(it.short_circuit());
                            }
                            let k = kc(it, f)?;
                            // fast paths: array[index], typed[index]
                            if let (Value::Num(n), Value::Obj(ao)) = (&k, &ov) {
                                let n = *n;
                                if n >= 0.0 && n.fract() == 0.0 {
                                    match &ao.borrow().kind {
                                        ObjKind::Array(items) => {
                                            if let Some(v) = items.get(n as usize) {
                                                return Ok(v.clone());
                                            }
                                        }
                                        ObjKind::TypedArray(t) => {
                                            if let Some(x) = t.get(n as usize) {
                                                return Ok(Value::Num(x));
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            let key = it.to_key(&k)?;
                            it.get(&ov, &key)
                        })
                    }
                    other => {
                        let key = match self.key(other)? {
                            KeyCode::Static(k) => k,
                            KeyCode::Dynamic(_) => unreachable!(),
                        };
                        ecode!(|it, f| {
                            let ov = o(it, f)?;
                            if optional && ov.is_nullish() {
                                return Err(it.short_circuit());
                            }
                            it.get(&ov, &key)
                        })
                    }
                }
            }
            Expr::OptChain(inner) => {
                let c = self.expr(inner)?;
                ecode!(|it, f| match c(it, f) {
                    Err(e) if is_short(it, &e) => Ok(Value::Undefined),
                    r => r,
                })
            }
            Expr::Call { callee, args, optional } => {
                if let Expr::Super = **callee {
                    return self.super_call(args);
                }
                let c = self.callee(callee)?;
                let a = self.args(args)?;
                let optional = *optional;
                let desc: Rc<str> = Rc::from(callee_desc(callee).as_str());
                ecode!(|it, f| {
                    let (func, this) = c(it, f)?;
                    if optional && func.is_nullish() {
                        return Err(it.short_circuit());
                    }
                    let argv = a(it, f)?;
                    if !func.is_callable() {
                        throw!(it, "TypeError", "{desc} is not a function");
                    }
                    it.call(&func, this, &argv)
                })
            }
            Expr::New { callee, args } => {
                let c = self.expr(callee)?;
                let a = self.args(args)?;
                let desc: Rc<str> = Rc::from(callee_desc(callee).as_str());
                ecode!(|it, f| {
                    let func = c(it, f)?;
                    let argv = a(it, f)?;
                    if !func.is_callable() {
                        throw!(it, "TypeError", "{desc} is not a constructor");
                    }
                    it.construct(&func, &argv, None)
                })
            }
            Expr::Super => return unsupported("bare super"),
            Expr::Await(a) => {
                let c = self.expr(a)?;
                ecode!(|it, f| {
                    let v = c(it, f)?;
                    it.await_value(v)
                })
            }
            Expr::Yield { arg, delegate } => {
                let c = match arg {
                    Some(a) => Some(self.expr(a)?),
                    None => None,
                };
                let delegate = *delegate;
                ecode!(|it, f| {
                    let v = match &c {
                        Some(c) => c(it, f)?,
                        None => Value::Undefined,
                    };
                    if delegate {
                        let mut iter = it.get_iterator(&v)?;
                        while let Some(x) = it.iter_step(&mut iter)? {
                            it.co_suspend(Yield::Yielded(x))?;
                        }
                        return Ok(Value::Undefined);
                    }
                    it.co_suspend(Yield::Yielded(v))
                })
            }
            Expr::PrivateIn(n, obj) => {
                let o = self.expr(obj)?;
                let k = PropKey::Str(Rc::from(format!("#{n}").as_str()));
                let n = n.clone();
                ecode!(|it, f| {
                    let Value::Obj(ob) = o(it, f)? else { throw!(it, "TypeError", "Cannot use 'in' operator to search for '#{n}' in non-object") };
                    Ok(Value::Bool(it.has_property(&ob, &k)))
                })
            }
        })
    }

    /// An operand for `numop!`: a local slot or a numeric literal when possible.
    fn opnd(&mut self, e: &Expr) -> CResult<Opnd> {
        match e {
            Expr::Num(n) => return Ok(Opnd::Num(*n)),
            Expr::Ident(n) => {
                if let (Loc::Slot(i), _) = self.resolve(n)? {
                    return Ok(Opnd::Slot(i));
                }
            }
            _ => {}
        }
        Ok(Opnd::Code(self.expr(e)?))
    }

    fn binary(&mut self, op: BinOp, le: &Expr, re: &Expr) -> CResult<ECode> {
        use super::interp::{js_mod, js_pow};
        if num_op(op).is_some() || matches!(op, BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge) {
            let (l, r) = (self.opnd(le)?, self.opnd(re)?);
            return Ok(match op {
                BinOp::Add => numop!(l, r, op, |x, y| Value::Num(x + y)),
                BinOp::Sub => numop!(l, r, op, |x, y| Value::Num(x - y)),
                BinOp::Mul => numop!(l, r, op, |x, y| Value::Num(x * y)),
                BinOp::Div => numop!(l, r, op, |x, y| Value::Num(x / y)),
                BinOp::Mod => numop!(l, r, op, |x, y| Value::Num(js_mod(x, y))),
                BinOp::Exp => numop!(l, r, op, |x, y| Value::Num(js_pow(x, y))),
                BinOp::Lt => numop!(l, r, op, |x, y| Value::Bool(x < y)),
                BinOp::Gt => numop!(l, r, op, |x, y| Value::Bool(x > y)),
                BinOp::Le => numop!(l, r, op, |x, y| Value::Bool(x <= y)),
                BinOp::Ge => numop!(l, r, op, |x, y| Value::Bool(x >= y)),
                BinOp::BitAnd => numop!(l, r, op, |x, y| Value::Num((to_int32(x) & to_int32(y)) as f64)),
                BinOp::BitOr => numop!(l, r, op, |x, y| Value::Num((to_int32(x) | to_int32(y)) as f64)),
                BinOp::BitXor => numop!(l, r, op, |x, y| Value::Num((to_int32(x) ^ to_int32(y)) as f64)),
                BinOp::Shl => numop!(l, r, op, |x, y| Value::Num(to_int32(x).wrapping_shl(to_uint32(y) & 31) as f64)),
                BinOp::Shr => numop!(l, r, op, |x, y| Value::Num((to_int32(x) >> (to_uint32(y) & 31)) as f64)),
                _ => numop!(l, r, op, |x, y| Value::Num((to_uint32(x) >> (to_uint32(y) & 31)) as f64)),
            });
        }
        let (l, r) = (self.expr(le)?, self.expr(re)?);
        Ok(match op {
            BinOp::StrictEq => ecode!(|it, f| {
                let a = l(it, f)?;
                let b = r(it, f)?;
                Ok(Value::Bool(strict_equals(&a, &b)))
            }),
            BinOp::StrictNe => ecode!(|it, f| {
                let a = l(it, f)?;
                let b = r(it, f)?;
                Ok(Value::Bool(!strict_equals(&a, &b)))
            }),
            _ => ecode!(|it, f| {
                let a = l(it, f)?;
                let b = r(it, f)?;
                it.binary(op, &a, &b)
            }),
        })
    }

    fn unary(&mut self, op: UnaryOp, arg: &Expr) -> CResult<ECode> {
        Ok(match op {
            UnaryOp::TypeOf => {
                let c = self.expr(arg)?;
                if let Expr::Ident(_) = arg {
                    // an undeclared global is "undefined", not an error
                    ecode!(|it, f| Ok(Value::str(match c(it, f) {
                        Ok(v) => v.type_of(),
                        Err(_) => "undefined",
                    })))
                } else {
                    ecode!(|it, f| Ok(Value::str(c(it, f)?.type_of())))
                }
            }
            UnaryOp::Delete => match arg {
                Expr::Member { obj, prop, .. } => {
                    let o = self.expr(obj)?;
                    let k = self.key(prop)?;
                    ecode!(|it, f| {
                        let ov = o(it, f)?;
                        let key = k.get(it, f)?;
                        Ok(Value::Bool(it.delete(&ov, &key)?))
                    })
                }
                Expr::OptChain(inner) => {
                    let c = self.unary(UnaryOp::Delete, inner)?;
                    ecode!(|it, f| match c(it, f) {
                        Err(e) if is_short(it, &e) => Ok(Value::Bool(true)),
                        r => r,
                    })
                }
                _ => {
                    let c = self.expr(arg)?;
                    ecode!(|it, f| {
                        let _ = c(it, f);
                        Ok(Value::Bool(true))
                    })
                }
            },
            UnaryOp::Void => {
                let c = self.expr(arg)?;
                ecode!(|it, f| {
                    c(it, f)?;
                    Ok(Value::Undefined)
                })
            }
            UnaryOp::Not => {
                let c = self.expr(arg)?;
                ecode!(|it, f| Ok(Value::Bool(!c(it, f)?.truthy())))
            }
            UnaryOp::Neg => {
                let c = self.expr(arg)?;
                ecode!(|it, f| {
                    let v = c(it, f)?;
                    if let Value::Num(n) = v {
                        return Ok(Value::Num(-n));
                    }
                    Ok(match it.to_numeric(&v)? {
                        Value::BigInt(b) => Value::BigInt(-b),
                        Value::Num(n) => Value::Num(-n),
                        _ => Value::Num(f64::NAN),
                    })
                })
            }
            UnaryOp::Plus => {
                let c = self.expr(arg)?;
                ecode!(|it, f| {
                    let v = c(it, f)?;
                    if let Value::Num(_) = v {
                        return Ok(v);
                    }
                    Ok(Value::Num(it.to_number(&v)?))
                })
            }
            UnaryOp::BitNot => {
                let c = self.expr(arg)?;
                ecode!(|it, f| {
                    let v = c(it, f)?;
                    Ok(match it.to_numeric(&v)? {
                        Value::BigInt(b) => Value::BigInt(!b),
                        Value::Num(n) => Value::Num(!to_int32(n) as f64),
                        _ => Value::Num(-1.0),
                    })
                })
            }
        })
    }
}

/// `var` names declared in statements (not inside nested functions), for synthesized bodies.
fn var_names_of(stmts: &[Stmt], out: &mut Vec<Name>) {
    for s in stmts {
        match &s.kind {
            StmtKind::Decl(DeclKind::Var, ds) => {
                for (p, _) in ds {
                    pattern_names(p, out);
                }
            }
            StmtKind::If(_, a, b) => {
                var_names_of(std::slice::from_ref(a), out);
                if let Some(b) = b {
                    var_names_of(std::slice::from_ref(b), out);
                }
            }
            StmtKind::Block(b) => var_names_of(&b.body, out),
            StmtKind::For { init, body, .. } => {
                if let Some(ForInit::Decl(DeclKind::Var, ds)) = init {
                    for (p, _) in ds {
                        pattern_names(p, out);
                    }
                }
                var_names_of(std::slice::from_ref(body), out);
            }
            StmtKind::ForIn { target, body, .. } | StmtKind::ForOf { target, body, .. } => {
                if let ForTarget::Decl(DeclKind::Var, p) = target {
                    pattern_names(p, out);
                }
                var_names_of(std::slice::from_ref(body), out);
            }
            StmtKind::While(_, b) | StmtKind::DoWhile(b, _) | StmtKind::Labeled(_, b) => var_names_of(std::slice::from_ref(b), out),
            StmtKind::Try { block, handler, finalizer, .. } => {
                var_names_of(&block.body, out);
                if let Some(h) = handler {
                    var_names_of(&h.body, out);
                }
                if let Some(f) = finalizer {
                    var_names_of(&f.body, out);
                }
            }
            StmtKind::Switch(_, cases, _) => {
                for c in cases {
                    var_names_of(&c.body, out);
                }
            }
            _ => {}
        }
    }
}

/// A method-like function with the given body (field initializers, static blocks).
fn synth_method(body: FuncBody, line: u32) -> Rc<FuncDef> {
    let mut vars = Vec::new();
    if let FuncBody::Block(b) = &body {
        var_names_of(b, &mut vars);
    }
    Rc::new(FuncDef {
        name: None,
        params: vec![],
        rest: None,
        body,
        is_arrow: false,
        is_async: false,
        is_generator: false,
        is_method: true,
        var_names: vars,
        uses_arguments: false,
        uses_this: true,
        simple_params: true,
        line,
        source: Rc::from(""),
    })
}

enum PropC {
    KeyValue(KeyCode, ECode, bool, bool),
    Shorthand(Name, ECode),
    Method(KeyCode, MethodKind, Rc<FuncDef>, Rc<CFunc>),
    Spread(ECode),
}

enum MemberC {
    Method { key: KeyCode, kind: MethodKind, is_static: bool, def: Rc<FuncDef>, cf: Rc<CFunc> },
    Field { key: KeyCode, is_static: bool, init: Option<(Rc<FuncDef>, Rc<CFunc>)> },
    StaticBlock(Rc<FuncDef>, Rc<CFunc>),
}

fn insert_method(target: &Obj, k: PropKey, kind: MethodKind, mv: Value, enumerable: bool) {
    let mut b = target.borrow_mut();
    match kind {
        MethodKind::Method => {
            let p = if enumerable { Prop::data(mv) } else { Prop::hidden(mv) };
            b.props.insert(k, p);
        }
        MethodKind::Get => {
            let setter = b.props.get(&k).and_then(|p| p.setter.clone());
            b.props.insert(k, Prop { value: Value::Undefined, getter: Some(mv), setter, enumerable, writable: true, configurable: true });
        }
        MethodKind::Set => {
            let getter = b.props.get(&k).and_then(|p| p.getter.clone());
            b.props.insert(k, Prop { value: Value::Undefined, getter, setter: Some(mv), enumerable, writable: true, configurable: true });
        }
    }
}

impl Compiler {
    fn update(&mut self, inc: bool, prefix: bool, target: &Expr) -> CResult<ECode> {
        let delta = if inc { 1.0 } else { -1.0 };
        let step = move |it: &mut Interp, old: Value| -> R<(Value, Value)> {
            if let Value::Num(n) = old {
                return Ok((Value::Num(n), Value::Num(n + delta)));
            }
            let oldn = it.to_numeric(&old)?;
            let newv = match &oldn {
                Value::BigInt(b) => Value::BigInt(if inc { b + 1 } else { b - 1 }),
                Value::Num(n) => Value::Num(n + delta),
                _ => Value::Num(f64::NAN),
            };
            Ok((oldn, newv))
        };
        Ok(match target {
            Expr::Ident(n) => {
                let (loc, konst) = self.resolve(n)?;
                if konst {
                    return Ok(ecode!(|it, _f| throw!(it, "TypeError", "Assignment to constant variable.")));
                }
                if let Loc::Slot(i) = loc {
                    // the hot loop counter
                    return Ok(ecode!(|it, f| {
                        if let Value::Num(n) = f.slots[i] {
                            f.slots[i] = Value::Num(n + delta);
                            return Ok(Value::Num(if prefix { n + delta } else { n }));
                        }
                        let old = f.slots[i].clone();
                        let (o, nv) = step(it, old)?;
                        f.slots[i] = nv.clone();
                        Ok(if prefix { nv } else { o })
                    }));
                }
                ecode!(|it, f| {
                    let old = read_loc(it, f, &loc)?;
                    let (o, nv) = step(it, old)?;
                    write_loc(it, f, &loc, nv.clone())?;
                    Ok(if prefix { nv } else { o })
                })
            }
            Expr::Member { obj, prop, .. } => {
                if let Expr::Super = **obj {
                    return unsupported("super member update");
                }
                let o = self.expr(obj)?;
                let k = self.key(prop)?;
                ecode!(|it, f| {
                    let ov = o(it, f)?;
                    let key = k.get(it, f)?;
                    let old = it.get(&ov, &key)?;
                    let (oldv, nv) = step(it, old)?;
                    it.set(&ov, key, nv.clone())?;
                    Ok(if prefix { nv } else { oldv })
                })
            }
            _ => return unsupported("update target"),
        })
    }

    fn assign(&mut self, op: AssignOp, target: &Pattern, value: &Expr) -> CResult<ECode> {
        // normalise `(x) = ...` written as an expression pattern
        let target = match target {
            Pattern::Expr(e) => match &**e {
                Expr::Ident(n) => Pattern::Ident(n.clone()),
                _ => target.clone(),
            },
            t => t.clone(),
        };
        match op {
            AssignOp::Assign => match &target {
                Pattern::Ident(n) => {
                    let (loc, konst) = self.resolve(n)?;
                    let v = self.expr(value)?;
                    if konst {
                        return Ok(ecode!(|it, f| {
                            v(it, f)?;
                            throw!(it, "TypeError", "Assignment to constant variable.")
                        }));
                    }
                    if is_anon_fn(value) {
                        let key = PropKey::Str(n.clone());
                        return Ok(ecode!(|it, f| {
                            let x = v(it, f)?;
                            it.maybe_name(&x, &key);
                            write_loc(it, f, &loc, x.clone())?;
                            Ok(x)
                        }));
                    }
                    Ok(match loc {
                        Loc::Slot(i) => ecode!(|it, f| {
                            let x = v(it, f)?;
                            if let Value::Num(n) = x {
                                store_num(&mut f.slots[i], n);
                                return Ok(x);
                            }
                            f.slots[i] = x.clone();
                            Ok(x)
                        }),
                        Loc::Cell(i) => ecode!(|it, f| {
                            let x = v(it, f)?;
                            *f.cells[i].borrow_mut() = x.clone();
                            Ok(x)
                        }),
                        loc => ecode!(|it, f| {
                            let x = v(it, f)?;
                            write_loc(it, f, &loc, x.clone())?;
                            Ok(x)
                        }),
                    })
                }
                Pattern::Expr(e) => match &**e {
                    Expr::Member { obj, prop, .. } => {
                        let key = self.key(prop)?;
                        let o = if let Expr::Super = **obj { self.this_code()? } else { self.expr(obj)? };
                        let v = self.expr(value)?;
                        Ok(ecode!(|it, f| {
                            let ov = o(it, f)?;
                            let k = key.get(it, f)?;
                            let x = v(it, f)?;
                            // array[index] = x on plain arrays
                            if let (Value::Obj(ao), PropKey::Str(_)) = (&ov, &k) {
                                if let Some(i) = k.array_index() {
                                    let mut b = ao.borrow_mut();
                                    if !b.frozen {
                                        if let ObjKind::Array(items) = &mut b.kind {
                                            if i < items.len() {
                                                items[i] = x.clone();
                                                return Ok(x);
                                            }
                                        }
                                    }
                                }
                            }
                            it.set(&ov, k, x.clone())?;
                            Ok(x)
                        }))
                    }
                    _ => unsupported("assignment target"),
                },
                pat => {
                    let b = self.binder(pat, None)?;
                    let v = self.expr(value)?;
                    Ok(ecode!(|it, f| {
                        let x = v(it, f)?;
                        b(it, f, x.clone())?;
                        Ok(x)
                    }))
                }
            },
            AssignOp::Op(bop) => {
                let rhs = self.expr(value)?;
                match &target {
                    Pattern::Ident(n) => {
                        let (loc, konst) = self.resolve(n)?;
                        if konst {
                            return Ok(ecode!(|it, _f| throw!(it, "TypeError", "Assignment to constant variable.")));
                        }
                        if let (Loc::Slot(i), Some(nop)) = (&loc, num_op(bop)) {
                            // `x op= y` on a local number: in place, no boxing
                            let i = *i;
                            return Ok(ecode!(|it, f| {
                                let r = rhs(it, f)?;
                                if let (Value::Num(x), Value::Num(y)) = (&mut f.slots[i], &r) {
                                    *x = nop(*x, *y);
                                    return Ok(Value::Num(*x));
                                }
                                let old = f.slots[i].clone();
                                let nv = it.binary(bop, &old, &r)?;
                                f.slots[i] = nv.clone();
                                Ok(nv)
                            }));
                        }
                        if let (Loc::Cell(i), Some(nop)) = (&loc, num_op(bop)) {
                            let i = *i;
                            return Ok(ecode!(|it, f| {
                                let r = rhs(it, f)?;
                                {
                                    let mut c = f.cells[i].borrow_mut();
                                    if let (Value::Num(x), Value::Num(y)) = (&mut *c, &r) {
                                        *x = nop(*x, *y);
                                        return Ok(Value::Num(*x));
                                    }
                                }
                                let old = f.cells[i].borrow().clone();
                                let nv = it.binary(bop, &old, &r)?;
                                *f.cells[i].borrow_mut() = nv.clone();
                                Ok(nv)
                            }));
                        }
                        Ok(ecode!(|it, f| {
                            let old = read_loc(it, f, &loc)?;
                            let r = rhs(it, f)?;
                            let nv = it.binary(bop, &old, &r)?;
                            write_loc(it, f, &loc, nv.clone())?;
                            Ok(nv)
                        }))
                    }
                    Pattern::Expr(e) => match &**e {
                        Expr::Member { obj, prop, .. } => {
                            if let Expr::Super = **obj {
                                return unsupported("super compound assignment");
                            }
                            let o = self.expr(obj)?;
                            let k = self.key(prop)?;
                            Ok(ecode!(|it, f| {
                                let ov = o(it, f)?;
                                let key = k.get(it, f)?;
                                let old = it.get(&ov, &key)?;
                                let r = rhs(it, f)?;
                                let nv = it.binary(bop, &old, &r)?;
                                it.set(&ov, key, nv.clone())?;
                                Ok(nv)
                            }))
                        }
                        _ => unsupported("compound assignment target"),
                    },
                    _ => unsupported("compound assignment target"),
                }
            }
            AssignOp::Logic(lop) => {
                let rhs = self.expr(value)?;
                let should = move |v: &Value| match lop {
                    LogicOp::And => v.truthy(),
                    LogicOp::Or => !v.truthy(),
                    LogicOp::Nullish => v.is_nullish(),
                };
                match &target {
                    Pattern::Ident(n) => {
                        let (loc, konst) = self.resolve(n)?;
                        Ok(ecode!(|it, f| {
                            let old = read_loc(it, f, &loc)?;
                            if !should(&old) {
                                return Ok(old);
                            }
                            if konst {
                                throw!(it, "TypeError", "Assignment to constant variable.");
                            }
                            let v = rhs(it, f)?;
                            write_loc(it, f, &loc, v.clone())?;
                            Ok(v)
                        }))
                    }
                    Pattern::Expr(e) => match &**e {
                        Expr::Member { obj, prop, .. } => {
                            let o = self.expr(obj)?;
                            let k = self.key(prop)?;
                            Ok(ecode!(|it, f| {
                                let ov = o(it, f)?;
                                let key = k.get(it, f)?;
                                let old = it.get(&ov, &key)?;
                                if !should(&old) {
                                    return Ok(old);
                                }
                                let v = rhs(it, f)?;
                                it.set(&ov, key, v.clone())?;
                                Ok(v)
                            }))
                        }
                        _ => unsupported("logical assignment target"),
                    },
                    _ => unsupported("logical assignment target"),
                }
            }
        }
    }

    fn object(&mut self, props: &[ObjProp]) -> CResult<ECode> {
        let mut ps: Vec<PropC> = Vec::new();
        for p in props {
            ps.push(match p {
                ObjProp::KeyValue(k, v) => {
                    let is_proto = matches!(k, Key::Name(n) if &**n == "__proto__");
                    PropC::KeyValue(self.key(k)?, self.expr(v)?, is_anon_fn(v), is_proto)
                }
                ObjProp::Shorthand(n) => {
                    let (loc, _) = self.resolve(n)?;
                    PropC::Shorthand(n.clone(), Self::loc_reader(loc))
                }
                ObjProp::Method(k, kind, def) => {
                    let cf = self.function(def, None)?;
                    PropC::Method(self.key(k)?, *kind, def.clone(), cf)
                }
                ObjProp::Spread(e) => PropC::Spread(self.expr(e)?),
            });
        }
        Ok(ecode!(|it, f| {
            let o = it.new_object();
            for p in &ps {
                match p {
                    PropC::KeyValue(k, v, anon, is_proto) => {
                        let key = k.get(it, f)?;
                        let val = v(it, f)?;
                        if *anon {
                            it.maybe_name(&val, &key);
                        }
                        if *is_proto {
                            match val {
                                Value::Obj(p) => o.borrow_mut().proto = Some(p),
                                Value::Null => o.borrow_mut().proto = None,
                                _ => {}
                            }
                            continue;
                        }
                        o.borrow_mut().props.insert(key, Prop::data(val));
                    }
                    PropC::Shorthand(n, c) => {
                        let v = c(it, f)?;
                        o.borrow_mut().props.insert(PropKey::Str(n.clone()), Prop::data(v));
                    }
                    PropC::Method(k, kind, def, cf) => {
                        let key = k.get(it, f)?;
                        let fv = make_fn(it, f, def, cf, Some(o.clone()));
                        it.maybe_name(&fv, &key);
                        insert_method(&o, key, *kind, fv, true);
                    }
                    PropC::Spread(c) => {
                        let v = c(it, f)?;
                        if v.is_nullish() {
                            continue;
                        }
                        if let Value::Str(s) = &v {
                            for (i, ch) in s.chars().enumerate() {
                                o.set_prop(&i.to_string(), Value::Str(Rc::from(ch.to_string().as_str())));
                            }
                            continue;
                        }
                        if let Value::Obj(src) = &v {
                            for k in it.own_keys_with_symbols(src, false) {
                                let val = it.get(&v, &k)?;
                                o.borrow_mut().props.insert(k, Prop::data(val));
                            }
                        }
                    }
                }
            }
            Ok(Value::Obj(o))
        }))
    }

    fn class(&mut self, c: &Rc<ClassDef>) -> CResult<ECode> {
        let superclass = match &c.superclass {
            Some(e) => Some(self.expr(e)?),
            None => None,
        };
        let derived = c.superclass.is_some();
        self.push_scope();
        let r = (|| {
            let name_loc = match &c.name {
                Some(n) => Some(self.declare(n, true)),
                None => None,
            };
            let ctor_def = match &c.constructor {
                Some(f) => f.clone(),
                None => Rc::new(FuncDef {
                    name: c.name.clone(),
                    params: vec![],
                    rest: if derived { Some(Pattern::Ident(Rc::from("args"))) } else { None },
                    body: FuncBody::Block(if derived {
                        vec![Stmt { kind: StmtKind::Expr(Expr::Call { callee: Box::new(Expr::Super), args: vec![ArrayElem::Spread(Expr::Ident(Rc::from("args")))], optional: false }), line: 0 }]
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
            let ctor_cf = self.function(&ctor_def, None)?;
            let mut members = Vec::new();
            for m in &c.members {
                members.push(match m {
                    ClassMember::Method { key, kind, is_static, func } => {
                        let key = self.key(key)?;
                        let cf = self.function(func, None)?;
                        MemberC::Method { key, kind: *kind, is_static: *is_static, def: func.clone(), cf }
                    }
                    ClassMember::Field { key, is_static, value } => {
                        let key = self.key(key)?;
                        let init = match value {
                            Some(v) => {
                                let def = synth_method(FuncBody::Expr(v.clone()), 0);
                                let cf = self.function(&def, None)?;
                                Some((def, cf))
                            }
                            None => None,
                        };
                        MemberC::Field { key, is_static: *is_static, init }
                    }
                    ClassMember::StaticBlock(body) => {
                        let def = synth_method(FuncBody::Block(body.clone()), 0);
                        let cf = self.function(&def, None)?;
                        MemberC::StaticBlock(def, cf)
                    }
                });
            }
            Ok((name_loc, ctor_def, ctor_cf, members))
        })();
        let fresh = self.pop_scope();
        let (name_loc, ctor_def, ctor_cf, members) = r?;
        let cdef = c.clone();
        Ok(ecode!(|it, f| {
            for &i in &fresh {
                f.cells[i] = cell(Value::Undefined);
            }
            let (parent, proto_parent) = match &superclass {
                Some(sc) => {
                    let p = sc(it, f)?;
                    match &p {
                        Value::Null => (None, None),
                        Value::Obj(o) if o.is_callable() => {
                            let pp = it.get_str(&p, "prototype")?;
                            let pp = match pp {
                                Value::Obj(o) => Some(o),
                                Value::Null => None,
                                _ => throw!(it, "TypeError", "Class extends value does not have valid prototype property"),
                            };
                            (Some(p), pp)
                        }
                        _ => throw!(it, "TypeError", "Class extends value {} is not a constructor or null", it.short_desc(&p)),
                    }
                }
                None => (None, Some(it.p.object.clone())),
            };
            let proto = Obj::new(ObjKind::Ordinary, proto_parent, "Object");
            let ci = Rc::new(ClassInfo { def: cdef.clone(), env: f.env.clone(), derived, parent: parent.clone(), fields: RefCell::new(vec![]), proto: RefCell::new(Some(proto.clone())) });
            let caps: Rc<[Cell]> = ctor_cf
                .caps
                .iter()
                .map(|s| match s {
                    CapSrc::Cell(i) => f.cells[*i].clone(),
                    CapSrc::Cap(i) => f.caps[*i].clone(),
                })
                .collect();
            let closure = Rc::new(Closure { def: ctor_def.clone(), env: f.env.clone(), home: RefCell::new(Some(proto.clone())), class: Some(ci.clone()), compiled: Some(ctor_cf.clone()), caps });
            let fproto = match &parent {
                Some(Value::Obj(p)) => p.clone(),
                _ => it.p.function.clone(),
            };
            let fo = Obj::new(ObjKind::Function(Func::Closure(closure)), Some(fproto), "Function");
            {
                let mut b = fo.borrow_mut();
                b.props.insert(PropKey::from("name"), Prop { value: Value::str(cdef.name.as_deref().unwrap_or("")), getter: None, setter: None, enumerable: false, writable: false, configurable: true });
                b.props.insert(PropKey::from("prototype"), Prop { value: Value::Obj(proto.clone()), getter: None, setter: None, enumerable: false, writable: false, configurable: false });
            }
            proto.set_hidden("constructor", Value::Obj(fo.clone()));
            let fv = Value::Obj(fo.clone());
            if let Some(l) = &name_loc {
                write_loc(it, f, l, fv.clone())?;
            }
            let mut statics: Vec<(Option<PropKey>, Value)> = Vec::new();
            for m in &members {
                match m {
                    MemberC::Method { key, kind, is_static, def, cf } => {
                        let target = if *is_static { fo.clone() } else { proto.clone() };
                        let k = key.get(it, f)?;
                        let mv = make_fn(it, f, def, cf, Some(target.clone()));
                        it.maybe_name(&mv, &k);
                        insert_method(&target, k, *kind, mv, false);
                    }
                    MemberC::Field { key, is_static, init } => {
                        let k = key.get(it, f)?;
                        let home = if *is_static { fo.clone() } else { proto.clone() };
                        let initf = init.as_ref().map(|(def, cf)| make_fn(it, f, def, cf, Some(home)));
                        if *is_static {
                            statics.push((Some(k), initf.unwrap_or(Value::Undefined)));
                        } else {
                            ci.fields.borrow_mut().push((k, initf.map(FieldInit::Func).unwrap_or(FieldInit::None)));
                        }
                    }
                    MemberC::StaticBlock(def, cf) => {
                        let bf = make_fn(it, f, def, cf, Some(fo.clone()));
                        statics.push((None, bf));
                    }
                }
            }
            // static fields and blocks run in order with `this` = the class
            for (k, initf) in statics {
                match k {
                    Some(k) => {
                        let v = if initf.is_callable() { it.call(&initf, fv.clone(), &[])? } else { Value::Undefined };
                        it.maybe_name(&v, &k);
                        fo.borrow_mut().props.insert(k, Prop::data(v));
                    }
                    None => {
                        it.call(&initf, fv.clone(), &[])?;
                    }
                }
            }
            Ok(fv)
        }))
    }
}

// ---------------------------------------------------------------- running compiled code

fn new_frame(cf: &CFunc, c: &Closure, func: Option<Obj>, this: Value, nt: Value, args: Vec<Value>) -> Frame {
    Frame {
        slots: vec![Value::Undefined; cf.nslots],
        cells: (0..cf.ncells).map(|_| cell(Value::Undefined)).collect(),
        caps: c.caps.clone(),
        this,
        func,
        new_target: nt,
        home: c.home.borrow().clone(),
        env: c.env.clone(),
        args,
    }
}

fn run_body(it: &mut Interp, cf: &CFunc, f: &mut Frame) -> R<Value> {
    match (cf.body)(it, f)? {
        Flow::Return(v) => Ok(v),
        _ => Ok(Value::Undefined),
    }
}

impl Interp {
    pub(crate) fn make_compiled_function(&mut self, def: Rc<FuncDef>, cf: Rc<CFunc>, caps: Rc<[Cell]>, env: Env, home: Option<Obj>) -> Value {
        let c = Rc::new(Closure { def, env, home: RefCell::new(home), class: None, compiled: Some(cf), caps });
        self.function_object(c)
    }

    /// Call a compiled function (generators and async functions run on coroutines).
    pub(crate) fn call_compiled(&mut self, c: &Rc<Closure>, cf: &Rc<CFunc>, fo: &Obj, this: Value, args: &[Value], nt: Value) -> R<Value> {
        let def = &c.def;
        let this = if def.is_arrow {
            Value::Undefined
        } else if this.is_nullish() && !def.is_method {
            Value::Obj(self.global.clone())
        } else {
            this
        };
        let mut frame = new_frame(cf, c, Some(fo.clone()), this, nt, args.to_vec());
        if def.is_generator {
            (cf.prologue)(self, &mut frame)?;
            let cf = cf.clone();
            let body: Box<dyn FnOnce(&mut Interp) -> R<Value>> = Box::new(move |it: &mut Interp| run_body(it, &cf, &mut frame));
            return Ok(self.new_generator(body));
        }
        if def.is_async {
            let cf = cf.clone();
            let body: Box<dyn FnOnce(&mut Interp) -> R<Value>> = Box::new(move |it: &mut Interp| {
                (cf.prologue)(it, &mut frame)?;
                run_body(it, &cf, &mut frame)
            });
            return Ok(self.new_async(body));
        }
        (cf.prologue)(self, &mut frame)?;
        run_body(self, cf, &mut frame)
    }

    /// Run a compiled class constructor; `this` is `None` in derived classes until `super()`.
    pub(crate) fn construct_compiled(&mut self, c: &Rc<Closure>, cf: &Rc<CFunc>, fo: &Obj, this: Option<Value>, args: &[Value], nt: Value) -> R<Value> {
        let this = this.unwrap_or_else(|| Value::Obj(self.uninit.clone()));
        let mut frame = new_frame(cf, c, Some(fo.clone()), this, nt, args.to_vec());
        (cf.prologue)(self, &mut frame)?;
        let r = run_body(self, cf, &mut frame)?;
        if let Value::Obj(_) = r {
            return Ok(r);
        }
        let t = read_loc(self, &mut frame, &cf.this_loc)?;
        if matches!(&t, Value::Obj(o) if o.ptr_eq(&self.uninit)) {
            throw!(self, "ReferenceError", "Must call super constructor in derived class before accessing 'this' or returning from derived constructor");
        }
        Ok(t)
    }

    /// Run a compiled script's top level.
    pub(crate) fn run_compiled_script(&mut self, cf: &Rc<CFunc>, env: Env, this: Value) -> R<Flow> {
        let mut frame = Frame {
            slots: vec![Value::Undefined; cf.nslots],
            cells: (0..cf.ncells).map(|_| cell(Value::Undefined)).collect(),
            caps: Rc::new([]),
            this,
            func: None,
            new_target: Value::Undefined,
            home: None,
            env,
            args: vec![],
        };
        (cf.prologue)(self, &mut frame)?;
        (cf.body)(self, &mut frame)
    }
}

/// Compile a script, or explain why it can't be (the caller then interprets it).
pub fn compile_script(stmts: &[Stmt]) -> Result<Rc<CFunc>, Unsupported> {
    Compiler::compile_script(stmts, false)
}
