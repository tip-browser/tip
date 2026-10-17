//! Syntax tree for the Rust subset.

use std::rc::Rc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pos {
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    /// A path like `Vec<i32>` or `std::collections::HashMap<K, V>`; segments plus generic args.
    Path(Vec<String>, Vec<Type>),
    Ref(bool, Box<Type>),
    Tuple(Vec<Type>),
    Array(Box<Type>, Option<Box<Expr>>),
    Fn(Vec<Type>, Box<Type>),
    /// `dyn Trait` / `impl Trait`
    Trait(String),
    Infer,
    Never,
}

impl Type {
    pub fn unit() -> Type {
        Type::Tuple(vec![])
    }
    /// The last path segment, e.g. `HashMap` for `std::collections::HashMap<K, V>`.
    pub fn name(&self) -> &str {
        match self {
            Type::Path(p, _) => p.last().map(|s| s.as_str()).unwrap_or(""),
            Type::Ref(_, t) => t.name(),
            _ => "",
        }
    }
    pub fn args(&self) -> &[Type] {
        match self {
            Type::Path(_, a) => a,
            Type::Ref(_, t) => t.args(),
            _ => &[],
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Lit {
    Int(u128, Option<String>),
    Float(f64, Option<String>),
    Str(String),
    ByteStr(Vec<u8>),
    Char(char),
    Byte(u8),
    Bool(bool),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pat {
    Wild,
    /// `name`, `mut name`, `ref name`, `name @ subpattern`
    Bind { name: String, mutable: bool, sub: Option<Box<Pat>> },
    Lit(Lit, bool /* negated */),
    Range(Box<Pat>, Box<Pat>, bool /* inclusive */),
    Tuple(Vec<Pat>),
    /// `Some(x)`, `Color::Rgb(r, g, b)`, `Point(x, y)`
    TupleStruct(Vec<String>, Vec<Pat>),
    /// `Point { x, y: py, .. }`
    Struct(Vec<String>, Vec<(String, Pat)>, bool),
    /// `None`, `Color::Red`, constants
    Path(Vec<String>),
    Slice(Vec<Pat>),
    /// `..` inside tuple/slice patterns
    Rest,
    Ref(Box<Pat>),
    Or(Vec<Pat>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    Deref,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub pos: Pos,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Lit(Lit),
    Path(Vec<String>, Vec<Type>),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Assign(Box<Expr>, Box<Expr>),
    AssignOp(BinOp, Box<Expr>, Box<Expr>),
    Ref(bool, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>),
    MethodCall(Box<Expr>, String, Vec<Type>, Vec<Expr>),
    Field(Box<Expr>, String),
    TupleField(Box<Expr>, usize),
    Index(Box<Expr>, Box<Expr>),
    Tuple(Vec<Expr>),
    Array(Vec<Expr>),
    ArrayRepeat(Box<Expr>, Box<Expr>),
    StructLit(Vec<String>, Vec<(String, Expr)>, Option<Box<Expr>>),
    Range(Option<Box<Expr>>, Option<Box<Expr>>, bool),
    Block(Rc<Block>),
    If(Box<Expr>, Rc<Block>, Option<Box<Expr>>),
    /// `if let PAT = EXPR { } else { }`
    IfLet(Pat, Box<Expr>, Rc<Block>, Option<Box<Expr>>),
    While(Option<String>, Box<Expr>, Rc<Block>),
    WhileLet(Option<String>, Pat, Box<Expr>, Rc<Block>),
    Loop(Option<String>, Rc<Block>),
    For(Option<String>, Pat, Box<Expr>, Rc<Block>),
    Match(Box<Expr>, Vec<Arm>),
    Closure(Rc<Closure>),
    Return(Option<Box<Expr>>),
    Break(Option<String>, Option<Box<Expr>>),
    Continue(Option<String>),
    Try(Box<Expr>),
    Cast(Box<Expr>, Type),
    Macro(String, Vec<Expr>),
    /// `format!`-style macro with a literal format string.
    FormatMacro(String, String, Vec<Expr>, Vec<(String, Expr)>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Arm {
    pub pat: Pat,
    pub guard: Option<Expr>,
    pub body: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Closure {
    pub params: Vec<(Pat, Option<Type>)>,
    pub ret: Option<Type>,
    pub body: Expr,
    pub is_move: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Let(Pat, Option<Type>, Option<Expr>, Option<Rc<Block>>, Pos),
    Expr(Expr, bool /* has semicolon */),
    Item(Item),
    Empty,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub pos: Pos,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FnDecl {
    pub name: String,
    pub params: Vec<(Pat, Type)>,
    /// Methods: the receiver kind (`self`, `&self`, `&mut self`).
    pub self_param: Option<SelfKind>,
    pub ret: Option<Type>,
    pub body: Option<Rc<Block>>,
    pub export: bool,
    pub pos: Pos,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfKind {
    Value,
    Ref,
    RefMut,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StructKind {
    Named(Vec<(String, Type)>),
    Tuple(Vec<Type>),
    Unit,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    pub name: String,
    pub kind: StructKind,
    pub discriminant: Option<i128>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Fn(Rc<FnDecl>),
    Struct { name: String, kind: StructKind, derives: Vec<String>, pos: Pos },
    Enum { name: String, variants: Vec<Variant>, derives: Vec<String>, pos: Pos },
    Impl { ty: String, trait_name: Option<String>, items: Vec<Item>, pos: Pos },
    Trait { name: String, items: Vec<Item>, pos: Pos },
    /// `value` is `None` for a trait's required associated const.
    Const { name: String, ty: Type, value: Option<Expr>, pos: Pos },
    Use(Vec<String>),
    Mod { name: String, items: Vec<Item> },
    TypeAlias { name: String, ty: Type },
}
