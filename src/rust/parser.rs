//! Parser for the Rust subset.

use std::rc::Rc;

use super::ast::*;
use super::lexer::{Tok, Token};

pub struct CompileError {
    pub msg: String,
    pub pos: Pos,
}

type PResult<T> = Result<T, CompileError>;

pub struct Parser {
    toks: Vec<Token>,
    pos: usize,
    no_struct: bool,
}

const KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "crate", "else", "enum", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "static",
    "struct", "trait", "true", "type", "unsafe", "use", "where", "while", "dyn",
];

pub fn parse_file(toks: Vec<Token>) -> PResult<Vec<Item>> {
    let mut p = Parser { toks, pos: 0, no_struct: false };
    let mut items = Vec::new();
    while !p.at_eof() {
        if let Some(it) = p.item()? {
            items.push(it);
        }
    }
    Ok(items)
}

impl Parser {
    // ------------------------------------------------------------ token helpers

    fn peek(&self) -> &Tok {
        &self.toks[self.pos].tok
    }
    fn peek_at(&self, n: usize) -> &Tok {
        self.toks.get(self.pos + n).map(|t| &t.tok).unwrap_or(&Tok::Eof)
    }
    fn here(&self) -> Pos {
        let t = &self.toks[self.pos];
        Pos { line: t.line, col: t.col }
    }
    fn at_eof(&self) -> bool {
        matches!(self.peek(), Tok::Eof)
    }
    fn advance(&mut self) -> Tok {
        let t = self.toks[self.pos].tok.clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }
    fn is(&self, p: &str) -> bool {
        matches!(self.peek(), Tok::Punct(x) if *x == p)
    }
    fn is_kw(&self, k: &str) -> bool {
        matches!(self.peek(), Tok::Ident(x) if x == k)
    }
    fn eat(&mut self, p: &str) -> bool {
        if self.is(p) {
            self.advance();
            true
        } else {
            false
        }
    }
    fn eat_kw(&mut self, k: &str) -> bool {
        if self.is_kw(k) {
            self.advance();
            true
        } else {
            false
        }
    }
    fn err<T>(&self, msg: impl Into<String>) -> PResult<T> {
        Err(CompileError { msg: msg.into(), pos: self.here() })
    }
    fn describe(&self) -> String {
        match self.peek() {
            Tok::Ident(s) => format!("`{s}`"),
            Tok::Punct(p) => format!("`{p}`"),
            Tok::Int(v, _) => format!("`{v}`"),
            Tok::Float(v, _) => format!("`{v}`"),
            Tok::Str(_) | Tok::ByteStr(_) => "string literal".into(),
            Tok::Char(c) => format!("`'{c}'`"),
            Tok::Byte(_) => "byte literal".into(),
            Tok::Lifetime(l) => format!("`'{l}`"),
            Tok::Eof => "end of file".into(),
        }
    }
    fn expect(&mut self, p: &str) -> PResult<()> {
        if self.eat(p) {
            Ok(())
        } else {
            self.err(format!("expected `{p}`, found {}", self.describe()))
        }
    }
    fn expect_kw(&mut self, k: &str) -> PResult<()> {
        if self.eat_kw(k) {
            Ok(())
        } else {
            self.err(format!("expected `{k}`, found {}", self.describe()))
        }
    }
    fn ident(&mut self) -> PResult<String> {
        match self.peek().clone() {
            Tok::Ident(s) if !KEYWORDS.contains(&s.as_str()) || s == "crate" => {
                self.advance();
                Ok(s)
            }
            _ => self.err(format!("expected identifier, found {}", self.describe())),
        }
    }
    /// Consume a `>` closing generic arguments, splitting `>>`, `>=` and `>>=`.
    fn eat_gt(&mut self) -> PResult<()> {
        let rest = match self.peek() {
            Tok::Punct(">") => None,
            Tok::Punct(">>") => Some(">"),
            Tok::Punct(">=") => Some("="),
            Tok::Punct(">>=") => Some(">="),
            _ => return self.err(format!("expected `>`, found {}", self.describe())),
        };
        match rest {
            None => {
                self.advance();
            }
            Some(r) => {
                self.toks[self.pos].tok = Tok::Punct(r);
                self.toks[self.pos].col += 1;
            }
        }
        Ok(())
    }

    /// Skip balanced `<...>` generic parameter lists (on items).
    fn skip_generics(&mut self) -> PResult<()> {
        if !self.is("<") {
            return Ok(());
        }
        let mut depth = 0i32;
        loop {
            match self.peek() {
                Tok::Punct("<") => depth += 1,
                Tok::Punct(">") => depth -= 1,
                Tok::Punct(">>") => depth -= 2,
                Tok::Punct("->") => {}
                Tok::Eof => return self.err("unterminated generic parameters"),
                _ => {}
            }
            self.advance();
            if depth <= 0 {
                return Ok(());
            }
        }
    }

    fn skip_where(&mut self) {
        if self.eat_kw("where") {
            while !self.at_eof() && !self.is("{") && !self.is(";") {
                self.advance();
            }
        }
    }

    // ------------------------------------------------------------ items

    fn attributes(&mut self) -> PResult<(Vec<String>, bool)> {
        let mut derives = Vec::new();
        let mut export = false;
        while self.is("#") {
            self.advance();
            self.eat("!");
            self.expect("[")?;
            let name = self.ident().unwrap_or_default();
            if name == "derive" && self.eat("(") {
                while !self.is(")") && !self.at_eof() {
                    if let Tok::Ident(d) = self.advance() {
                        derives.push(d);
                    }
                }
                self.expect(")")?;
            } else {
                if matches!(name.as_str(), "export" | "wasm_bindgen" | "no_mangle") {
                    export = true;
                }
                let mut depth = 0;
                while !self.at_eof() && !(depth == 0 && self.is("]")) {
                    match self.advance() {
                        Tok::Punct("[") | Tok::Punct("(") => depth += 1,
                        Tok::Punct("]") | Tok::Punct(")") => depth -= 1,
                        _ => {}
                    }
                }
            }
            self.expect("]")?;
        }
        Ok((derives, export))
    }

    fn visibility(&mut self) {
        if self.eat_kw("pub") && self.is("(") {
            while !self.at_eof() && !self.is(")") {
                self.advance();
            }
            self.advance();
        }
    }

    fn item(&mut self) -> PResult<Option<Item>> {
        let (derives, export) = self.attributes()?;
        if self.at_eof() {
            return Ok(None);
        }
        self.visibility();
        let pos = self.here();
        if self.is_kw("const") && matches!(self.peek_at(1), Tok::Ident(f) if f == "fn") {
            self.advance();
        }
        self.eat_kw("unsafe");
        self.eat_kw("async");
        if self.is_kw("extern") {
            self.advance();
            if matches!(self.peek(), Tok::Str(_)) {
                self.advance();
            }
        }
        match self.peek().clone() {
            Tok::Ident(k) if k == "fn" => Ok(Some(Item::Fn(Rc::new(self.fn_decl(export)?)))),
            Tok::Ident(k) if k == "struct" => {
                self.advance();
                let name = self.ident()?;
                self.skip_generics()?;
                self.skip_where();
                let kind = if self.eat(";") {
                    StructKind::Unit
                } else if self.is("(") {
                    let k = StructKind::Tuple(self.tuple_fields()?);
                    self.skip_where();
                    self.expect(";")?;
                    k
                } else {
                    StructKind::Named(self.named_fields()?)
                };
                Ok(Some(Item::Struct { name, kind, derives, pos }))
            }
            Tok::Ident(k) if k == "enum" => {
                self.advance();
                let name = self.ident()?;
                self.skip_generics()?;
                self.skip_where();
                self.expect("{")?;
                let mut variants = Vec::new();
                while !self.eat("}") {
                    self.attributes()?;
                    let vname = self.ident()?;
                    let kind = if self.is("(") {
                        StructKind::Tuple(self.tuple_fields()?)
                    } else if self.is("{") {
                        StructKind::Named(self.named_fields()?)
                    } else {
                        StructKind::Unit
                    };
                    let mut discriminant = None;
                    if self.eat("=") {
                        let neg = self.eat("-");
                        match self.advance() {
                            Tok::Int(v, _) => discriminant = Some(if neg { -(v as i128) } else { v as i128 }),
                            _ => return self.err("expected integer discriminant"),
                        }
                    }
                    variants.push(Variant { name: vname, kind, discriminant });
                    if !self.eat(",") {
                        self.expect("}")?;
                        break;
                    }
                }
                Ok(Some(Item::Enum { name, variants, derives, pos }))
            }
            Tok::Ident(k) if k == "impl" => {
                self.advance();
                self.skip_generics()?;
                let first = self.ty()?;
                let (ty, trait_name) = if self.eat_kw("for") {
                    let t = self.ty()?;
                    (t.name().to_string(), Some(first.name().to_string()))
                } else {
                    (first.name().to_string(), None)
                };
                self.skip_where();
                self.expect("{")?;
                let mut items = Vec::new();
                while !self.eat("}") {
                    if self.at_eof() {
                        return self.err("unterminated impl block");
                    }
                    if let Some(it) = self.item()? {
                        items.push(it);
                    }
                }
                Ok(Some(Item::Impl { ty, trait_name, items, pos }))
            }
            Tok::Ident(k) if k == "trait" => {
                self.advance();
                let name = self.ident()?;
                self.skip_generics()?;
                if self.eat(":") {
                    while !self.is("{") && !self.at_eof() && !self.is_kw("where") {
                        self.advance();
                    }
                }
                self.skip_where();
                self.expect("{")?;
                let mut items = Vec::new();
                while !self.eat("}") {
                    if self.at_eof() {
                        return self.err("unterminated trait");
                    }
                    if self.is_kw("type") {
                        while !self.eat(";") {
                            self.advance();
                        }
                        continue;
                    }
                    if let Some(it) = self.item()? {
                        items.push(it);
                    }
                }
                Ok(Some(Item::Trait { name, items, pos }))
            }
            Tok::Ident(k) if k == "const" || k == "static" => {
                self.advance();
                self.eat_kw("mut");
                let name = self.ident()?;
                self.expect(":")?;
                let ty = self.ty()?;
                if self.eat(";") {
                    return Ok(Some(Item::Const { name, ty, value: None, pos }));
                }
                self.expect("=")?;
                let value = self.expr()?;
                self.expect(";")?;
                Ok(Some(Item::Const { name, ty, value: Some(value), pos }))
            }
            Tok::Ident(k) if k == "use" => {
                self.advance();
                let mut path = Vec::new();
                while !self.eat(";") {
                    if self.at_eof() {
                        return self.err("expected `;`");
                    }
                    if let Tok::Ident(s) = self.advance() {
                        path.push(s);
                    }
                }
                Ok(Some(Item::Use(path)))
            }
            Tok::Ident(k) if k == "mod" => {
                self.advance();
                let name = self.ident()?;
                if self.eat(";") {
                    return Ok(Some(Item::Mod { name, items: vec![] }));
                }
                self.expect("{")?;
                let mut items = Vec::new();
                while !self.eat("}") {
                    if self.at_eof() {
                        return self.err("unterminated module");
                    }
                    if let Some(it) = self.item()? {
                        items.push(it);
                    }
                }
                Ok(Some(Item::Mod { name, items }))
            }
            Tok::Ident(k) if k == "type" => {
                self.advance();
                let name = self.ident()?;
                self.skip_generics()?;
                self.expect("=")?;
                let ty = self.ty()?;
                self.expect(";")?;
                Ok(Some(Item::TypeAlias { name, ty }))
            }
            Tok::Ident(k) if k == "macro_rules" => self.err("macro_rules! is not supported"),
            Tok::Punct(";") => {
                self.advance();
                Ok(None)
            }
            _ => self.err(format!("expected item, found {}", self.describe())),
        }
    }

    fn tuple_fields(&mut self) -> PResult<Vec<Type>> {
        self.expect("(")?;
        let mut fields = Vec::new();
        while !self.eat(")") {
            self.attributes()?;
            self.visibility();
            fields.push(self.ty()?);
            if !self.eat(",") {
                self.expect(")")?;
                break;
            }
        }
        Ok(fields)
    }

    fn named_fields(&mut self) -> PResult<Vec<(String, Type)>> {
        self.expect("{")?;
        let mut fields = Vec::new();
        while !self.eat("}") {
            self.attributes()?;
            self.visibility();
            let n = self.ident()?;
            self.expect(":")?;
            fields.push((n, self.ty()?));
            if !self.eat(",") {
                self.expect("}")?;
                break;
            }
        }
        Ok(fields)
    }

    fn fn_decl(&mut self, export: bool) -> PResult<FnDecl> {
        let pos = self.here();
        self.expect_kw("fn")?;
        let name = self.ident()?;
        self.skip_generics()?;
        self.expect("(")?;
        let mut params = Vec::new();
        let mut self_param = None;
        // receiver
        let save = self.pos;
        let mut is_ref = false;
        let mut is_mut_ref = false;
        if self.eat("&") {
            is_ref = true;
            if matches!(self.peek(), Tok::Lifetime(_)) {
                self.advance();
            }
            is_mut_ref = self.eat_kw("mut");
        }
        let mut_self = !is_ref && self.is_kw("mut") && matches!(self.peek_at(1), Tok::Ident(s) if s == "self");
        if mut_self {
            self.advance();
        }
        if self.eat_kw("self") {
            self_param = Some(if is_mut_ref { SelfKind::RefMut } else if is_ref { SelfKind::Ref } else { SelfKind::Value });
            if self.eat(":") {
                let t = self.ty()?;
                if let Type::Ref(m, _) = t {
                    self_param = Some(if m { SelfKind::RefMut } else { SelfKind::Ref });
                }
            }
            if !self.eat(",") {
                self.expect(")")?;
                return self.fn_rest(name, params, self_param, export, pos);
            }
        } else {
            self.pos = save;
        }
        while !self.eat(")") {
            self.attributes()?;
            let pat = self.pattern()?;
            self.expect(":")?;
            let ty = self.ty()?;
            params.push((pat, ty));
            if !self.eat(",") {
                self.expect(")")?;
                break;
            }
        }
        self.fn_rest(name, params, self_param, export, pos)
    }

    fn fn_rest(&mut self, name: String, params: Vec<(Pat, Type)>, self_param: Option<SelfKind>, export: bool, pos: Pos) -> PResult<FnDecl> {
        let ret = if self.eat("->") { Some(self.ty()?) } else { None };
        self.skip_where();
        let body = if self.eat(";") { None } else { Some(Rc::new(self.block()?)) };
        Ok(FnDecl { name, params, self_param, ret, body, export, pos })
    }

    // ------------------------------------------------------------ types

    pub fn ty(&mut self) -> PResult<Type> {
        if self.eat("&") || {
            // `&&T`
            if self.is("&&") {
                self.toks[self.pos].tok = Tok::Punct("&");
                true
            } else {
                false
            }
        } {
            if matches!(self.peek(), Tok::Lifetime(_)) {
                self.advance();
            }
            let m = self.eat_kw("mut");
            return Ok(Type::Ref(m, Box::new(self.ty()?)));
        }
        if self.eat("*") {
            if !self.eat_kw("const") {
                self.eat_kw("mut");
            }
            return Ok(Type::Ref(true, Box::new(self.ty()?)));
        }
        if self.eat("(") {
            let mut items = Vec::new();
            while !self.eat(")") {
                items.push(self.ty()?);
                if !self.eat(",") {
                    self.expect(")")?;
                    break;
                }
            }
            return Ok(if items.len() == 1 { items.pop().unwrap() } else { Type::Tuple(items) });
        }
        if self.eat("[") {
            let t = self.ty()?;
            let n = if self.eat(";") { Some(Box::new(self.expr()?)) } else { None };
            self.expect("]")?;
            return Ok(Type::Array(Box::new(t), n));
        }
        if self.eat("!") {
            return Ok(Type::Never);
        }
        if self.eat_kw("_") {
            return Ok(Type::Infer);
        }
        if self.is_kw("dyn") || self.is_kw("impl") {
            self.advance();
            let t = self.ty()?;
            // `+ Send + 'static` bounds
            while self.eat("+") {
                if matches!(self.peek(), Tok::Lifetime(_)) {
                    self.advance();
                } else {
                    self.ty()?;
                }
            }
            return Ok(Type::Trait(t.name().to_string()));
        }
        if self.is_kw("fn") {
            self.advance();
            self.expect("(")?;
            let mut args = Vec::new();
            while !self.eat(")") {
                args.push(self.ty()?);
                if !self.eat(",") {
                    self.expect(")")?;
                    break;
                }
            }
            let ret = if self.eat("->") { self.ty()? } else { Type::unit() };
            return Ok(Type::Fn(args, Box::new(ret)));
        }
        let mut path = Vec::new();
        let mut args = Vec::new();
        loop {
            let seg = match self.advance() {
                Tok::Ident(s) => s,
                _ => {
                    self.pos -= 1;
                    return self.err(format!("expected type, found {}", self.describe()));
                }
            };
            // Fn(A, B) -> C sugar
            if matches!(seg.as_str(), "Fn" | "FnMut" | "FnOnce") && self.is("(") {
                self.advance();
                let mut a = Vec::new();
                while !self.eat(")") {
                    a.push(self.ty()?);
                    if !self.eat(",") {
                        self.expect(")")?;
                        break;
                    }
                }
                let ret = if self.eat("->") { self.ty()? } else { Type::unit() };
                return Ok(Type::Fn(a, Box::new(ret)));
            }
            path.push(seg);
            if self.is("<") {
                self.advance();
                while !self.is(">") && !self.is(">>") && !self.is(">=") && !self.is(">>=") {
                    if matches!(self.peek(), Tok::Lifetime(_)) {
                        self.advance();
                    } else if matches!(self.peek(), Tok::Ident(_)) && matches!(self.peek_at(1), Tok::Punct("=")) {
                        // associated type binding `Item = T`
                        self.advance();
                        self.advance();
                        args.push(self.ty()?);
                    } else {
                        args.push(self.ty()?);
                    }
                    if !self.eat(",") {
                        break;
                    }
                }
                self.eat_gt()?;
            }
            if self.is("::") && matches!(self.peek_at(1), Tok::Ident(_)) {
                self.advance();
                continue;
            }
            break;
        }
        Ok(Type::Path(path, args))
    }

    // ------------------------------------------------------------ statements

    pub fn block(&mut self) -> PResult<Block> {
        let pos = self.here();
        self.expect("{")?;
        let saved = self.no_struct;
        self.no_struct = false;
        let mut stmts = Vec::new();
        while !self.eat("}") {
            if self.at_eof() {
                return self.err("this file contains an unclosed delimiter");
            }
            stmts.push(self.stmt()?);
        }
        self.no_struct = saved;
        Ok(Block { stmts, pos })
    }

    fn is_item_start(&self) -> bool {
        match self.peek() {
            Tok::Punct("#") => true,
            Tok::Ident(k) => {
                matches!(k.as_str(), "fn" | "struct" | "enum" | "impl" | "trait" | "use" | "mod" | "pub")
                    || (k == "const" && !matches!(self.peek_at(1), Tok::Punct("{")))
                    || (k == "static" && matches!(self.peek_at(1), Tok::Ident(_)))
            }
            _ => false,
        }
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        if self.eat(";") {
            return Ok(Stmt::Empty);
        }
        if self.is_item_start() {
            return Ok(match self.item()? {
                Some(it) => Stmt::Item(it),
                None => Stmt::Empty,
            });
        }
        if self.is_kw("let") {
            let pos = self.here();
            self.advance();
            let pat = self.pattern()?;
            let ty = if self.eat(":") { Some(self.ty()?) } else { None };
            let init = if self.eat("=") { Some(self.expr()?) } else { None };
            let els = if self.eat_kw("else") { Some(Rc::new(self.block()?)) } else { None };
            self.expect(";")?;
            return Ok(Stmt::Let(pat, ty, init, els, pos));
        }
        let block_like = matches!(self.peek(), Tok::Punct("{")) || matches!(self.peek(), Tok::Ident(k) if matches!(k.as_str(), "if" | "match" | "loop" | "while" | "for" | "unsafe")) || matches!(self.peek(), Tok::Lifetime(_));
        if block_like {
            let e = self.expr_block_like()?;
            // method chains / ? on a block-like expression continue it
            if self.is(".") || self.is("?") {
                let e = self.postfix_from(e)?;
                let e = self.binary_from(e, 0)?;
                let e = self.assign_from(e)?;
                let semi = self.eat(";");
                return Ok(Stmt::Expr(e, semi));
            }
            let semi = self.eat(";");
            return Ok(Stmt::Expr(e, semi));
        }
        let e = self.expr()?;
        if self.eat(";") {
            return Ok(Stmt::Expr(e, true));
        }
        if !self.is("}") {
            return self.err(format!("expected `;`, found {}", self.describe()));
        }
        Ok(Stmt::Expr(e, false))
    }

    // ------------------------------------------------------------ expressions

    pub fn expr(&mut self) -> PResult<Expr> {
        let lhs = self.range_expr()?;
        self.assign_from(lhs)
    }

    fn expr_no_struct(&mut self) -> PResult<Expr> {
        let saved = self.no_struct;
        self.no_struct = true;
        let e = self.expr();
        self.no_struct = saved;
        e
    }

    fn assign_from(&mut self, lhs: Expr) -> PResult<Expr> {
        let pos = lhs.pos;
        let op = match self.peek() {
            Tok::Punct("=") => None,
            Tok::Punct("+=") => Some(BinOp::Add),
            Tok::Punct("-=") => Some(BinOp::Sub),
            Tok::Punct("*=") => Some(BinOp::Mul),
            Tok::Punct("/=") => Some(BinOp::Div),
            Tok::Punct("%=") => Some(BinOp::Rem),
            Tok::Punct("&=") => Some(BinOp::BitAnd),
            Tok::Punct("|=") => Some(BinOp::BitOr),
            Tok::Punct("^=") => Some(BinOp::BitXor),
            Tok::Punct("<<=") => Some(BinOp::Shl),
            Tok::Punct(">>=") => Some(BinOp::Shr),
            _ => return Ok(lhs),
        };
        self.advance();
        let rhs = self.expr()?;
        Ok(Expr { kind: match op { None => ExprKind::Assign(Box::new(lhs), Box::new(rhs)), Some(o) => ExprKind::AssignOp(o, Box::new(lhs), Box::new(rhs)) }, pos })
    }

    fn range_expr(&mut self) -> PResult<Expr> {
        let pos = self.here();
        if self.is("..") || self.is("..=") {
            let incl = self.advance() == Tok::Punct("..=");
            let end = if self.range_end_follows() { Some(Box::new(self.binary(0)?)) } else { None };
            return Ok(Expr { kind: ExprKind::Range(None, end, incl), pos });
        }
        let lhs = self.binary(0)?;
        if self.is("..") || self.is("..=") {
            let incl = self.advance() == Tok::Punct("..=");
            let end = if self.range_end_follows() { Some(Box::new(self.binary(0)?)) } else { None };
            return Ok(Expr { kind: ExprKind::Range(Some(Box::new(lhs)), end, incl), pos });
        }
        Ok(lhs)
    }

    fn range_end_follows(&self) -> bool {
        !matches!(self.peek(), Tok::Punct(")") | Tok::Punct("]") | Tok::Punct("}") | Tok::Punct(",") | Tok::Punct(";") | Tok::Punct("=>") | Tok::Eof) && !(self.no_struct && self.is("{"))
    }

    fn binop(&self) -> Option<(BinOp, u8)> {
        Some(match self.peek() {
            Tok::Punct("||") => (BinOp::Or, 1),
            Tok::Punct("&&") => (BinOp::And, 2),
            Tok::Punct("==") => (BinOp::Eq, 3),
            Tok::Punct("!=") => (BinOp::Ne, 3),
            Tok::Punct("<") => (BinOp::Lt, 3),
            Tok::Punct("<=") => (BinOp::Le, 3),
            Tok::Punct(">") => (BinOp::Gt, 3),
            Tok::Punct(">=") => (BinOp::Ge, 3),
            Tok::Punct("|") => (BinOp::BitOr, 4),
            Tok::Punct("^") => (BinOp::BitXor, 5),
            Tok::Punct("&") => (BinOp::BitAnd, 6),
            Tok::Punct("<<") => (BinOp::Shl, 7),
            Tok::Punct(">>") => (BinOp::Shr, 7),
            Tok::Punct("+") => (BinOp::Add, 8),
            Tok::Punct("-") => (BinOp::Sub, 8),
            Tok::Punct("*") => (BinOp::Mul, 9),
            Tok::Punct("/") => (BinOp::Div, 9),
            Tok::Punct("%") => (BinOp::Rem, 9),
            _ => return None,
        })
    }

    fn binary(&mut self, min: u8) -> PResult<Expr> {
        let lhs = self.cast_expr()?;
        self.binary_from(lhs, min)
    }

    fn binary_from(&mut self, mut lhs: Expr, min: u8) -> PResult<Expr> {
        while let Some((op, prec)) = self.binop() {
            if prec < min.max(1) {
                break;
            }
            let pos = self.here();
            self.advance();
            let rhs = if prec == 3 { self.binary(prec + 1)? } else { self.binary(prec + 1)? };
            lhs = Expr { kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)), pos };
        }
        Ok(lhs)
    }

    fn cast_expr(&mut self) -> PResult<Expr> {
        let mut e = self.unary()?;
        while self.is_kw("as") {
            let pos = self.here();
            self.advance();
            let t = self.ty()?;
            e = Expr { kind: ExprKind::Cast(Box::new(e), t), pos };
        }
        Ok(e)
    }

    fn unary(&mut self) -> PResult<Expr> {
        let pos = self.here();
        if self.eat("-") {
            let e = self.unary()?;
            return Ok(Expr { kind: ExprKind::Unary(UnOp::Neg, Box::new(e)), pos });
        }
        if self.eat("!") {
            let e = self.unary()?;
            return Ok(Expr { kind: ExprKind::Unary(UnOp::Not, Box::new(e)), pos });
        }
        if self.eat("*") {
            let e = self.unary()?;
            return Ok(Expr { kind: ExprKind::Unary(UnOp::Deref, Box::new(e)), pos });
        }
        if self.is("&&") {
            self.advance();
            let m = self.eat_kw("mut");
            let e = self.unary()?;
            let inner = Expr { kind: ExprKind::Ref(m, Box::new(e)), pos };
            return Ok(Expr { kind: ExprKind::Ref(false, Box::new(inner)), pos });
        }
        if self.eat("&") {
            let m = self.eat_kw("mut");
            let e = self.unary()?;
            return Ok(Expr { kind: ExprKind::Ref(m, Box::new(e)), pos });
        }
        let e = self.primary()?;
        self.postfix_from(e)
    }

    fn call_args(&mut self, close: &str) -> PResult<Vec<Expr>> {
        let saved = self.no_struct;
        self.no_struct = false;
        let mut args = Vec::new();
        while !self.eat(close) {
            args.push(self.expr()?);
            if !self.eat(",") {
                self.expect(close)?;
                break;
            }
        }
        self.no_struct = saved;
        Ok(args)
    }

    fn postfix_from(&mut self, mut e: Expr) -> PResult<Expr> {
        loop {
            let pos = self.here();
            if self.eat("?") {
                e = Expr { kind: ExprKind::Try(Box::new(e)), pos };
            } else if self.is("(") {
                self.advance();
                let args = self.call_args(")")?;
                e = Expr { kind: ExprKind::Call(Box::new(e), args), pos };
            } else if self.is("[") {
                self.advance();
                let saved = self.no_struct;
                self.no_struct = false;
                let idx = self.expr()?;
                self.no_struct = saved;
                self.expect("]")?;
                e = Expr { kind: ExprKind::Index(Box::new(e), Box::new(idx)), pos };
            } else if self.is(".") {
                self.advance();
                match self.advance() {
                    Tok::Int(n, _) => e = Expr { kind: ExprKind::TupleField(Box::new(e), n as usize), pos },
                    Tok::Float(f, None) => {
                        // `t.0.1` lexes as `t` `.` `0.1`
                        let s = format!("{f}");
                        let mut parts = s.split('.');
                        let a: usize = parts.next().unwrap_or("0").parse().unwrap_or(0);
                        let b: usize = parts.next().unwrap_or("0").parse().unwrap_or(0);
                        e = Expr { kind: ExprKind::TupleField(Box::new(e), a), pos };
                        e = Expr { kind: ExprKind::TupleField(Box::new(e), b), pos };
                    }
                    Tok::Ident(name) if name == "await" => {}
                    Tok::Ident(name) => {
                        let mut generics = Vec::new();
                        if self.is("::") {
                            self.advance();
                            self.expect("<")?;
                            while !self.is(">") && !self.is(">>") {
                                generics.push(self.ty()?);
                                if !self.eat(",") {
                                    break;
                                }
                            }
                            self.eat_gt()?;
                        }
                        if self.is("(") {
                            self.advance();
                            let args = self.call_args(")")?;
                            e = Expr { kind: ExprKind::MethodCall(Box::new(e), name, generics, args), pos };
                        } else {
                            e = Expr { kind: ExprKind::Field(Box::new(e), name), pos };
                        }
                    }
                    _ => {
                        self.pos -= 1;
                        return self.err(format!("unexpected token: {}", self.describe()));
                    }
                }
            } else {
                return Ok(e);
            }
        }
    }

    fn expr_block_like(&mut self) -> PResult<Expr> {
        let pos = self.here();
        let label = if let Tok::Lifetime(l) = self.peek().clone() {
            self.advance();
            self.expect(":")?;
            Some(l)
        } else {
            None
        };
        match self.peek().clone() {
            Tok::Punct("{") => Ok(Expr { kind: ExprKind::Block(Rc::new(self.block()?)), pos }),
            Tok::Ident(k) if k == "unsafe" => {
                self.advance();
                Ok(Expr { kind: ExprKind::Block(Rc::new(self.block()?)), pos })
            }
            Tok::Ident(k) if k == "if" => self.if_expr(),
            Tok::Ident(k) if k == "match" => {
                self.advance();
                let scrut = self.expr_no_struct()?;
                self.expect("{")?;
                let mut arms = Vec::new();
                while !self.eat("}") {
                    self.attributes()?;
                    self.eat("|");
                    let pat = self.pattern()?;
                    let guard = if self.eat_kw("if") { Some(self.expr()?) } else { None };
                    self.expect("=>")?;
                    let body_is_block = self.is("{");
                    let body = self.expr()?;
                    let comma = self.eat(",");
                    if !comma && !body_is_block && !self.is("}") {
                        return self.err(format!("expected `,` following `match` arm, found {}", self.describe()));
                    }
                    arms.push(Arm { pat, guard, body });
                }
                Ok(Expr { kind: ExprKind::Match(Box::new(scrut), arms), pos })
            }
            Tok::Ident(k) if k == "loop" => {
                self.advance();
                Ok(Expr { kind: ExprKind::Loop(label, Rc::new(self.block()?)), pos })
            }
            Tok::Ident(k) if k == "while" => {
                self.advance();
                if self.eat_kw("let") {
                    let pat = self.pattern()?;
                    self.expect("=")?;
                    let e = self.expr_no_struct()?;
                    let body = Rc::new(self.block()?);
                    return Ok(Expr { kind: ExprKind::WhileLet(label, pat, Box::new(e), body), pos });
                }
                let cond = self.expr_no_struct()?;
                Ok(Expr { kind: ExprKind::While(label, Box::new(cond), Rc::new(self.block()?)), pos })
            }
            Tok::Ident(k) if k == "for" => {
                self.advance();
                let pat = self.pattern()?;
                self.expect_kw("in")?;
                let it = self.expr_no_struct()?;
                Ok(Expr { kind: ExprKind::For(label, pat, Box::new(it), Rc::new(self.block()?)), pos })
            }
            _ => self.err(format!("expected expression, found {}", self.describe())),
        }
    }

    fn if_expr(&mut self) -> PResult<Expr> {
        let pos = self.here();
        self.expect_kw("if")?;
        if self.eat_kw("let") {
            let pat = self.pattern()?;
            self.expect("=")?;
            let e = self.expr_no_struct()?;
            let then = Rc::new(self.block()?);
            let els = self.else_branch()?;
            return Ok(Expr { kind: ExprKind::IfLet(pat, Box::new(e), then, els), pos });
        }
        let cond = self.expr_no_struct()?;
        let then = Rc::new(self.block()?);
        let els = self.else_branch()?;
        Ok(Expr { kind: ExprKind::If(Box::new(cond), then, els), pos })
    }

    fn else_branch(&mut self) -> PResult<Option<Box<Expr>>> {
        if !self.eat_kw("else") {
            return Ok(None);
        }
        let pos = self.here();
        if self.is_kw("if") {
            return Ok(Some(Box::new(self.if_expr()?)));
        }
        Ok(Some(Box::new(Expr { kind: ExprKind::Block(Rc::new(self.block()?)), pos })))
    }

    fn primary(&mut self) -> PResult<Expr> {
        let pos = self.here();
        let lit = |k: Lit| Ok(Expr { kind: ExprKind::Lit(k), pos });
        match self.peek().clone() {
            Tok::Int(v, s) => {
                self.advance();
                lit(Lit::Int(v, s))
            }
            Tok::Float(v, s) => {
                self.advance();
                lit(Lit::Float(v, s))
            }
            Tok::Str(s) => {
                self.advance();
                lit(Lit::Str(s))
            }
            Tok::ByteStr(b) => {
                self.advance();
                lit(Lit::ByteStr(b))
            }
            Tok::Char(c) => {
                self.advance();
                lit(Lit::Char(c))
            }
            Tok::Byte(b) => {
                self.advance();
                lit(Lit::Byte(b))
            }
            Tok::Lifetime(_) => self.expr_block_like(),
            Tok::Punct("(") => {
                self.advance();
                let saved = self.no_struct;
                self.no_struct = false;
                let mut items = Vec::new();
                let mut trailing = false;
                while !self.eat(")") {
                    items.push(self.expr()?);
                    trailing = false;
                    if !self.eat(",") {
                        self.expect(")")?;
                        break;
                    }
                    trailing = true;
                }
                self.no_struct = saved;
                if items.len() == 1 && !trailing {
                    let mut e = items.pop().unwrap();
                    e.pos = pos;
                    return Ok(e);
                }
                Ok(Expr { kind: ExprKind::Tuple(items), pos })
            }
            Tok::Punct("[") => {
                self.advance();
                let saved = self.no_struct;
                self.no_struct = false;
                if self.eat("]") {
                    self.no_struct = saved;
                    return Ok(Expr { kind: ExprKind::Array(vec![]), pos });
                }
                let first = self.expr()?;
                if self.eat(";") {
                    let n = self.expr()?;
                    self.expect("]")?;
                    self.no_struct = saved;
                    return Ok(Expr { kind: ExprKind::ArrayRepeat(Box::new(first), Box::new(n)), pos });
                }
                let mut items = vec![first];
                if self.eat(",") {
                    let rest = self.call_args("]")?;
                    items.extend(rest);
                } else {
                    self.expect("]")?;
                }
                self.no_struct = saved;
                Ok(Expr { kind: ExprKind::Array(items), pos })
            }
            Tok::Punct("{") => self.expr_block_like(),
            Tok::Punct("|") | Tok::Punct("||") => self.closure(false),
            Tok::Ident(k) => match k.as_str() {
                "true" | "false" => {
                    self.advance();
                    lit(Lit::Bool(k == "true"))
                }
                "if" | "match" | "loop" | "while" | "for" | "unsafe" => self.expr_block_like(),
                "move" => {
                    self.advance();
                    self.closure(true)
                }
                "return" => {
                    self.advance();
                    let v = if self.expr_follows() { Some(Box::new(self.expr()?)) } else { None };
                    Ok(Expr { kind: ExprKind::Return(v), pos })
                }
                "break" => {
                    self.advance();
                    let label = if let Tok::Lifetime(l) = self.peek().clone() {
                        self.advance();
                        Some(l)
                    } else {
                        None
                    };
                    let v = if self.expr_follows() { Some(Box::new(self.expr()?)) } else { None };
                    Ok(Expr { kind: ExprKind::Break(label, v), pos })
                }
                "continue" => {
                    self.advance();
                    let label = if let Tok::Lifetime(l) = self.peek().clone() {
                        self.advance();
                        Some(l)
                    } else {
                        None
                    };
                    Ok(Expr { kind: ExprKind::Continue(label), pos })
                }
                _ => self.path_expr(),
            },
            _ => self.err(format!("expected expression, found {}", self.describe())),
        }
    }

    fn expr_follows(&self) -> bool {
        !matches!(self.peek(), Tok::Punct(";") | Tok::Punct("}") | Tok::Punct(")") | Tok::Punct("]") | Tok::Punct(",") | Tok::Eof)
    }

    fn closure(&mut self, is_move: bool) -> PResult<Expr> {
        let pos = self.here();
        let mut params = Vec::new();
        if !self.eat("||") {
            self.expect("|")?;
            while !self.eat("|") {
                let p = self.pattern_no_or()?;
                let t = if self.eat(":") { Some(self.ty()?) } else { None };
                params.push((p, t));
                if !self.eat(",") {
                    self.expect("|")?;
                    break;
                }
            }
        }
        let ret = if self.eat("->") { Some(self.ty()?) } else { None };
        let body = if ret.is_some() {
            let bpos = self.here();
            Expr { kind: ExprKind::Block(Rc::new(self.block()?)), pos: bpos }
        } else {
            self.expr()?
        };
        Ok(Expr { kind: ExprKind::Closure(Rc::new(Closure { params, ret, body, is_move })), pos })
    }

    fn path_segments(&mut self) -> PResult<(Vec<String>, Vec<Type>)> {
        let mut path = Vec::new();
        let mut generics = Vec::new();
        if self.eat("::") {}
        loop {
            match self.advance() {
                Tok::Ident(s) => path.push(s),
                Tok::Punct("<") if path.is_empty() => {
                    // `<T as Trait>::f` / `<Vec<i32>>::new`
                    let t = self.ty()?;
                    if self.eat_kw("as") {
                        self.ty()?;
                    }
                    self.eat_gt()?;
                    path.push(t.name().to_string());
                }
                _ => {
                    self.pos -= 1;
                    return self.err(format!("expected identifier, found {}", self.describe()));
                }
            }
            if self.is("::") {
                self.advance();
                if self.is("<") {
                    self.advance();
                    while !self.is(">") && !self.is(">>") {
                        generics.push(self.ty()?);
                        if !self.eat(",") {
                            break;
                        }
                    }
                    self.eat_gt()?;
                    if self.is("::") {
                        self.advance();
                        continue;
                    }
                    break;
                }
                continue;
            }
            break;
        }
        Ok((path, generics))
    }

    fn path_expr(&mut self) -> PResult<Expr> {
        let pos = self.here();
        let (path, generics) = self.path_segments()?;
        // macros
        if self.is("!") && path.len() == 1 && matches!(self.peek_at(1), Tok::Punct("(") | Tok::Punct("[") | Tok::Punct("{")) {
            self.advance();
            return self.macro_call(path[0].clone(), pos);
        }
        // struct literal
        let last = path.last().map(|s| s.as_str()).unwrap_or("");
        if self.is("{") && !self.no_struct && last.chars().next().is_some_and(|c| c.is_uppercase()) {
            self.advance();
            let mut fields = Vec::new();
            let mut base = None;
            while !self.eat("}") {
                if self.eat("..") {
                    base = Some(Box::new(self.expr()?));
                    self.expect("}")?;
                    break;
                }
                let fpos = self.here();
                let name = match self.advance() {
                    Tok::Ident(s) => s,
                    Tok::Int(n, _) => n.to_string(),
                    _ => {
                        self.pos -= 1;
                        return self.err(format!("expected identifier, found {}", self.describe()));
                    }
                };
                let value = if self.eat(":") { self.expr()? } else { Expr { kind: ExprKind::Path(vec![name.clone()], vec![]), pos: fpos } };
                fields.push((name, value));
                if !self.eat(",") {
                    self.expect("}")?;
                    break;
                }
            }
            return Ok(Expr { kind: ExprKind::StructLit(path, fields, base), pos });
        }
        Ok(Expr { kind: ExprKind::Path(path, generics), pos })
    }

    fn macro_call(&mut self, name: String, pos: Pos) -> PResult<Expr> {
        let close = match self.advance() {
            Tok::Punct("(") => ")",
            Tok::Punct("[") => "]",
            _ => "}",
        };
        let saved = self.no_struct;
        self.no_struct = false;
        let r = self.macro_body(name, close, pos);
        self.no_struct = saved;
        r
    }

    fn macro_body(&mut self, name: String, close: &str, pos: Pos) -> PResult<Expr> {
        match name.as_str() {
            "vec" => {
                if self.eat(close) {
                    return Ok(Expr { kind: ExprKind::Macro(name, vec![]), pos });
                }
                let first = self.expr()?;
                if self.eat(";") {
                    let n = self.expr()?;
                    self.expect(close)?;
                    return Ok(Expr { kind: ExprKind::Macro("vec_repeat".into(), vec![first, n]), pos });
                }
                let mut items = vec![first];
                if self.eat(",") {
                    items.extend(self.call_args(close)?);
                } else {
                    self.expect(close)?;
                }
                Ok(Expr { kind: ExprKind::Macro(name, items), pos })
            }
            "matches" => {
                let e = self.expr()?;
                self.expect(",")?;
                let pat = self.pattern()?;
                let guard = if self.eat_kw("if") { Some(self.expr()?) } else { None };
                self.eat(",");
                self.expect(close)?;
                let t = Expr { kind: ExprKind::Lit(Lit::Bool(true)), pos };
                let f = Expr { kind: ExprKind::Lit(Lit::Bool(false)), pos };
                Ok(Expr { kind: ExprKind::Match(Box::new(e), vec![Arm { pat, guard, body: t }, Arm { pat: Pat::Wild, guard: None, body: f }]), pos })
            }
            "println" | "print" | "eprintln" | "eprint" | "format" | "panic" | "write" | "writeln" | "unreachable" | "todo" | "unimplemented" | "format_args" => {
                let mut target = None;
                if name == "write" || name == "writeln" {
                    target = Some(self.expr()?);
                    if !self.eat(",") {
                        self.expect(close)?;
                        let fmt = String::new();
                        return Ok(Expr { kind: ExprKind::FormatMacro(name, fmt, target.into_iter().collect(), vec![]), pos });
                    }
                }
                if self.eat(close) {
                    return Ok(Expr { kind: ExprKind::FormatMacro(name, String::new(), target.into_iter().collect(), vec![]), pos });
                }
                let fmt = match self.advance() {
                    Tok::Str(s) => s,
                    _ => {
                        self.pos -= 1;
                        // panic!(value) style
                        let e = self.expr()?;
                        self.eat(",");
                        self.expect(close)?;
                        let mut args: Vec<Expr> = target.into_iter().collect();
                        args.push(e);
                        return Ok(Expr { kind: ExprKind::FormatMacro(name, "{}".into(), args, vec![]), pos });
                    }
                };
                let (args, named) = self.format_args(close, target)?;
                Ok(Expr { kind: ExprKind::FormatMacro(name, fmt, args, named), pos })
            }
            "assert" | "assert_eq" | "assert_ne" | "debug_assert" | "debug_assert_eq" | "debug_assert_ne" => {
                let base = name.trim_start_matches("debug_").to_string();
                let n = if base == "assert" { 1 } else { 2 };
                let mut args = Vec::new();
                for i in 0..n {
                    args.push(self.expr()?);
                    if i + 1 < n {
                        self.expect(",")?;
                    }
                }
                if self.eat(",") && !self.is(close) {
                    let fpos = self.here();
                    if let Tok::Str(s) = self.peek().clone() {
                        self.advance();
                        let (fa, named) = self.format_args(close, None)?;
                        args.push(Expr { kind: ExprKind::FormatMacro("format".into(), s, fa, named), pos: fpos });
                        return Ok(Expr { kind: ExprKind::Macro(base, args), pos });
                    }
                }
                self.expect(close)?;
                Ok(Expr { kind: ExprKind::Macro(base, args), pos })
            }
            "dbg" => {
                let args = self.call_args(close)?;
                Ok(Expr { kind: ExprKind::Macro(name, args), pos })
            }
            _ => self.err(format!("cannot find macro `{name}` in this scope")),
        }
    }

    fn format_args(&mut self, close: &str, target: Option<Expr>) -> PResult<(Vec<Expr>, Vec<(String, Expr)>)> {
        let mut args: Vec<Expr> = target.into_iter().collect();
        let mut named = Vec::new();
        while self.eat(",") {
            if self.is(close) {
                break;
            }
            if let (Tok::Ident(n), Tok::Punct("=")) = (self.peek().clone(), self.peek_at(1).clone()) {
                self.advance();
                self.advance();
                named.push((n, self.expr()?));
            } else {
                args.push(self.expr()?);
            }
        }
        self.expect(close)?;
        Ok((args, named))
    }

    // ------------------------------------------------------------ patterns

    pub fn pattern(&mut self) -> PResult<Pat> {
        let first = self.pattern_no_or()?;
        if self.is("|") {
            let mut alts = vec![first];
            while self.eat("|") {
                alts.push(self.pattern_no_or()?);
            }
            return Ok(Pat::Or(alts));
        }
        Ok(first)
    }

    fn lit_pat(&mut self) -> PResult<Option<(Lit, bool)>> {
        let neg = self.is("-") && matches!(self.peek_at(1), Tok::Int(..) | Tok::Float(..));
        if neg {
            self.advance();
        }
        let l = match self.peek().clone() {
            Tok::Int(v, s) => Lit::Int(v, s),
            Tok::Float(v, s) => Lit::Float(v, s),
            Tok::Str(s) => Lit::Str(s),
            Tok::Char(c) => Lit::Char(c),
            Tok::Byte(b) => Lit::Byte(b),
            Tok::Ident(k) if k == "true" || k == "false" => Lit::Bool(k == "true"),
            _ => return Ok(None),
        };
        self.advance();
        Ok(Some((l, neg)))
    }

    fn pattern_no_or(&mut self) -> PResult<Pat> {
        if let Some((l, neg)) = self.lit_pat()? {
            let p = Pat::Lit(l, neg);
            if self.is("..=") || self.is("...") || self.is("..") {
                let incl = !self.is("..");
                self.advance();
                let Some((hi, hneg)) = self.lit_pat()? else { return self.err("expected range end") };
                return Ok(Pat::Range(Box::new(p), Box::new(Pat::Lit(hi, hneg)), incl));
            }
            return Ok(p);
        }
        if self.eat("&") || {
            if self.is("&&") {
                self.toks[self.pos].tok = Tok::Punct("&");
                true
            } else {
                false
            }
        } {
            self.eat_kw("mut");
            return Ok(Pat::Ref(Box::new(self.pattern_no_or()?)));
        }
        if self.eat("..") {
            return Ok(Pat::Rest);
        }
        if self.eat("(") {
            let mut items = Vec::new();
            while !self.eat(")") {
                items.push(self.pattern()?);
                if !self.eat(",") {
                    self.expect(")")?;
                    if items.len() == 1 {
                        return Ok(items.pop().unwrap());
                    }
                    break;
                }
            }
            return Ok(Pat::Tuple(items));
        }
        if self.eat("[") {
            let mut items = Vec::new();
            while !self.eat("]") {
                items.push(self.pattern()?);
                if !self.eat(",") {
                    self.expect("]")?;
                    break;
                }
            }
            return Ok(Pat::Slice(items));
        }
        if self.eat_kw("_") {
            return Ok(Pat::Wild);
        }
        let by_ref = self.eat_kw("ref");
        let mutable = self.eat_kw("mut");
        let _ = by_ref;
        let (path, _) = self.path_segments()?;
        if self.eat("(") {
            let mut items = Vec::new();
            while !self.eat(")") {
                items.push(self.pattern()?);
                if !self.eat(",") {
                    self.expect(")")?;
                    break;
                }
            }
            return Ok(Pat::TupleStruct(path, items));
        }
        if self.is("{") {
            self.advance();
            let mut fields = Vec::new();
            let mut rest = false;
            while !self.eat("}") {
                if self.eat("..") {
                    rest = true;
                    self.expect("}")?;
                    break;
                }
                self.eat_kw("ref");
                let fmut = self.eat_kw("mut");
                let name = match self.advance() {
                    Tok::Ident(s) => s,
                    Tok::Int(n, _) => n.to_string(),
                    _ => {
                        self.pos -= 1;
                        return self.err(format!("expected identifier, found {}", self.describe()));
                    }
                };
                let p = if self.eat(":") { self.pattern()? } else { Pat::Bind { name: name.clone(), mutable: fmut, sub: None } };
                fields.push((name, p));
                if !self.eat(",") {
                    self.expect("}")?;
                    break;
                }
            }
            return Ok(Pat::Struct(path, fields, rest));
        }
        if path.len() == 1 {
            let name = path[0].clone();
            // `name @ subpattern`
            if self.eat("@") {
                let sub = self.pattern_no_or()?;
                return Ok(Pat::Bind { name, mutable, sub: Some(Box::new(sub)) });
            }
            let first = name.chars().next().unwrap_or('a');
            // capitalised single names are unit variants/consts (None, MAX)
            if first.is_uppercase() && !mutable {
                return Ok(Pat::Path(path));
            }
            if self.is("..=") || self.is("..") {
                let incl = self.is("..=");
                self.advance();
                let hi = self.pattern_no_or()?;
                return Ok(Pat::Range(Box::new(Pat::Path(path)), Box::new(hi), incl));
            }
            return Ok(Pat::Bind { name, mutable, sub: None });
        }
        Ok(Pat::Path(path))
    }
}
