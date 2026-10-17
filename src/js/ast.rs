//! JavaScript abstract syntax tree.

use std::rc::Rc;

pub type Name = Rc<str>;

#[derive(Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub line: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclKind {
    Var,
    Let,
    Const,
}

#[derive(Debug)]
pub enum ForInit {
    Decl(DeclKind, Vec<(Pattern, Option<Expr>)>),
    Expr(Expr),
}

#[derive(Debug)]
pub enum ForTarget {
    Decl(DeclKind, Pattern),
    Pattern(Pattern),
}

#[derive(Debug)]
pub struct SwitchCase {
    pub test: Option<Expr>,
    pub body: Vec<Stmt>,
}

#[derive(Debug)]
pub enum StmtKind {
    Expr(Expr),
    Decl(DeclKind, Vec<(Pattern, Option<Expr>)>),
    Function(Rc<FuncDef>),
    Class(Rc<ClassDef>),
    Return(Option<Expr>),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    Block(Block),
    For { init: Option<ForInit>, test: Option<Expr>, update: Option<Expr>, body: Box<Stmt>, per_iteration: Vec<Name> },
    ForIn { target: ForTarget, obj: Expr, body: Box<Stmt> },
    ForOf { target: ForTarget, iter: Expr, body: Box<Stmt>, is_await: bool },
    While(Expr, Box<Stmt>),
    DoWhile(Box<Stmt>, Expr),
    Break(Option<Name>),
    Continue(Option<Name>),
    Throw(Expr),
    Try { block: Block, param: Option<Pattern>, handler: Option<Block>, finalizer: Option<Block> },
    Switch(Expr, Vec<SwitchCase>, bool),
    Labeled(Name, Box<Stmt>),
    Empty,
    Debugger,
}

#[derive(Debug)]
pub struct Block {
    pub body: Vec<Stmt>,
    /// Whether this block declares let/const/class/function bindings and needs its own scope.
    pub scoped: bool,
}

#[derive(Debug, Clone)]
pub enum Key {
    Name(Name),
    Computed(Box<Expr>),
    Private(Name),
}

#[derive(Debug, Clone)]
pub enum Pattern {
    Ident(Name),
    Object { props: Vec<(Key, Pattern, Option<Expr>)>, rest: Option<Box<Pattern>> },
    Array { elems: Vec<Option<(Pattern, Option<Expr>)>>, rest: Option<Box<Pattern>> },
    /// Assignment target such as `a.b` or `a[0]` (only in assignment patterns).
    Expr(Box<Expr>),
}

#[derive(Debug, Clone)]
pub struct Param {
    pub pattern: Pattern,
    pub default: Option<Expr>,
}

#[derive(Debug)]
pub enum FuncBody {
    Block(Vec<Stmt>),
    Expr(Expr),
}

#[derive(Debug)]
pub struct FuncDef {
    pub name: Option<Name>,
    pub params: Vec<Param>,
    pub rest: Option<Pattern>,
    pub body: FuncBody,
    pub is_arrow: bool,
    pub is_async: bool,
    pub is_generator: bool,
    pub is_method: bool,
    /// Hoisted `var` names declared anywhere in the body (not in nested functions).
    pub var_names: Vec<Name>,
    pub uses_arguments: bool,
    pub uses_this: bool,
    pub simple_params: bool,
    pub line: u32,
    pub source: Rc<str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodKind {
    Method,
    Get,
    Set,
}

#[derive(Debug)]
pub enum ClassMember {
    Method { key: Key, kind: MethodKind, is_static: bool, func: Rc<FuncDef> },
    Field { key: Key, is_static: bool, value: Option<Expr> },
    StaticBlock(Vec<Stmt>),
}

#[derive(Debug)]
pub struct ClassDef {
    pub name: Option<Name>,
    pub superclass: Option<Expr>,
    pub constructor: Option<Rc<FuncDef>>,
    pub members: Vec<ClassMember>,
}

#[derive(Debug, Clone)]
pub enum ArrayElem {
    Hole,
    Item(Expr),
    Spread(Expr),
}

#[derive(Debug, Clone)]
pub enum ObjProp {
    KeyValue(Key, Expr),
    Shorthand(Name),
    Method(Key, MethodKind, Rc<FuncDef>),
    Spread(Expr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Exp,
    Eq,
    Ne,
    StrictEq,
    StrictNe,
    Lt,
    Gt,
    Le,
    Ge,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    UShr,
    In,
    InstanceOf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogicOp {
    And,
    Or,
    Nullish,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Plus,
    Not,
    BitNot,
    TypeOf,
    Void,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOp {
    Assign,
    Op(BinOp),
    Logic(LogicOp),
}

#[derive(Debug, Clone)]
pub enum Expr {
    Num(f64),
    Str(Name),
    BigInt(Name),
    Template { quasis: Vec<Name>, exprs: Vec<Expr> },
    TaggedTemplate { tag: Box<Expr>, cooked: Vec<Name>, raw: Vec<Name>, exprs: Vec<Expr> },
    Regex { pattern: Name, flags: Name },
    Ident(Name),
    This,
    Super,
    NewTarget,
    Null,
    Bool(bool),
    Array(Vec<ArrayElem>),
    Object(Vec<ObjProp>),
    Function(Rc<FuncDef>),
    Class(Rc<ClassDef>),
    Unary(UnaryOp, Box<Expr>),
    Update { inc: bool, prefix: bool, target: Box<Expr> },
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Logical(LogicOp, Box<Expr>, Box<Expr>),
    Assign(AssignOp, Box<Pattern>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    Call { callee: Box<Expr>, args: Vec<ArrayElem>, optional: bool },
    New { callee: Box<Expr>, args: Vec<ArrayElem> },
    Member { obj: Box<Expr>, prop: Box<Key>, optional: bool },
    OptChain(Box<Expr>),
    Seq(Vec<Expr>),
    Await(Box<Expr>),
    Yield { arg: Option<Box<Expr>>, delegate: bool },
    PrivateIn(Name, Box<Expr>),
}

/// Static import/export metadata collected while parsing a module.
#[derive(Debug, Default, Clone)]
pub struct ModuleInfo {
    /// Module specifiers in source order (duplicates removed).
    pub requests: Vec<Name>,
    pub imports: Vec<ImportEntry>,
    pub exports: Vec<ExportEntry>,
    /// `export * from "x"` specifiers.
    pub star_exports: Vec<Name>,
}

#[derive(Debug, Clone)]
pub struct ImportEntry {
    pub specifier: Name,
    /// Imported name; `None` for a namespace import (`* as ns`).
    pub import: Option<Name>,
    pub local: Name,
}

#[derive(Debug, Clone)]
pub enum ExportEntry {
    Local { exported: Name, local: Name },
    /// Re-export from another module; `import: None` is `export * as ns from`.
    Indirect { exported: Name, specifier: Name, import: Option<Name> },
}

/// Hidden binding that holds the value of `export default <expr>`.
pub const DEFAULT_EXPORT_BINDING: &str = "*default*";
