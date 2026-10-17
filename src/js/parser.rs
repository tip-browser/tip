//! JavaScript parser producing the AST in `ast.rs`.

use std::rc::Rc;

use super::ast::*;
use super::lexer::{self, TemplatePart, Tok, Token};

struct FuncCtx {
    var_names: Vec<Name>,
    uses_arguments: bool,
    uses_this: bool,
    is_async: bool,
    is_generator: bool,
    is_arrow: bool,
}

pub struct Parser {
    toks: Vec<Token>,
    pos: usize,
    src: Rc<[char]>,
    funcs: Vec<FuncCtx>,
    no_in: bool,
    in_class_field: bool,
    /// Present while parsing an ES module.
    module: Option<ModuleInfo>,
}

type PResult<T> = Result<T, String>;

pub fn parse_program(src: &str, base_line: u32) -> PResult<(Vec<Stmt>, Vec<Name>)> {
    let toks = lexer::tokenize(src, base_line)?;
    let chars: Rc<[char]> = src.chars().collect::<Vec<_>>().into();
    let mut p = Parser { toks, pos: 0, src: chars, funcs: vec![], no_in: false, in_class_field: false, module: None };
    p.funcs.push(FuncCtx { var_names: vec![], uses_arguments: false, uses_this: false, is_async: true, is_generator: false, is_arrow: false });
    let mut body = Vec::new();
    while !p.at_eof() {
        body.push(p.statement()?);
    }
    let ctx = p.funcs.pop().unwrap();
    Ok((body, ctx.var_names))
}

/// Parse an ES module: statements, top-level `var` names and import/export metadata.
pub fn parse_module(src: &str) -> PResult<(Vec<Stmt>, Vec<Name>, ModuleInfo)> {
    let toks = lexer::tokenize(src, 0)?;
    let chars: Rc<[char]> = src.chars().collect::<Vec<_>>().into();
    let mut p = Parser { toks, pos: 0, src: chars, funcs: vec![], no_in: false, in_class_field: false, module: Some(ModuleInfo::default()) };
    p.funcs.push(FuncCtx { var_names: vec![], uses_arguments: false, uses_this: false, is_async: true, is_generator: false, is_arrow: false });
    let mut body = Vec::new();
    while !p.at_eof() {
        body.push(p.statement()?);
    }
    let ctx = p.funcs.pop().unwrap();
    Ok((body, ctx.var_names, p.module.take().unwrap_or_default()))
}

/// Parse the body of a `new Function(...)` / event handler attribute.
pub fn parse_function_body(params: &[String], body: &str) -> PResult<Rc<FuncDef>> {
    let src = format!("(function anonymous({}\n) {{\n{}\n}})", params.join(","), body);
    let (stmts, _) = parse_program(&src, 0)?;
    if let Some(Stmt { kind: StmtKind::Expr(Expr::Function(f)), .. }) = stmts.into_iter().next() {
        return Ok(f);
    }
    Err("SyntaxError: bad function".into())
}

fn name(s: &str) -> Name {
    Rc::from(s)
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos].tok
    }
    fn peek_at(&self, n: usize) -> &Tok {
        self.toks.get(self.pos + n).map(|t| &t.tok).unwrap_or(&Tok::Eof)
    }
    fn tok(&self) -> &Token {
        &self.toks[self.pos]
    }
    fn line(&self) -> u32 {
        self.toks[self.pos].line
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
    fn is_punct(&self, p: &str) -> bool {
        matches!(self.peek(), Tok::Punct(x) if *x == p)
    }
    fn is_kw(&self, k: &str) -> bool {
        matches!(self.peek(), Tok::Keyword(x) if *x == k)
    }
    fn is_ident(&self, s: &str) -> bool {
        matches!(self.peek(), Tok::Ident(x) if x == s)
    }
    fn eat_punct(&mut self, p: &str) -> bool {
        if self.is_punct(p) {
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
    fn err<T>(&self, msg: &str) -> PResult<T> {
        let t = self.tok();
        Err(format!("SyntaxError: {} (line {}:{})", msg, t.line + 1, t.col + 1))
    }
    fn unexpected<T>(&self) -> PResult<T> {
        let desc = match self.peek() {
            Tok::Eof => "end of input".to_string(),
            Tok::Punct(p) => format!("token '{p}'"),
            Tok::Keyword(k) => format!("keyword '{k}'"),
            Tok::Ident(i) => format!("identifier '{i}'"),
            Tok::Num(n) => format!("number {n}"),
            Tok::Str(_) => "string".to_string(),
            t => format!("{t:?}"),
        };
        self.err(&format!("Unexpected {desc}"))
    }
    fn expect_punct(&mut self, p: &str) -> PResult<()> {
        if self.eat_punct(p) {
            Ok(())
        } else {
            self.err(&format!("Expected '{p}'")).or_else(|e: String| {
                let u: PResult<()> = self.unexpected();
                Err(format!("{} — {}", u.unwrap_err(), e.trim_start_matches("SyntaxError: ")))
            })
        }
    }
    fn semicolon(&mut self) -> PResult<()> {
        if self.eat_punct(";") {
            return Ok(());
        }
        if self.is_punct("}") || self.at_eof() || self.tok().nl_before {
            return Ok(());
        }
        self.unexpected()
    }

    fn ident_name(&mut self) -> PResult<Name> {
        match self.advance() {
            Tok::Ident(s) => Ok(name(&s)),
            Tok::Keyword(k) => Ok(name(k)),
            _ => {
                self.pos -= 1;
                self.unexpected()
            }
        }
    }

    fn binding_ident(&mut self) -> PResult<Name> {
        match self.peek().clone() {
            Tok::Ident(s) => {
                self.advance();
                Ok(name(&s))
            }
            _ => self.unexpected(),
        }
    }

    fn fctx(&mut self) -> &mut FuncCtx {
        self.funcs.last_mut().unwrap()
    }

    fn declare_var(&mut self, p: &Pattern) {
        let mut names = Vec::new();
        pattern_names(p, &mut names);
        for n in names {
            if !self.fctx().var_names.contains(&n) {
                self.fctx().var_names.push(n);
            }
        }
    }

    // ------------------------------------------------------------ statements

    fn statement(&mut self) -> PResult<Stmt> {
        let line = self.line();
        let kind = self.statement_kind()?;
        Ok(Stmt { kind, line })
    }

    fn is_let_decl(&self) -> bool {
        self.is_ident("let") && matches!(self.peek_at(1), Tok::Ident(_) | Tok::Punct("[") | Tok::Punct("{") | Tok::Keyword("yield") | Tok::Keyword("await"))
    }

    fn statement_kind(&mut self) -> PResult<StmtKind> {
        match self.peek().clone() {
            Tok::Punct("{") => Ok(StmtKind::Block(self.block()?)),
            Tok::Punct(";") => {
                self.advance();
                Ok(StmtKind::Empty)
            }
            Tok::Keyword("var") => {
                self.advance();
                let d = self.declarations(DeclKind::Var)?;
                self.semicolon()?;
                Ok(d)
            }
            Tok::Keyword("const") => {
                self.advance();
                let d = self.declarations(DeclKind::Const)?;
                self.semicolon()?;
                Ok(d)
            }
            Tok::Ident(ref s) if s == "let" && self.is_let_decl() => {
                self.advance();
                let d = self.declarations(DeclKind::Let)?;
                self.semicolon()?;
                Ok(d)
            }
            Tok::Keyword("function") => {
                let f = self.function(false, true)?;
                Ok(StmtKind::Function(f))
            }
            Tok::Ident(ref s) if s == "async" && matches!(self.peek_at(1), Tok::Keyword("function")) && !self.toks[self.pos + 1].nl_before => {
                self.advance();
                let f = self.function(true, true)?;
                Ok(StmtKind::Function(f))
            }
            Tok::Keyword("class") => {
                let c = self.class(true)?;
                Ok(StmtKind::Class(c))
            }
            Tok::Keyword("if") => {
                self.advance();
                self.expect_punct("(")?;
                let test = self.expression()?;
                self.expect_punct(")")?;
                let cons = Box::new(self.statement()?);
                let alt = if self.eat_kw("else") { Some(Box::new(self.statement()?)) } else { None };
                Ok(StmtKind::If(test, cons, alt))
            }
            Tok::Keyword("for") => self.for_statement(),
            Tok::Keyword("while") => {
                self.advance();
                self.expect_punct("(")?;
                let test = self.expression()?;
                self.expect_punct(")")?;
                let body = Box::new(self.statement()?);
                Ok(StmtKind::While(test, body))
            }
            Tok::Keyword("do") => {
                self.advance();
                let body = Box::new(self.statement()?);
                if !self.eat_kw("while") {
                    return self.unexpected();
                }
                self.expect_punct("(")?;
                let test = self.expression()?;
                self.expect_punct(")")?;
                self.eat_punct(";");
                Ok(StmtKind::DoWhile(body, test))
            }
            Tok::Keyword("return") => {
                self.advance();
                let arg = if self.is_punct(";") || self.is_punct("}") || self.at_eof() || self.tok().nl_before { None } else { Some(self.expression()?) };
                self.semicolon()?;
                Ok(StmtKind::Return(arg))
            }
            Tok::Keyword("break") | Tok::Keyword("continue") => {
                let is_break = self.is_kw("break");
                self.advance();
                let label = match self.peek().clone() {
                    Tok::Ident(s) if !self.tok().nl_before => {
                        self.advance();
                        Some(name(&s))
                    }
                    _ => None,
                };
                self.semicolon()?;
                Ok(if is_break { StmtKind::Break(label) } else { StmtKind::Continue(label) })
            }
            Tok::Keyword("throw") => {
                self.advance();
                if self.tok().nl_before {
                    return self.err("Illegal newline after throw");
                }
                let e = self.expression()?;
                self.semicolon()?;
                Ok(StmtKind::Throw(e))
            }
            Tok::Keyword("try") => {
                self.advance();
                let block = self.block()?;
                let mut param = None;
                let mut handler = None;
                let mut finalizer = None;
                if self.eat_kw("catch") {
                    if self.eat_punct("(") {
                        param = Some(self.binding_target()?);
                        self.expect_punct(")")?;
                    }
                    handler = Some(self.block()?);
                }
                if self.eat_kw("finally") {
                    finalizer = Some(self.block()?);
                }
                if handler.is_none() && finalizer.is_none() {
                    return self.err("Missing catch or finally after try");
                }
                Ok(StmtKind::Try { block, param, handler, finalizer })
            }
            Tok::Keyword("switch") => {
                self.advance();
                self.expect_punct("(")?;
                let disc = self.expression()?;
                self.expect_punct(")")?;
                self.expect_punct("{")?;
                let mut cases = Vec::new();
                let mut scoped = false;
                while !self.eat_punct("}") {
                    let test = if self.eat_kw("case") {
                        Some(self.expression()?)
                    } else if self.eat_kw("default") {
                        None
                    } else {
                        return self.unexpected();
                    };
                    self.expect_punct(":")?;
                    let mut body = Vec::new();
                    while !self.is_kw("case") && !self.is_kw("default") && !self.is_punct("}") {
                        let s = self.statement()?;
                        if is_lexical(&s) {
                            scoped = true;
                        }
                        body.push(s);
                    }
                    cases.push(SwitchCase { test, body });
                }
                Ok(StmtKind::Switch(disc, cases, scoped))
            }
            Tok::Keyword("debugger") => {
                self.advance();
                self.semicolon()?;
                Ok(StmtKind::Debugger)
            }
            Tok::Keyword("import") if !matches!(self.peek_at(1), Tok::Punct("(") | Tok::Punct(".")) => {
                if self.module.is_some() && self.funcs.len() == 1 {
                    return self.import_declaration();
                }
                self.err("Cannot use import statement outside a module")
            }
            Tok::Keyword("export") if self.module.is_some() && self.funcs.len() == 1 => self.export_declaration(),
            Tok::Keyword("export") => {
                // Treat `export` declarations as plain declarations.
                self.advance();
                if self.eat_kw("default") {
                    if self.is_kw("function") || self.is_kw("class") || self.is_ident("async") {
                        return self.statement_kind();
                    }
                    let e = self.assign()?;
                    self.semicolon()?;
                    return Ok(StmtKind::Expr(e));
                }
                if self.is_punct("{") || self.is_punct("*") {
                    while !self.at_eof() && !self.is_punct(";") && !self.tok().nl_before {
                        self.advance();
                    }
                    self.eat_punct(";");
                    return Ok(StmtKind::Empty);
                }
                self.statement_kind()
            }
            Tok::Ident(ref s) if matches!(self.peek_at(1), Tok::Punct(":")) => {
                let label = name(s);
                self.advance();
                self.advance();
                let body = self.statement()?;
                Ok(StmtKind::Labeled(label, Box::new(body)))
            }
            _ => {
                let e = self.expression()?;
                self.semicolon()?;
                Ok(StmtKind::Expr(e))
            }
        }
    }

    // ------------------------------------------------------------ modules

    fn module_info(&mut self) -> &mut ModuleInfo {
        self.module.as_mut().unwrap()
    }

    fn add_request(&mut self, spec: &Name) {
        if !self.module_info().requests.contains(spec) {
            self.module_info().requests.push(spec.clone());
        }
    }

    /// `"spec"` after `from`, plus an optional `with { ... }` attributes clause.
    fn module_specifier(&mut self) -> PResult<Name> {
        let spec = match self.advance() {
            Tok::Str(s) => name(&s),
            _ => {
                self.pos -= 1;
                return self.unexpected();
            }
        };
        if (self.is_kw("with") || self.is_ident("assert")) && !self.tok().nl_before {
            self.advance();
            self.expect_punct("{")?;
            let mut depth = 1;
            while depth > 0 && !self.at_eof() {
                match self.advance() {
                    Tok::Punct("{") => depth += 1,
                    Tok::Punct("}") => depth -= 1,
                    _ => {}
                }
            }
        }
        self.add_request(&spec);
        Ok(spec)
    }

    /// An export/import name: identifier, keyword or string literal.
    fn module_export_name(&mut self) -> PResult<Name> {
        if let Tok::Str(s) = self.peek().clone() {
            self.advance();
            return Ok(name(&s));
        }
        self.ident_name()
    }

    fn expect_from(&mut self) -> PResult<()> {
        if self.is_ident("from") {
            self.advance();
            Ok(())
        } else {
            self.err("Expected 'from'")
        }
    }

    fn import_declaration(&mut self) -> PResult<StmtKind> {
        self.advance(); // import
        if let Tok::Str(_) = self.peek() {
            self.module_specifier()?;
            self.semicolon()?;
            return Ok(StmtKind::Empty);
        }
        // (local, imported) pairs; imported None = namespace
        let mut bindings: Vec<(Name, Option<Name>)> = Vec::new();
        if let Tok::Ident(_) = self.peek() {
            if !self.is_ident("from") || matches!(self.peek_at(1), Tok::Punct(",") | Tok::Ident(_)) {
                let local = self.binding_ident()?;
                bindings.push((local, Some(name("default"))));
                if !self.eat_punct(",") {
                    self.expect_from()?;
                    return self.finish_import(bindings);
                }
            }
        }
        if self.eat_punct("*") {
            if !self.is_ident("as") {
                return self.err("Expected 'as'");
            }
            self.advance();
            let local = self.binding_ident()?;
            bindings.push((local, None));
        } else if self.eat_punct("{") {
            while !self.eat_punct("}") {
                let imported = self.module_export_name()?;
                let local = if self.is_ident("as") {
                    self.advance();
                    self.binding_ident()?
                } else {
                    imported.clone()
                };
                bindings.push((local, Some(imported)));
                if !self.eat_punct(",") {
                    self.expect_punct("}")?;
                    break;
                }
            }
        }
        self.expect_from()?;
        self.finish_import(bindings)
    }

    fn finish_import(&mut self, bindings: Vec<(Name, Option<Name>)>) -> PResult<StmtKind> {
        let spec = self.module_specifier()?;
        self.semicolon()?;
        for (local, import) in bindings {
            self.module_info().imports.push(ImportEntry { specifier: spec.clone(), import, local });
        }
        Ok(StmtKind::Empty)
    }

    fn export_declaration(&mut self) -> PResult<StmtKind> {
        self.advance(); // export
        if self.eat_kw("default") {
            let named_fn = |p: &Parser, at: usize| matches!(p.peek_at(at), Tok::Keyword("function")) && (matches!(p.peek_at(at + 1), Tok::Ident(_)) || (matches!(p.peek_at(at + 1), Tok::Punct("*")) && matches!(p.peek_at(at + 2), Tok::Ident(_))));
            let is_decl = named_fn(self, 0) || (self.is_ident("async") && named_fn(self, 1)) || (self.is_kw("class") && matches!(self.peek_at(1), Tok::Ident(_)));
            if is_decl {
                let kind = self.statement_kind()?;
                let local = match &kind {
                    StmtKind::Function(f) => f.name.clone(),
                    StmtKind::Class(c) => c.name.clone(),
                    _ => None,
                };
                if let Some(local) = local {
                    self.module_info().exports.push(ExportEntry::Local { exported: name("default"), local });
                }
                return Ok(kind);
            }
            let e = self.assign()?;
            self.semicolon()?;
            let local = name(DEFAULT_EXPORT_BINDING);
            self.module_info().exports.push(ExportEntry::Local { exported: name("default"), local: local.clone() });
            return Ok(StmtKind::Decl(DeclKind::Let, vec![(Pattern::Ident(local), Some(e))]));
        }
        if self.eat_punct("*") {
            let exported = if self.is_ident("as") {
                self.advance();
                Some(self.module_export_name()?)
            } else {
                None
            };
            self.expect_from()?;
            let spec = self.module_specifier()?;
            self.semicolon()?;
            match exported {
                Some(exported) => self.module_info().exports.push(ExportEntry::Indirect { exported, specifier: spec, import: None }),
                None => self.module_info().star_exports.push(spec),
            }
            return Ok(StmtKind::Empty);
        }
        if self.eat_punct("{") {
            let mut pairs = Vec::new();
            while !self.eat_punct("}") {
                let local = self.module_export_name()?;
                let exported = if self.is_ident("as") {
                    self.advance();
                    self.module_export_name()?
                } else {
                    local.clone()
                };
                pairs.push((local, exported));
                if !self.eat_punct(",") {
                    self.expect_punct("}")?;
                    break;
                }
            }
            if self.is_ident("from") {
                self.advance();
                let spec = self.module_specifier()?;
                for (import, exported) in pairs {
                    self.module_info().exports.push(ExportEntry::Indirect { exported, specifier: spec.clone(), import: Some(import) });
                }
            } else {
                for (local, exported) in pairs {
                    self.module_info().exports.push(ExportEntry::Local { exported, local });
                }
            }
            self.semicolon()?;
            return Ok(StmtKind::Empty);
        }
        let kind = self.statement_kind()?;
        let mut names = Vec::new();
        match &kind {
            StmtKind::Decl(_, decls) => {
                for (p, _) in decls {
                    pattern_names(p, &mut names);
                }
            }
            StmtKind::Function(f) => names.extend(f.name.clone()),
            StmtKind::Class(c) => names.extend(c.name.clone()),
            _ => return self.err("Unexpected export"),
        }
        for n in names {
            self.module_info().exports.push(ExportEntry::Local { exported: n.clone(), local: n });
        }
        Ok(kind)
    }

    fn block(&mut self) -> PResult<Block> {
        self.expect_punct("{")?;
        let mut body = Vec::new();
        let mut scoped = false;
        while !self.eat_punct("}") {
            if self.at_eof() {
                return self.unexpected();
            }
            let s = self.statement()?;
            if is_lexical(&s) {
                scoped = true;
            }
            body.push(s);
        }
        Ok(Block { body, scoped })
    }

    fn declarations(&mut self, kind: DeclKind) -> PResult<StmtKind> {
        let mut decls = Vec::new();
        loop {
            let target = self.binding_target()?;
            let init = if self.eat_punct("=") { Some(self.assign()?) } else { None };
            if kind == DeclKind::Var {
                self.declare_var(&target);
            }
            decls.push((target, init));
            if !self.eat_punct(",") {
                break;
            }
        }
        Ok(StmtKind::Decl(kind, decls))
    }

    fn for_statement(&mut self) -> PResult<StmtKind> {
        self.advance();
        let is_await = if self.is_ident("await") || self.is_kw("await") {
            self.advance();
            true
        } else {
            false
        };
        self.expect_punct("(")?;
        let mut init: Option<ForInit> = None;
        let decl_kind = if self.is_kw("var") {
            Some(DeclKind::Var)
        } else if self.is_kw("const") {
            Some(DeclKind::Const)
        } else if self.is_ident("let") && matches!(self.peek_at(1), Tok::Ident(_) | Tok::Punct("[") | Tok::Punct("{")) {
            Some(DeclKind::Let)
        } else {
            None
        };
        if let Some(kind) = decl_kind {
            self.advance();
            let target = self.binding_target()?;
            if self.is_ident("of") || self.is_kw("in") {
                let of = self.is_ident("of");
                self.advance();
                if kind == DeclKind::Var {
                    self.declare_var(&target);
                }
                let rhs = if of { self.assign()? } else { self.expression()? };
                self.expect_punct(")")?;
                let body = Box::new(self.statement()?);
                return Ok(if of {
                    StmtKind::ForOf { target: ForTarget::Decl(kind, target), iter: rhs, body, is_await }
                } else {
                    StmtKind::ForIn { target: ForTarget::Decl(kind, target), obj: rhs, body }
                });
            }
            let mut decls = Vec::new();
            let save = self.no_in;
            self.no_in = true;
            let first_init = if self.eat_punct("=") { Some(self.assign()?) } else { None };
            if kind == DeclKind::Var {
                self.declare_var(&target);
            }
            decls.push((target, first_init));
            while self.eat_punct(",") {
                let t = self.binding_target()?;
                let i = if self.eat_punct("=") { Some(self.assign()?) } else { None };
                if kind == DeclKind::Var {
                    self.declare_var(&t);
                }
                decls.push((t, i));
            }
            self.no_in = save;
            init = Some(ForInit::Decl(kind, decls));
        } else if !self.is_punct(";") {
            let save = self.no_in;
            self.no_in = true;
            let e = self.expression()?;
            self.no_in = save;
            if self.is_ident("of") || self.is_kw("in") {
                let of = self.is_ident("of");
                self.advance();
                let target = self.expr_to_pattern(e)?;
                let rhs = if of { self.assign()? } else { self.expression()? };
                self.expect_punct(")")?;
                let body = Box::new(self.statement()?);
                return Ok(if of {
                    StmtKind::ForOf { target: ForTarget::Pattern(target), iter: rhs, body, is_await }
                } else {
                    StmtKind::ForIn { target: ForTarget::Pattern(target), obj: rhs, body }
                });
            }
            init = Some(ForInit::Expr(e));
        }
        self.expect_punct(";")?;
        let test = if self.is_punct(";") { None } else { Some(self.expression()?) };
        self.expect_punct(";")?;
        let update = if self.is_punct(")") { None } else { Some(self.expression()?) };
        self.expect_punct(")")?;
        let body = Box::new(self.statement()?);
        let mut per_iteration = Vec::new();
        if let Some(ForInit::Decl(DeclKind::Let, decls)) = &init {
            for (p, _) in decls {
                pattern_names(p, &mut per_iteration);
            }
        }
        Ok(StmtKind::For { init, test, update, body, per_iteration })
    }

    // ------------------------------------------------------------ patterns

    fn binding_target(&mut self) -> PResult<Pattern> {
        match self.peek().clone() {
            Tok::Punct("[") => {
                self.advance();
                let mut elems = Vec::new();
                let mut rest = None;
                loop {
                    if self.eat_punct("]") {
                        break;
                    }
                    if self.is_punct(",") {
                        self.advance();
                        elems.push(None);
                        continue;
                    }
                    if self.eat_punct("...") {
                        rest = Some(Box::new(self.binding_target()?));
                        self.eat_punct(",");
                        self.expect_punct("]")?;
                        break;
                    }
                    let p = self.binding_target()?;
                    let d = if self.eat_punct("=") { Some(self.assign()?) } else { None };
                    elems.push(Some((p, d)));
                    if !self.eat_punct(",") {
                        self.expect_punct("]")?;
                        break;
                    }
                }
                Ok(Pattern::Array { elems, rest })
            }
            Tok::Punct("{") => {
                self.advance();
                let mut props = Vec::new();
                let mut rest = None;
                loop {
                    if self.eat_punct("}") {
                        break;
                    }
                    if self.eat_punct("...") {
                        rest = Some(Box::new(Pattern::Ident(self.binding_ident()?)));
                        self.eat_punct(",");
                        self.expect_punct("}")?;
                        break;
                    }
                    let key = self.prop_key()?;
                    let (pat, def) = if self.eat_punct(":") {
                        let p = self.binding_target()?;
                        let d = if self.eat_punct("=") { Some(self.assign()?) } else { None };
                        (p, d)
                    } else {
                        let n = match &key {
                            Key::Name(n) => n.clone(),
                            _ => return self.unexpected(),
                        };
                        let d = if self.eat_punct("=") { Some(self.assign()?) } else { None };
                        (Pattern::Ident(n), d)
                    };
                    props.push((key, pat, d_or(def)));
                    if !self.eat_punct(",") {
                        self.expect_punct("}")?;
                        break;
                    }
                }
                Ok(Pattern::Object { props, rest })
            }
            _ => Ok(Pattern::Ident(self.binding_ident()?)),
        }
    }

    fn expr_to_pattern(&self, e: Expr) -> PResult<Pattern> {
        Ok(match e {
            Expr::Ident(n) => Pattern::Ident(n),
            Expr::Member { .. } => Pattern::Expr(Box::new(e)),
            Expr::Array(elems) => {
                let mut out = Vec::new();
                let mut rest = None;
                for el in elems {
                    match el {
                        ArrayElem::Hole => out.push(None),
                        ArrayElem::Spread(e) => rest = Some(Box::new(self.expr_to_pattern(e)?)),
                        ArrayElem::Item(Expr::Assign(AssignOp::Assign, target, def)) => out.push(Some((*target, Some(*def)))),
                        ArrayElem::Item(e) => out.push(Some((self.expr_to_pattern(e)?, None))),
                    }
                }
                Pattern::Array { elems: out, rest }
            }
            Expr::Object(props) => {
                let mut out = Vec::new();
                let mut rest = None;
                for p in props {
                    match p {
                        ObjProp::Shorthand(n) => out.push((Key::Name(n.clone()), Pattern::Ident(n), None)),
                        ObjProp::KeyValue(k, Expr::Assign(AssignOp::Assign, target, def)) => out.push((k, *target, Some(*def))),
                        ObjProp::KeyValue(k, v) => out.push((k, self.expr_to_pattern(v)?, None)),
                        ObjProp::Spread(e) => rest = Some(Box::new(self.expr_to_pattern(e)?)),
                        ObjProp::Method(..) => return self.err("Invalid destructuring assignment target"),
                    }
                }
                Pattern::Object { props: out, rest }
            }
            Expr::Assign(AssignOp::Assign, target, _) => *target,
            _ => return self.err("Invalid left-hand side in assignment"),
        })
    }

    // ------------------------------------------------------------ functions & classes

    fn params(&mut self) -> PResult<(Vec<Param>, Option<Pattern>)> {
        self.expect_punct("(")?;
        let mut params = Vec::new();
        let mut rest = None;
        while !self.eat_punct(")") {
            if self.eat_punct("...") {
                rest = Some(self.binding_target()?);
                self.eat_punct(",");
                self.expect_punct(")")?;
                break;
            }
            let pattern = self.binding_target()?;
            let default = if self.eat_punct("=") { Some(self.assign()?) } else { None };
            params.push(Param { pattern, default });
            if !self.eat_punct(",") {
                self.expect_punct(")")?;
                break;
            }
        }
        Ok((params, rest))
    }

    fn push_func(&mut self, is_async: bool, is_generator: bool, is_arrow: bool) {
        self.funcs.push(FuncCtx { var_names: vec![], uses_arguments: false, uses_this: false, is_async, is_generator, is_arrow });
    }

    fn pop_func(&mut self) -> FuncCtx {
        let f = self.funcs.pop().unwrap();
        if f.is_arrow {
            // arrows see the enclosing function's `arguments`/`this`
            if let Some(parent) = self.funcs.last_mut() {
                parent.uses_arguments |= f.uses_arguments;
                parent.uses_this |= f.uses_this;
            }
        }
        f
    }

    fn source_from(&self, start_tok: usize) -> Rc<str> {
        let s = self.toks[start_tok].start;
        let e = self.toks[self.pos.saturating_sub(1)].end;
        if e > s && e <= self.src.len() {
            self.src[s..e].iter().collect::<String>().into()
        } else {
            "function () { [native code] }".into()
        }
    }

    /// Parses `function [*] [name] (params) { body }` (current token: `function`).
    fn function(&mut self, is_async: bool, is_decl: bool) -> PResult<Rc<FuncDef>> {
        let start = if is_async { self.pos - 1 } else { self.pos };
        let line = self.line();
        self.advance(); // function
        let is_generator = self.eat_punct("*");
        let fname = match self.peek().clone() {
            Tok::Ident(s) => {
                self.advance();
                Some(name(&s))
            }
            Tok::Keyword(k) if !is_decl && matches!(k, "yield" | "await") => {
                self.advance();
                Some(name(k))
            }
            _ => {
                if is_decl {
                    return self.unexpected();
                }
                None
            }
        };
        self.function_rest(fname, is_async, is_generator, false, false, start, line)
    }

    #[allow(clippy::too_many_arguments)]
    fn function_rest(&mut self, fname: Option<Name>, is_async: bool, is_generator: bool, is_method: bool, _is_ctor: bool, start: usize, line: u32) -> PResult<Rc<FuncDef>> {
        self.push_func(is_async, is_generator, false);
        let (params, rest) = self.params()?;
        let save_no_in = self.no_in;
        self.no_in = false;
        let save_field = self.in_class_field;
        self.in_class_field = false;
        let body = self.function_body()?;
        self.in_class_field = save_field;
        self.no_in = save_no_in;
        let ctx = self.pop_func();
        let simple = params.iter().all(|p| matches!(p.pattern, Pattern::Ident(_)) && p.default.is_none()) && rest.is_none();
        Ok(Rc::new(FuncDef {
            name: fname,
            params,
            rest,
            body: FuncBody::Block(body),
            is_arrow: false,
            is_async,
            is_generator,
            is_method,
            var_names: ctx.var_names,
            uses_arguments: ctx.uses_arguments,
            uses_this: ctx.uses_this,
            simple_params: simple,
            line,
            source: self.source_from(start),
        }))
    }

    fn function_body(&mut self) -> PResult<Vec<Stmt>> {
        self.expect_punct("{")?;
        let mut body = Vec::new();
        while !self.eat_punct("}") {
            if self.at_eof() {
                return self.unexpected();
            }
            body.push(self.statement()?);
        }
        Ok(body)
    }

    fn arrow(&mut self, params: Vec<Param>, rest: Option<Pattern>, is_async: bool, start: usize, line: u32) -> PResult<Expr> {
        self.expect_punct("=>")?;
        self.push_func(is_async, false, true);
        let body = if self.is_punct("{") {
            let save = self.no_in;
            self.no_in = false;
            let b = FuncBody::Block(self.function_body()?);
            self.no_in = save;
            b
        } else {
            FuncBody::Expr(self.assign()?)
        };
        let ctx = self.pop_func();
        let simple = params.iter().all(|p| matches!(p.pattern, Pattern::Ident(_)) && p.default.is_none()) && rest.is_none();
        Ok(Expr::Function(Rc::new(FuncDef {
            name: None,
            params,
            rest,
            body,
            is_arrow: true,
            is_async,
            is_generator: false,
            is_method: false,
            var_names: ctx.var_names,
            uses_arguments: false,
            uses_this: ctx.uses_this,
            simple_params: simple,
            line,
            source: self.source_from(start),
        })))
    }

    fn prop_key(&mut self) -> PResult<Key> {
        match self.advance() {
            Tok::Ident(s) => Ok(Key::Name(name(&s))),
            Tok::Keyword(k) => Ok(Key::Name(name(k))),
            Tok::Str(s) => Ok(Key::Name(name(&s))),
            Tok::Num(n) => Ok(Key::Name(name(&super::value::num_to_string(n)))),
            Tok::BigInt(s) => Ok(Key::Name(name(&s))),
            Tok::PrivateName(s) => Ok(Key::Private(name(&s))),
            Tok::Punct("[") => {
                let save = self.no_in;
                self.no_in = false;
                let e = self.assign()?;
                self.no_in = save;
                self.expect_punct("]")?;
                Ok(Key::Computed(Box::new(e)))
            }
            _ => {
                self.pos -= 1;
                self.unexpected()
            }
        }
    }

    fn class(&mut self, is_decl: bool) -> PResult<Rc<ClassDef>> {
        self.advance(); // class
        let cname = match self.peek().clone() {
            Tok::Ident(s) if s != "extends" || is_decl => {
                self.advance();
                Some(name(&s))
            }
            _ => None,
        };
        let superclass = if self.eat_kw("extends") { Some(self.lhs()?) } else { None };
        self.expect_punct("{")?;
        let mut constructor = None;
        let mut members = Vec::new();
        while !self.eat_punct("}") {
            if self.eat_punct(";") {
                continue;
            }
            let line = self.line();
            let start = self.pos;
            let mut is_static = false;
            if self.is_ident("static") && !matches!(self.peek_at(1), Tok::Punct("(") | Tok::Punct("=") | Tok::Punct(";") | Tok::Punct("}")) {
                self.advance();
                is_static = true;
                if self.is_punct("{") {
                    self.push_func(false, false, false);
                    let body = self.function_body()?;
                    self.pop_func();
                    members.push(ClassMember::StaticBlock(body));
                    continue;
                }
            }
            let mut is_async = false;
            let mut kind = MethodKind::Method;
            if self.is_ident("async") && !matches!(self.peek_at(1), Tok::Punct("(") | Tok::Punct("=") | Tok::Punct(";") | Tok::Punct("}")) && !self.toks[self.pos + 1].nl_before {
                self.advance();
                is_async = true;
            }
            let is_gen = self.eat_punct("*");
            if (self.is_ident("get") || self.is_ident("set")) && !matches!(self.peek_at(1), Tok::Punct("(") | Tok::Punct("=") | Tok::Punct(";") | Tok::Punct("}")) {
                kind = if self.is_ident("get") { MethodKind::Get } else { MethodKind::Set };
                self.advance();
            }
            let key = self.prop_key()?;
            if self.is_punct("(") {
                let mname = match &key {
                    Key::Name(n) | Key::Private(n) => Some(n.clone()),
                    _ => None,
                };
                let is_ctor = !is_static && matches!(&key, Key::Name(n) if &**n == "constructor") && kind == MethodKind::Method;
                let f = self.function_rest(mname, is_async, is_gen, true, is_ctor, start, line)?;
                if is_ctor {
                    constructor = Some(f);
                } else {
                    members.push(ClassMember::Method { key, kind, is_static, func: f });
                }
            } else {
                // field
                let value = if self.eat_punct("=") {
                    self.push_func(false, false, false);
                    let save = self.in_class_field;
                    self.in_class_field = true;
                    let v = self.assign()?;
                    self.in_class_field = save;
                    self.pop_func();
                    Some(v)
                } else {
                    None
                };
                self.semicolon()?;
                members.push(ClassMember::Field { key, is_static, value });
            }
        }
        Ok(Rc::new(ClassDef { name: cname, superclass, constructor, members }))
    }

    // ------------------------------------------------------------ expressions

    pub fn expression(&mut self) -> PResult<Expr> {
        let first = self.assign()?;
        if self.is_punct(",") {
            let mut v = vec![first];
            while self.eat_punct(",") {
                v.push(self.assign()?);
            }
            return Ok(Expr::Seq(v));
        }
        Ok(first)
    }

    fn arrow_ahead(&self) -> bool {
        // current token is "(": find matching ")" and check for "=>"
        let mut depth = 0;
        let mut i = self.pos;
        while i < self.toks.len() {
            match &self.toks[i].tok {
                Tok::Punct("(") | Tok::Punct("[") | Tok::Punct("{") => depth += 1,
                Tok::Punct(")") | Tok::Punct("]") | Tok::Punct("}") => {
                    depth -= 1;
                    if depth == 0 {
                        return matches!(self.toks.get(i + 1).map(|t| &t.tok), Some(Tok::Punct("=>"))) && !self.toks[i + 1].nl_before;
                    }
                }
                Tok::Eof => return false,
                _ => {}
            }
            i += 1;
        }
        false
    }

    pub fn assign(&mut self) -> PResult<Expr> {
        let start = self.pos;
        let line = self.line();
        // arrow functions
        match self.peek().clone() {
            Tok::Ident(s) if matches!(self.peek_at(1), Tok::Punct("=>")) => {
                self.advance();
                let p = Param { pattern: Pattern::Ident(name(&s)), default: None };
                return self.arrow(vec![p], None, false, start, line);
            }
            Tok::Ident(s) if s == "async" && !self.toks[self.pos + 1].nl_before => {
                if let (Tok::Ident(p), Tok::Punct("=>")) = (self.peek_at(1).clone(), self.peek_at(2).clone()) {
                    self.advance();
                    self.advance();
                    let param = Param { pattern: Pattern::Ident(name(&p)), default: None };
                    return self.arrow(vec![param], None, true, start, line);
                }
                if matches!(self.peek_at(1), Tok::Punct("(")) {
                    self.advance();
                    if self.arrow_ahead() {
                        self.push_func(true, false, true);
                        let (params, rest) = self.params()?;
                        self.pop_func();
                        return self.arrow(params, rest, true, start, line);
                    }
                    self.pos -= 1;
                }
            }
            Tok::Punct("(") if self.arrow_ahead() => {
                self.push_func(false, false, true);
                let (params, rest) = self.params()?;
                self.pop_func();
                return self.arrow(params, rest, false, start, line);
            }
            Tok::Keyword("yield") | Tok::Ident(_) if self.is_yield() => {
                self.advance();
                let delegate = self.eat_punct("*");
                let arg = if self.tok().nl_before || matches!(self.peek(), Tok::Punct(")") | Tok::Punct("]") | Tok::Punct("}") | Tok::Punct(",") | Tok::Punct(";") | Tok::Punct(":") | Tok::Eof) {
                    None
                } else {
                    Some(Box::new(self.assign()?))
                };
                return Ok(Expr::Yield { arg, delegate });
            }
            _ => {}
        }
        let lhs = self.conditional()?;
        let op = match self.peek() {
            Tok::Punct("=") => AssignOp::Assign,
            Tok::Punct("+=") => AssignOp::Op(BinOp::Add),
            Tok::Punct("-=") => AssignOp::Op(BinOp::Sub),
            Tok::Punct("*=") => AssignOp::Op(BinOp::Mul),
            Tok::Punct("/=") => AssignOp::Op(BinOp::Div),
            Tok::Punct("%=") => AssignOp::Op(BinOp::Mod),
            Tok::Punct("**=") => AssignOp::Op(BinOp::Exp),
            Tok::Punct("<<=") => AssignOp::Op(BinOp::Shl),
            Tok::Punct(">>=") => AssignOp::Op(BinOp::Shr),
            Tok::Punct(">>>=") => AssignOp::Op(BinOp::UShr),
            Tok::Punct("&=") => AssignOp::Op(BinOp::BitAnd),
            Tok::Punct("|=") => AssignOp::Op(BinOp::BitOr),
            Tok::Punct("^=") => AssignOp::Op(BinOp::BitXor),
            Tok::Punct("&&=") => AssignOp::Logic(LogicOp::And),
            Tok::Punct("||=") => AssignOp::Logic(LogicOp::Or),
            Tok::Punct("??=") => AssignOp::Logic(LogicOp::Nullish),
            _ => return Ok(lhs),
        };
        self.advance();
        let target = if op == AssignOp::Assign {
            self.expr_to_pattern(lhs)?
        } else {
            match lhs {
                Expr::Ident(_) | Expr::Member { .. } => Pattern::Expr(Box::new(lhs)),
                _ => return self.err("Invalid left-hand side in assignment"),
            }
        };
        let target = match target {
            Pattern::Expr(e) if matches!(*e, Expr::Ident(_)) => {
                if let Expr::Ident(n) = *e {
                    Pattern::Ident(n)
                } else {
                    unreachable!()
                }
            }
            t => t,
        };
        let rhs = self.assign()?;
        Ok(Expr::Assign(op, Box::new(target), Box::new(rhs)))
    }

    fn is_yield(&self) -> bool {
        let in_gen = self.funcs.last().is_some_and(|f| f.is_generator);
        in_gen && matches!(self.peek(), Tok::Ident(s) if s == "yield")
    }

    fn conditional(&mut self) -> PResult<Expr> {
        let test = self.binary(1)?;
        if self.eat_punct("?") {
            let save = self.no_in;
            self.no_in = false;
            let cons = self.assign()?;
            self.no_in = save;
            self.expect_punct(":")?;
            let alt = self.assign()?;
            return Ok(Expr::Cond(Box::new(test), Box::new(cons), Box::new(alt)));
        }
        Ok(test)
    }

    fn binop_info(&self) -> Option<(u8, Result<BinOp, LogicOp>)> {
        Some(match self.peek() {
            Tok::Punct(p) => match *p {
                "??" => (1, Err(LogicOp::Nullish)),
                "||" => (2, Err(LogicOp::Or)),
                "&&" => (3, Err(LogicOp::And)),
                "|" => (4, Ok(BinOp::BitOr)),
                "^" => (5, Ok(BinOp::BitXor)),
                "&" => (6, Ok(BinOp::BitAnd)),
                "==" => (7, Ok(BinOp::Eq)),
                "!=" => (7, Ok(BinOp::Ne)),
                "===" => (7, Ok(BinOp::StrictEq)),
                "!==" => (7, Ok(BinOp::StrictNe)),
                "<" => (8, Ok(BinOp::Lt)),
                ">" => (8, Ok(BinOp::Gt)),
                "<=" => (8, Ok(BinOp::Le)),
                ">=" => (8, Ok(BinOp::Ge)),
                "<<" => (9, Ok(BinOp::Shl)),
                ">>" => (9, Ok(BinOp::Shr)),
                ">>>" => (9, Ok(BinOp::UShr)),
                "+" => (10, Ok(BinOp::Add)),
                "-" => (10, Ok(BinOp::Sub)),
                "*" => (11, Ok(BinOp::Mul)),
                "/" => (11, Ok(BinOp::Div)),
                "%" => (11, Ok(BinOp::Mod)),
                "**" => (12, Ok(BinOp::Exp)),
                _ => return None,
            },
            Tok::Keyword("instanceof") => (8, Ok(BinOp::InstanceOf)),
            Tok::Keyword("in") if !self.no_in => (8, Ok(BinOp::In)),
            _ => return None,
        })
    }

    fn binary(&mut self, min_prec: u8) -> PResult<Expr> {
        let mut lhs = if let (Tok::PrivateName(n), Tok::Keyword("in")) = (self.peek().clone(), self.peek_at(1).clone()) {
            self.advance();
            self.advance();
            let rhs = self.binary(9)?;
            Expr::PrivateIn(name(&n), Box::new(rhs))
        } else {
            self.unary()?
        };
        while let Some((prec, op)) = self.binop_info() {
            if prec < min_prec {
                break;
            }
            self.advance();
            // `**` is right-associative; everything else is left-associative.
            let rhs = if prec == 12 { self.binary(prec)? } else { self.binary(prec + 1)? };
            lhs = match op {
                Ok(b) => Expr::Binary(b, Box::new(lhs), Box::new(rhs)),
                Err(l) => Expr::Logical(l, Box::new(lhs), Box::new(rhs)),
            };
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> PResult<Expr> {
        let op = match self.peek() {
            Tok::Punct("!") => Some(UnaryOp::Not),
            Tok::Punct("-") => Some(UnaryOp::Neg),
            Tok::Punct("+") => Some(UnaryOp::Plus),
            Tok::Punct("~") => Some(UnaryOp::BitNot),
            Tok::Keyword("typeof") => Some(UnaryOp::TypeOf),
            Tok::Keyword("void") => Some(UnaryOp::Void),
            Tok::Keyword("delete") => Some(UnaryOp::Delete),
            _ => None,
        };
        if let Some(op) = op {
            self.advance();
            let arg = self.unary()?;
            return Ok(Expr::Unary(op, Box::new(arg)));
        }
        if self.is_punct("++") || self.is_punct("--") {
            let inc = self.is_punct("++");
            self.advance();
            let target = self.unary()?;
            return Ok(Expr::Update { inc, prefix: true, target: Box::new(target) });
        }
        if self.is_ident("await") && self.funcs.last().is_some_and(|f| f.is_async) && !matches!(self.peek_at(1), Tok::Punct("=>") | Tok::Punct(")") | Tok::Punct(",") | Tok::Punct(";") | Tok::Punct("=")) {
            self.advance();
            let arg = self.unary()?;
            return Ok(Expr::Await(Box::new(arg)));
        }
        let e = self.postfix()?;
        if self.is_punct("**") {
            // handled by binary()
        }
        Ok(e)
    }

    fn postfix(&mut self) -> PResult<Expr> {
        let e = self.lhs()?;
        if (self.is_punct("++") || self.is_punct("--")) && !self.tok().nl_before {
            let inc = self.is_punct("++");
            self.advance();
            return Ok(Expr::Update { inc, prefix: false, target: Box::new(e) });
        }
        Ok(e)
    }

    fn args(&mut self) -> PResult<Vec<ArrayElem>> {
        self.expect_punct("(")?;
        let mut args = Vec::new();
        let save = self.no_in;
        self.no_in = false;
        while !self.eat_punct(")") {
            if self.eat_punct("...") {
                args.push(ArrayElem::Spread(self.assign()?));
            } else {
                args.push(ArrayElem::Item(self.assign()?));
            }
            if !self.eat_punct(",") {
                self.expect_punct(")")?;
                break;
            }
        }
        self.no_in = save;
        Ok(args)
    }

    /// Member / call / new expressions.
    fn lhs(&mut self) -> PResult<Expr> {
        let mut e = if self.is_kw("new") {
            self.advance();
            if self.eat_punct(".") {
                let n = self.ident_name()?;
                if &*n != "target" {
                    return self.unexpected();
                }
                Expr::NewTarget
            } else {
                // callee: member expression without calls
                let mut callee = if self.is_kw("new") { self.lhs_new_only()? } else { self.primary()? };
                loop {
                    if self.eat_punct(".") {
                        let key = match self.peek().clone() {
                            Tok::PrivateName(n) => {
                                self.advance();
                                Key::Private(name(&n))
                            }
                            _ => Key::Name(self.ident_name()?),
                        };
                        callee = Expr::Member { obj: Box::new(callee), prop: Box::new(key), optional: false };
                    } else if self.is_punct("[") {
                        self.advance();
                        let p = self.expression()?;
                        self.expect_punct("]")?;
                        callee = Expr::Member { obj: Box::new(callee), prop: Box::new(Key::Computed(Box::new(p))), optional: false };
                    } else {
                        break;
                    }
                }
                let args = if self.is_punct("(") { self.args()? } else { vec![] };
                Expr::New { callee: Box::new(callee), args }
            }
        } else {
            self.primary()?
        };
        let mut optional_chain = false;
        loop {
            if self.is_punct(".") {
                self.advance();
                let key = match self.peek().clone() {
                    Tok::PrivateName(n) => {
                        self.advance();
                        Key::Private(name(&n))
                    }
                    _ => Key::Name(self.ident_name()?),
                };
                e = Expr::Member { obj: Box::new(e), prop: Box::new(key), optional: false };
            } else if self.is_punct("?.") {
                self.advance();
                optional_chain = true;
                if self.is_punct("(") {
                    let args = self.args()?;
                    e = Expr::Call { callee: Box::new(e), args, optional: true };
                } else if self.eat_punct("[") {
                    let p = self.expression()?;
                    self.expect_punct("]")?;
                    e = Expr::Member { obj: Box::new(e), prop: Box::new(Key::Computed(Box::new(p))), optional: true };
                } else {
                    let key = match self.peek().clone() {
                        Tok::PrivateName(n) => {
                            self.advance();
                            Key::Private(name(&n))
                        }
                        _ => Key::Name(self.ident_name()?),
                    };
                    e = Expr::Member { obj: Box::new(e), prop: Box::new(key), optional: true };
                }
            } else if self.is_punct("[") {
                self.advance();
                let save = self.no_in;
                self.no_in = false;
                let p = self.expression()?;
                self.no_in = save;
                self.expect_punct("]")?;
                e = Expr::Member { obj: Box::new(e), prop: Box::new(Key::Computed(Box::new(p))), optional: false };
            } else if self.is_punct("(") {
                let args = self.args()?;
                e = Expr::Call { callee: Box::new(e), args, optional: false };
            } else if let Tok::Template(parts) = self.peek().clone() {
                if optional_chain {
                    return self.err("Invalid tagged template on optional chain");
                }
                self.advance();
                let (cooked, raw, exprs) = self.template_parts(parts)?;
                e = Expr::TaggedTemplate { tag: Box::new(e), cooked, raw, exprs };
            } else {
                break;
            }
        }
        if optional_chain {
            e = Expr::OptChain(Box::new(e));
        }
        Ok(e)
    }

    fn lhs_new_only(&mut self) -> PResult<Expr> {
        // `new new X()()` — rare; parse nested new as an lhs without trailing calls
        self.advance();
        let callee = self.primary()?;
        let args = if self.is_punct("(") { self.args()? } else { vec![] };
        Ok(Expr::New { callee: Box::new(callee), args })
    }

    fn template_parts(&mut self, parts: Vec<TemplatePart>) -> PResult<(Vec<Name>, Vec<Name>, Vec<Expr>)> {
        let mut cooked = Vec::new();
        let mut raw = Vec::new();
        let mut exprs = Vec::new();
        for p in parts {
            match p {
                TemplatePart::Str { cooked: c, raw: r } => {
                    cooked.push(name(&c));
                    raw.push(name(&r));
                }
                TemplatePart::Expr { src, line } => {
                    let toks = lexer::tokenize(&src, line)?;
                    let chars: Rc<[char]> = src.chars().collect::<Vec<_>>().into();
                    let mut sub = Parser { toks, pos: 0, src: chars, funcs: vec![], no_in: false, in_class_field: self.in_class_field, module: None };
                    // share function context flags (await/yield/arguments)
                    let f = self.funcs.last().map(|f| FuncCtx { var_names: vec![], uses_arguments: false, uses_this: false, is_async: f.is_async, is_generator: f.is_generator, is_arrow: true });
                    sub.funcs.push(f.unwrap_or(FuncCtx { var_names: vec![], uses_arguments: false, uses_this: false, is_async: false, is_generator: false, is_arrow: true }));
                    let e = sub.expression()?;
                    if !sub.at_eof() {
                        return sub.unexpected();
                    }
                    let ctx = sub.funcs.pop().unwrap();
                    if let Some(f) = self.funcs.last_mut() {
                        f.uses_arguments |= ctx.uses_arguments;
                        f.uses_this |= ctx.uses_this;
                    }
                    exprs.push(e);
                }
            }
        }
        Ok((cooked, raw, exprs))
    }

    fn primary(&mut self) -> PResult<Expr> {
        let start = self.pos;
        let line = self.line();
        match self.advance() {
            Tok::Num(n) => Ok(Expr::Num(n)),
            Tok::BigInt(s) => Ok(Expr::BigInt(name(&s))),
            Tok::Str(s) => Ok(Expr::Str(name(&s))),
            Tok::Template(parts) => {
                let (cooked, _, exprs) = self.template_parts(parts)?;
                Ok(Expr::Template { quasis: cooked, exprs })
            }
            Tok::Regex(p, f) => Ok(Expr::Regex { pattern: name(&p), flags: name(&f) }),
            Tok::Ident(s) => {
                if s == "async" && self.is_kw("function") && !self.tok().nl_before {
                    let f = self.function(true, false)?;
                    return Ok(Expr::Function(f));
                }
                if s == "arguments" {
                    if let Some(f) = self.funcs.iter_mut().rev().find(|f| !f.is_arrow) {
                        f.uses_arguments = true;
                    }
                }
                Ok(Expr::Ident(name(&s)))
            }
            Tok::Keyword("this") => {
                for f in self.funcs.iter_mut().rev() {
                    f.uses_this = true;
                    if !f.is_arrow {
                        break;
                    }
                }
                Ok(Expr::This)
            }
            Tok::Keyword("super") => Ok(Expr::Super),
            Tok::Keyword("null") => Ok(Expr::Null),
            Tok::Keyword("true") => Ok(Expr::Bool(true)),
            Tok::Keyword("false") => Ok(Expr::Bool(false)),
            Tok::Keyword("function") => {
                self.pos -= 1;
                Ok(Expr::Function(self.function(false, false)?))
            }
            Tok::Keyword("class") => {
                self.pos -= 1;
                Ok(Expr::Class(self.class(false)?))
            }
            Tok::Keyword("import") => {
                // `import.meta` and `import(...)` resolve to per-module bindings
                if self.is_punct(".") && matches!(self.peek_at(1), Tok::Ident(m) if m == "meta") {
                    self.advance();
                    self.advance();
                    return Ok(Expr::Ident(name("__tip_import_meta")));
                }
                Ok(Expr::Ident(name("__tip_import")))
            }
            Tok::Punct("(") => {
                let save = self.no_in;
                self.no_in = false;
                let e = self.expression()?;
                self.no_in = save;
                self.expect_punct(")")?;
                Ok(e)
            }
            Tok::Punct("[") => {
                let mut elems = Vec::new();
                let save = self.no_in;
                self.no_in = false;
                loop {
                    if self.eat_punct("]") {
                        break;
                    }
                    if self.is_punct(",") {
                        self.advance();
                        elems.push(ArrayElem::Hole);
                        continue;
                    }
                    if self.eat_punct("...") {
                        elems.push(ArrayElem::Spread(self.assign()?));
                    } else {
                        elems.push(ArrayElem::Item(self.assign()?));
                    }
                    if !self.eat_punct(",") {
                        self.expect_punct("]")?;
                        break;
                    }
                }
                self.no_in = save;
                Ok(Expr::Array(elems))
            }
            Tok::Punct("{") => self.object_literal(),
            _ => {
                self.pos = start;
                let _ = line;
                self.unexpected()
            }
        }
    }

    fn object_literal(&mut self) -> PResult<Expr> {
        let mut props = Vec::new();
        let save = self.no_in;
        self.no_in = false;
        loop {
            if self.eat_punct("}") {
                break;
            }
            if self.eat_punct("...") {
                props.push(ObjProp::Spread(self.assign()?));
            } else {
                let start = self.pos;
                let line = self.line();
                let mut is_async = false;
                let mut kind = MethodKind::Method;
                let next_is_key_end = |p: &Parser| matches!(p.peek_at(1), Tok::Punct(",") | Tok::Punct(":") | Tok::Punct("(") | Tok::Punct("}") | Tok::Punct("="));
                if self.is_ident("async") && !next_is_key_end(self) {
                    self.advance();
                    is_async = true;
                }
                let is_gen = self.eat_punct("*");
                if (self.is_ident("get") || self.is_ident("set")) && !next_is_key_end(self) {
                    kind = if self.is_ident("get") { MethodKind::Get } else { MethodKind::Set };
                    self.advance();
                }
                let key_tok = self.peek().clone();
                let key = self.prop_key()?;
                if self.is_punct("(") {
                    let mname = match &key {
                        Key::Name(n) => Some(n.clone()),
                        _ => None,
                    };
                    let f = self.function_rest(mname, is_async, is_gen, true, false, start, line)?;
                    props.push(ObjProp::Method(key, kind, f));
                } else if self.eat_punct(":") {
                    let v = self.assign()?;
                    props.push(ObjProp::KeyValue(key, v));
                } else if let (Tok::Ident(n), Key::Name(_)) = (&key_tok, &key) {
                    if n == "arguments" {
                        if let Some(f) = self.funcs.iter_mut().rev().find(|f| !f.is_arrow) {
                            f.uses_arguments = true;
                        }
                    }
                    if self.eat_punct("=") {
                        // CoverInitializedName: only valid in destructuring
                        let d = self.assign()?;
                        props.push(ObjProp::KeyValue(key.clone(), Expr::Assign(AssignOp::Assign, Box::new(Pattern::Ident(name(n))), Box::new(d))));
                    } else {
                        props.push(ObjProp::Shorthand(name(n)));
                    }
                } else {
                    return self.unexpected();
                }
            }
            if !self.eat_punct(",") {
                self.expect_punct("}")?;
                break;
            }
        }
        self.no_in = save;
        Ok(Expr::Object(props))
    }
}

fn d_or(d: Option<Expr>) -> Option<Expr> {
    d
}

fn is_lexical(s: &Stmt) -> bool {
    matches!(s.kind, StmtKind::Decl(DeclKind::Let | DeclKind::Const, _) | StmtKind::Class(_) | StmtKind::Function(_))
}

pub fn pattern_names(p: &Pattern, out: &mut Vec<Name>) {
    match p {
        Pattern::Ident(n) => out.push(n.clone()),
        Pattern::Object { props, rest } => {
            for (_, p, _) in props {
                pattern_names(p, out);
            }
            if let Some(r) = rest {
                pattern_names(r, out);
            }
        }
        Pattern::Array { elems, rest } => {
            for (p, _) in elems.iter().flatten() {
                pattern_names(p, out);
            }
            if let Some(r) = rest {
                pattern_names(r, out);
            }
        }
        Pattern::Expr(_) => {}
    }
}
