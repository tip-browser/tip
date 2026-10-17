//! CSS tokenizer, parser, selectors and selector matching.

use crate::dom::{Document, NodeId};

// ---------------------------------------------------------------- tokens

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Ident(String),
    Function(String),
    AtKeyword(String),
    Hash(String),
    Str(String),
    Url(String),
    Number(f32),
    Percentage(f32),
    Dimension(f32, String),
    Ws,
    Colon,
    Semicolon,
    Comma,
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Delim(char),
}

pub fn tokenize(src: &str) -> Vec<Tok> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let n = chars.len();
    let is_name_start = |c: char| c.is_alphabetic() || c == '_' || c == '-' || !c.is_ascii();
    let is_name = |c: char| c.is_alphanumeric() || c == '_' || c == '-' || !c.is_ascii();
    while i < n {
        let c = chars[i];
        // comments
        if c == '/' && i + 1 < n && chars[i + 1] == '*' {
            i += 2;
            while i + 1 < n && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i += 2;
            continue;
        }
        if c.is_whitespace() {
            while i < n && chars[i].is_whitespace() {
                i += 1;
            }
            out.push(Tok::Ws);
            continue;
        }
        if c == '"' || c == '\'' {
            i += 1;
            let mut s = String::new();
            while i < n && chars[i] != c {
                if chars[i] == '\\' && i + 1 < n {
                    i += 1;
                    let (ch, adv) = read_escape(&chars, i);
                    if let Some(ch) = ch {
                        s.push(ch);
                    }
                    i += adv;
                    continue;
                }
                if chars[i] == '\n' {
                    break;
                }
                s.push(chars[i]);
                i += 1;
            }
            i += 1;
            out.push(Tok::Str(s));
            continue;
        }
        let starts_number = c.is_ascii_digit()
            || (c == '.' && i + 1 < n && chars[i + 1].is_ascii_digit())
            || ((c == '-' || c == '+')
                && i + 1 < n
                && (chars[i + 1].is_ascii_digit() || (chars[i + 1] == '.' && i + 2 < n && chars[i + 2].is_ascii_digit())));
        if starts_number {
            let start = i;
            if c == '-' || c == '+' {
                i += 1;
            }
            while i < n && chars[i].is_ascii_digit() {
                i += 1;
            }
            if i < n && chars[i] == '.' && i + 1 < n && chars[i + 1].is_ascii_digit() {
                i += 1;
                while i < n && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            if i < n && (chars[i] == 'e' || chars[i] == 'E') && i + 1 < n && (chars[i + 1].is_ascii_digit() || ((chars[i + 1] == '-' || chars[i + 1] == '+') && i + 2 < n && chars[i + 2].is_ascii_digit())) {
                i += 2;
                while i < n && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            let num: f32 = chars[start..i].iter().collect::<String>().parse().unwrap_or(0.0);
            if i < n && chars[i] == '%' {
                i += 1;
                out.push(Tok::Percentage(num));
            } else if i < n && is_name_start(chars[i]) {
                let us = i;
                while i < n && is_name(chars[i]) {
                    i += 1;
                }
                out.push(Tok::Dimension(num, chars[us..i].iter().collect::<String>().to_ascii_lowercase()));
            } else {
                out.push(Tok::Number(num));
            }
            continue;
        }
        let dash_ident = c != '-' || (i + 1 < n && (is_name_start(chars[i + 1]) || chars[i + 1] == '\\'));
        if (is_name_start(c) && dash_ident) || (c == '\\') {
            let mut name = String::new();
            while i < n && (is_name(chars[i]) || chars[i] == '\\') {
                if chars[i] == '\\' && i + 1 < n {
                    i += 1;
                    let (ch, adv) = read_escape(&chars, i);
                    if let Some(ch) = ch {
                        name.push(ch);
                    }
                    i += adv;
                } else {
                    name.push(chars[i]);
                    i += 1;
                }
            }
            if i < n && chars[i] == '(' {
                i += 1;
                if name.eq_ignore_ascii_case("url") {
                    // unquoted url(...)
                    let mut j = i;
                    while j < n && chars[j].is_whitespace() {
                        j += 1;
                    }
                    if j < n && chars[j] != '"' && chars[j] != '\'' {
                        let mut s = String::new();
                        while j < n && chars[j] != ')' {
                            s.push(chars[j]);
                            j += 1;
                        }
                        i = j + 1;
                        out.push(Tok::Url(s.trim().to_string()));
                        continue;
                    }
                }
                out.push(Tok::Function(name.to_ascii_lowercase()));
            } else {
                out.push(Tok::Ident(name));
            }
            continue;
        }
        i += 1;
        match c {
            '@' => {
                let s = i;
                while i < n && is_name(chars[i]) {
                    i += 1;
                }
                out.push(Tok::AtKeyword(chars[s..i].iter().collect::<String>().to_ascii_lowercase()));
            }
            '#' => {
                let s = i;
                while i < n && is_name(chars[i]) {
                    i += 1;
                }
                out.push(Tok::Hash(chars[s..i].iter().collect()));
            }
            ':' => out.push(Tok::Colon),
            ';' => out.push(Tok::Semicolon),
            ',' => out.push(Tok::Comma),
            '{' => out.push(Tok::LBrace),
            '}' => out.push(Tok::RBrace),
            '(' => out.push(Tok::LParen),
            ')' => out.push(Tok::RParen),
            '[' => out.push(Tok::LBracket),
            ']' => out.push(Tok::RBracket),
            _ => out.push(Tok::Delim(c)),
        }
    }
    out
}

fn read_escape(chars: &[char], i: usize) -> (Option<char>, usize) {
    let mut j = i;
    let mut hex = String::new();
    while j < chars.len() && hex.len() < 6 && chars[j].is_ascii_hexdigit() {
        hex.push(chars[j]);
        j += 1;
    }
    if !hex.is_empty() {
        if j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        let c = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32);
        return (c, j - i);
    }
    if chars[i] == '\n' {
        return (None, 1);
    }
    (Some(chars[i]), 1)
}

pub fn tokens_to_string(toks: &[Tok]) -> String {
    let mut s = String::new();
    for t in toks {
        match t {
            Tok::Ident(x) => s.push_str(x),
            Tok::Function(x) => {
                s.push_str(x);
                s.push('(');
            }
            Tok::AtKeyword(x) => {
                s.push('@');
                s.push_str(x);
            }
            Tok::Hash(x) => {
                s.push('#');
                s.push_str(x);
            }
            Tok::Str(x) => {
                s.push('"');
                s.push_str(x);
                s.push('"');
            }
            Tok::Url(x) => s.push_str(&format!("url({x})")),
            Tok::Number(n) => s.push_str(&fmt_num(*n)),
            Tok::Percentage(n) => {
                s.push_str(&fmt_num(*n));
                s.push('%');
            }
            Tok::Dimension(n, u) => {
                s.push_str(&fmt_num(*n));
                s.push_str(u);
            }
            Tok::Ws => s.push(' '),
            Tok::Colon => s.push(':'),
            Tok::Semicolon => s.push(';'),
            Tok::Comma => s.push(','),
            Tok::LBrace => s.push('{'),
            Tok::RBrace => s.push('}'),
            Tok::LParen => s.push('('),
            Tok::RParen => s.push(')'),
            Tok::LBracket => s.push('['),
            Tok::RBracket => s.push(']'),
            Tok::Delim(c) => s.push(*c),
        }
    }
    s.trim().to_string()
}

pub fn fmt_num(n: f32) -> String {
    if n.fract() == 0.0 && n.abs() < 1e9 {
        format!("{}", n as i64)
    } else {
        let s = format!("{:.4}", n);
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

// ---------------------------------------------------------------- stylesheet model

#[derive(Debug, Clone)]
pub struct Declaration {
    pub name: String,
    pub value: Vec<Tok>,
    pub important: bool,
}

impl Declaration {
    pub fn value_string(&self) -> String {
        tokens_to_string(&self.value)
    }
}

#[derive(Debug, Clone)]
pub struct StyleRule {
    pub selectors: Vec<Selector>,
    pub selector_text: String,
    pub decls: Vec<Declaration>,
    pub media: Option<MediaQuery>,
}

#[derive(Debug, Clone, Default)]
pub struct Stylesheet {
    pub rules: Vec<StyleRule>,
    pub imports: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct MediaQuery {
    /// OR of ANDed conditions.
    pub any: Vec<Vec<MediaCond>>,
}

#[derive(Debug, Clone)]
pub enum MediaCond {
    Always,
    Never,
    MinWidth(f32),
    MaxWidth(f32),
    MinHeight(f32),
    MaxHeight(f32),
    Not(Box<MediaCond>),
}

impl MediaQuery {
    pub fn matches(&self, vw: f32, vh: f32) -> bool {
        self.any.iter().any(|conds| conds.iter().all(|c| c.eval(vw, vh)))
    }
}

impl MediaCond {
    fn eval(&self, vw: f32, vh: f32) -> bool {
        match self {
            MediaCond::Always => true,
            MediaCond::Never => false,
            MediaCond::MinWidth(w) => vw >= *w,
            MediaCond::MaxWidth(w) => vw <= *w,
            MediaCond::MinHeight(h) => vh >= *h,
            MediaCond::MaxHeight(h) => vh <= *h,
            MediaCond::Not(c) => !c.eval(vw, vh),
        }
    }
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }
    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(Tok::Ws)) {
            self.pos += 1;
        }
    }

    /// Consume tokens up to (not including) a top-level `{` or `;`.
    fn prelude(&mut self) -> Vec<Tok> {
        let mut out = Vec::new();
        let mut depth = 0;
        while let Some(t) = self.peek() {
            match t {
                Tok::LBrace | Tok::Semicolon if depth == 0 => break,
                Tok::LParen | Tok::Function(_) | Tok::LBracket => depth += 1,
                Tok::RParen | Tok::RBracket => depth -= 1,
                _ => {}
            }
            out.push(self.next().unwrap());
        }
        out
    }

    /// Consume a `{ ... }` block, returning the inner tokens.
    fn block(&mut self) -> Vec<Tok> {
        // assumes current token is LBrace
        self.pos += 1;
        let mut out = Vec::new();
        let mut depth = 1;
        while let Some(t) = self.next() {
            match t {
                Tok::LBrace => depth += 1,
                Tok::RBrace => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            out.push(t);
        }
        out
    }

    fn rules(&mut self, media: Option<MediaQuery>, sheet: &mut Stylesheet) {
        loop {
            self.skip_ws();
            let Some(t) = self.peek().cloned() else { break };
            match t {
                Tok::AtKeyword(kw) => {
                    self.pos += 1;
                    let prelude = self.prelude();
                    match self.peek() {
                        Some(Tok::Semicolon) => {
                            self.pos += 1;
                            if kw == "import" {
                                for t in &prelude {
                                    match t {
                                        Tok::Str(s) | Tok::Url(s) => {
                                            sheet.imports.push(s.clone());
                                            break;
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                        Some(Tok::LBrace) => {
                            let inner = self.block();
                            match kw.as_str() {
                                "media" => {
                                    let mq = parse_media_query(&prelude);
                                    let combined = combine_media(media.clone(), mq);
                                    let mut p = Parser { toks: inner, pos: 0 };
                                    p.rules(Some(combined), sheet);
                                }
                                "supports" | "layer" | "document" | "container" => {
                                    let mut p = Parser { toks: inner, pos: 0 };
                                    p.rules(media.clone(), sheet);
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
                Tok::Delim('<') | Tok::Delim('-') => {
                    // CDO/CDC tokens: skip.
                    self.pos += 1;
                }
                _ => {
                    let prelude = self.prelude();
                    if !matches!(self.peek(), Some(Tok::LBrace)) {
                        self.pos += 1;
                        continue;
                    }
                    let inner = self.block();
                    let selector_text = tokens_to_string(&prelude);
                    if let Some(selectors) = parse_selector_list(&prelude) {
                        let decls = parse_declarations_toks(inner);
                        sheet.rules.push(StyleRule { selectors, selector_text, decls, media: media.clone() });
                    }
                }
            }
        }
    }
}

fn combine_media(outer: Option<MediaQuery>, inner: MediaQuery) -> MediaQuery {
    match outer {
        None => inner,
        Some(o) => {
            let mut any = Vec::new();
            for a in &o.any {
                for b in &inner.any {
                    let mut c = a.clone();
                    c.extend(b.iter().cloned());
                    any.push(c);
                }
            }
            MediaQuery { any }
        }
    }
}

fn parse_media_query(toks: &[Tok]) -> MediaQuery {
    let mut any = Vec::new();
    for part in toks.split(|t| *t == Tok::Comma) {
        let mut conds = Vec::new();
        let mut negate = false;
        let mut i = 0;
        while i < part.len() {
            match &part[i] {
                Tok::Ident(id) => match id.to_ascii_lowercase().as_str() {
                    "not" => negate = true,
                    "only" | "and" | "screen" | "all" => {}
                    "print" | "speech" => conds.push(MediaCond::Never),
                    _ => conds.push(MediaCond::Never),
                },
                Tok::LParen => {
                    let mut j = i + 1;
                    let mut inner = Vec::new();
                    while j < part.len() && part[j] != Tok::RParen {
                        if part[j] != Tok::Ws {
                            inner.push(part[j].clone());
                        }
                        j += 1;
                    }
                    conds.push(parse_media_feature(&inner));
                    i = j;
                }
                _ => {}
            }
            i += 1;
        }
        if conds.is_empty() {
            conds.push(MediaCond::Always);
        }
        if negate {
            conds = vec![MediaCond::Not(Box::new(if conds.len() == 1 { conds.remove(0) } else { MediaCond::Never }))];
        }
        any.push(conds);
    }
    if any.is_empty() {
        any.push(vec![MediaCond::Always]);
    }
    MediaQuery { any }
}

fn parse_media_feature(t: &[Tok]) -> MediaCond {
    let name = match t.first() {
        Some(Tok::Ident(n)) => n.to_ascii_lowercase(),
        _ => return MediaCond::Never,
    };
    let val = t.get(2).and_then(|v| match v {
        Tok::Dimension(n, u) => Some(match u.as_str() {
            "em" | "rem" => n * 16.0,
            _ => *n,
        }),
        Tok::Number(n) => Some(*n),
        _ => None,
    });
    let ident = t.get(2).and_then(|v| if let Tok::Ident(s) = v { Some(s.to_ascii_lowercase()) } else { None });
    match (name.as_str(), val) {
        ("min-width", Some(v)) => MediaCond::MinWidth(v),
        ("max-width", Some(v)) => MediaCond::MaxWidth(v),
        ("min-height", Some(v)) => MediaCond::MinHeight(v),
        ("max-height", Some(v)) => MediaCond::MaxHeight(v),
        ("prefers-color-scheme", _) => {
            if ident.as_deref() == Some("light") { MediaCond::Always } else { MediaCond::Never }
        }
        ("prefers-reduced-motion", _) => {
            if ident.as_deref() == Some("no-preference") { MediaCond::Always } else { MediaCond::Never }
        }
        ("hover", _) | ("pointer", _) => {
            if matches!(ident.as_deref(), Some("hover" | "fine")) { MediaCond::Always } else { MediaCond::Never }
        }
        ("orientation", _) => MediaCond::Always,
        ("color", _) => MediaCond::Always,
        _ => MediaCond::Never,
    }
}

pub fn parse_stylesheet(src: &str) -> Stylesheet {
    let mut sheet = Stylesheet::default();
    let mut p = Parser { toks: tokenize(src), pos: 0 };
    p.rules(None, &mut sheet);
    sheet
}

/// Parse a declaration list such as the contents of a `style` attribute.
pub fn parse_declarations(src: &str) -> Vec<Declaration> {
    parse_declarations_toks(tokenize(src))
}

fn parse_declarations_toks(toks: Vec<Tok>) -> Vec<Declaration> {
    let mut out = Vec::new();
    let mut parts: Vec<Vec<Tok>> = vec![vec![]];
    let mut depth = 0;
    for t in toks {
        match t {
            Tok::Semicolon if depth == 0 => {
                parts.push(vec![]);
                continue;
            }
            Tok::LParen | Tok::Function(_) | Tok::LBrace | Tok::LBracket => depth += 1,
            Tok::RParen | Tok::RBrace | Tok::RBracket => depth -= 1,
            _ => {}
        }
        parts.last_mut().unwrap().push(t);
    }
    for part in parts {
        let mut it = part.into_iter().skip_while(|t| *t == Tok::Ws);
        let name = match it.next() {
            Some(Tok::Ident(n)) => n,
            _ => continue,
        };
        let rest: Vec<Tok> = it.collect();
        let mut rest: Vec<Tok> = rest.into_iter().skip_while(|t| *t == Tok::Ws).collect();
        if rest.first() != Some(&Tok::Colon) {
            continue;
        }
        rest.remove(0);
        let mut value: Vec<Tok> = rest;
        while value.last() == Some(&Tok::Ws) {
            value.pop();
        }
        let mut important = false;
        if let Some(pos) = value.iter().rposition(|t| *t == Tok::Delim('!')) {
            if matches!(value.get(pos + 1), Some(Tok::Ident(s)) if s.eq_ignore_ascii_case("important")) {
                important = true;
                value.truncate(pos);
                while value.last() == Some(&Tok::Ws) {
                    value.pop();
                }
            }
        }
        while value.first() == Some(&Tok::Ws) {
            value.remove(0);
        }
        let name = if name.starts_with("--") { name } else { name.to_ascii_lowercase() };
        out.push(Declaration { name, value, important });
    }
    out
}

// ---------------------------------------------------------------- selectors

#[derive(Debug, Clone, PartialEq)]
pub enum Combinator {
    Descendant,
    Child,
    NextSibling,
    Subsequent,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AttrOp {
    Exists,
    Eq,
    Includes,
    DashMatch,
    Prefix,
    Suffix,
    Substring,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AttrSel {
    pub name: String,
    pub op: AttrOp,
    pub value: String,
    pub case_insensitive: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pseudo {
    Hover,
    Focus,
    FocusWithin,
    FocusVisible,
    Active,
    FirstChild,
    LastChild,
    OnlyChild,
    FirstOfType,
    LastOfType,
    OnlyOfType,
    NthChild(i32, i32),
    NthLastChild(i32, i32),
    NthOfType(i32, i32),
    NthLastOfType(i32, i32),
    Not(Vec<Selector>),
    Is(Vec<Selector>),
    Where(Vec<Selector>),
    Has(Vec<Selector>),
    Checked,
    Disabled,
    Enabled,
    Required,
    Root,
    Empty,
    Link,
    Never,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Compound {
    pub tag: Option<String>,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub attrs: Vec<AttrSel>,
    pub pseudos: Vec<Pseudo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PseudoElement {
    Before,
    After,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Selector {
    /// Rightmost compound first; each entry's combinator links it to the next (leftward) compound.
    pub parts: Vec<(Compound, Option<Combinator>)>,
    pub pseudo_element: Option<PseudoElement>,
}

impl Selector {
    pub fn specificity(&self) -> (u32, u32, u32) {
        let mut s = (0, 0, 0);
        for (c, _) in &self.parts {
            let cs = compound_specificity(c);
            s.0 += cs.0;
            s.1 += cs.1;
            s.2 += cs.2;
        }
        if self.pseudo_element.is_some() {
            s.2 += 1;
        }
        s
    }
}

fn max_spec(list: &[Selector]) -> (u32, u32, u32) {
    list.iter().map(|s| s.specificity()).max().unwrap_or((0, 0, 0))
}

fn compound_specificity(c: &Compound) -> (u32, u32, u32) {
    let mut s = (0, 0, 0);
    if c.id.is_some() {
        s.0 += 1;
    }
    s.1 += c.classes.len() as u32 + c.attrs.len() as u32;
    for p in &c.pseudos {
        match p {
            Pseudo::Not(l) | Pseudo::Is(l) | Pseudo::Has(l) => {
                let m = max_spec(l);
                s.0 += m.0;
                s.1 += m.1;
                s.2 += m.2;
            }
            Pseudo::Where(_) => {}
            _ => s.1 += 1,
        }
    }
    if c.tag.is_some() {
        s.2 += 1;
    }
    s
}

pub fn parse_selector_list_str(src: &str) -> Option<Vec<Selector>> {
    parse_selector_list(&tokenize(src))
}

pub fn parse_selector_list(toks: &[Tok]) -> Option<Vec<Selector>> {
    let mut out = Vec::new();
    for part in split_top_level_commas(toks) {
        out.push(parse_selector(&part)?);
    }
    if out.is_empty() { None } else { Some(out) }
}

fn split_top_level_commas(toks: &[Tok]) -> Vec<Vec<Tok>> {
    let mut parts = vec![vec![]];
    let mut depth = 0;
    for t in toks {
        match t {
            Tok::Comma if depth == 0 => {
                parts.push(vec![]);
                continue;
            }
            Tok::Function(_) | Tok::LParen | Tok::LBracket => depth += 1,
            Tok::RParen | Tok::RBracket => depth -= 1,
            _ => {}
        }
        parts.last_mut().unwrap().push(t.clone());
    }
    parts.retain(|p| p.iter().any(|t| *t != Tok::Ws));
    parts
}

fn parse_selector(toks: &[Tok]) -> Option<Selector> {
    let mut compounds: Vec<(Compound, Option<Combinator>)> = Vec::new();
    let mut cur = Compound::default();
    let mut cur_empty = true;
    let mut pending_comb: Option<Combinator> = None;
    let mut pseudo_element = None;
    let mut i = 0;
    let toks: Vec<Tok> = {
        // trim
        let mut v = toks.to_vec();
        while v.first() == Some(&Tok::Ws) {
            v.remove(0);
        }
        while v.last() == Some(&Tok::Ws) {
            v.pop();
        }
        v
    };
    let flush = |cur: &mut Compound, cur_empty: &mut bool, comb: Option<Combinator>, compounds: &mut Vec<(Compound, Option<Combinator>)>| {
        compounds.push((std::mem::take(cur), comb));
        *cur_empty = true;
    };
    while i < toks.len() {
        let t = &toks[i];
        match t {
            Tok::Ws => {
                if !cur_empty {
                    // could be descendant combinator, unless followed by explicit combinator
                    let mut j = i;
                    while j < toks.len() && toks[j] == Tok::Ws {
                        j += 1;
                    }
                    let explicit = matches!(toks.get(j), Some(Tok::Delim('>' | '+' | '~')));
                    if !explicit {
                        flush(&mut cur, &mut cur_empty, pending_comb.take(), &mut compounds);
                        pending_comb = Some(Combinator::Descendant);
                    }
                    i = j;
                    continue;
                }
            }
            Tok::Delim(c @ ('>' | '+' | '~')) => {
                if !cur_empty {
                    flush(&mut cur, &mut cur_empty, pending_comb.take(), &mut compounds);
                }
                pending_comb = Some(match c {
                    '>' => Combinator::Child,
                    '+' => Combinator::NextSibling,
                    _ => Combinator::Subsequent,
                });
                i += 1;
                while i < toks.len() && toks[i] == Tok::Ws {
                    i += 1;
                }
                continue;
            }
            Tok::Ident(name) => {
                cur.tag = Some(name.to_ascii_lowercase());
                cur_empty = false;
            }
            Tok::Delim('*') => {
                cur_empty = false;
            }
            Tok::Hash(h) => {
                cur.id = Some(h.clone());
                cur_empty = false;
            }
            Tok::Delim('.') => {
                i += 1;
                match toks.get(i) {
                    Some(Tok::Ident(c)) => cur.classes.push(c.clone()),
                    _ => return None,
                }
                cur_empty = false;
            }
            Tok::LBracket => {
                let mut j = i + 1;
                let mut inner = Vec::new();
                while j < toks.len() && toks[j] != Tok::RBracket {
                    if toks[j] != Tok::Ws {
                        inner.push(toks[j].clone());
                    }
                    j += 1;
                }
                i = j;
                cur.attrs.push(parse_attr_sel(&inner)?);
                cur_empty = false;
            }
            Tok::Colon => {
                i += 1;
                let double = toks.get(i) == Some(&Tok::Colon);
                if double {
                    i += 1;
                }
                match toks.get(i) {
                    Some(Tok::Ident(name)) => {
                        let name = name.to_ascii_lowercase();
                        if double || matches!(name.as_str(), "before" | "after" | "first-line" | "first-letter") {
                            pseudo_element = Some(match name.as_str() {
                                "before" => PseudoElement::Before,
                                "after" => PseudoElement::After,
                                _ => PseudoElement::Other,
                            });
                        } else {
                            cur.pseudos.push(match name.as_str() {
                                "hover" => Pseudo::Hover,
                                "focus" => Pseudo::Focus,
                                "focus-within" => Pseudo::FocusWithin,
                                "focus-visible" => Pseudo::FocusVisible,
                                "active" => Pseudo::Active,
                                "first-child" => Pseudo::FirstChild,
                                "last-child" => Pseudo::LastChild,
                                "only-child" => Pseudo::OnlyChild,
                                "first-of-type" => Pseudo::FirstOfType,
                                "last-of-type" => Pseudo::LastOfType,
                                "only-of-type" => Pseudo::OnlyOfType,
                                "checked" => Pseudo::Checked,
                                "disabled" => Pseudo::Disabled,
                                "enabled" => Pseudo::Enabled,
                                "required" => Pseudo::Required,
                                "root" => Pseudo::Root,
                                "empty" => Pseudo::Empty,
                                "link" | "any-link" => Pseudo::Link,
                                _ => Pseudo::Never,
                            });
                        }
                        cur_empty = false;
                    }
                    Some(Tok::Function(f)) => {
                        let f = f.clone();
                        let mut j = i + 1;
                        let mut depth = 1;
                        let mut inner = Vec::new();
                        while j < toks.len() {
                            match &toks[j] {
                                Tok::Function(_) | Tok::LParen => depth += 1,
                                Tok::RParen => {
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                }
                                _ => {}
                            }
                            inner.push(toks[j].clone());
                            j += 1;
                        }
                        i = j;
                        if double {
                            pseudo_element = Some(PseudoElement::Other);
                        } else {
                            cur.pseudos.push(match f.as_str() {
                                "not" => Pseudo::Not(parse_selector_list(&inner)?),
                                "is" | "matches" | "-webkit-any" => Pseudo::Is(parse_selector_list(&inner).unwrap_or_default()),
                                "where" => Pseudo::Where(parse_selector_list(&inner).unwrap_or_default()),
                                "has" => Pseudo::Has(parse_selector_list(&inner).unwrap_or_default()),
                                "nth-child" => {
                                    let (a, b) = parse_nth(&inner)?;
                                    Pseudo::NthChild(a, b)
                                }
                                "nth-last-child" => {
                                    let (a, b) = parse_nth(&inner)?;
                                    Pseudo::NthLastChild(a, b)
                                }
                                "nth-of-type" => {
                                    let (a, b) = parse_nth(&inner)?;
                                    Pseudo::NthOfType(a, b)
                                }
                                "nth-last-of-type" => {
                                    let (a, b) = parse_nth(&inner)?;
                                    Pseudo::NthLastOfType(a, b)
                                }
                                _ => Pseudo::Never,
                            });
                        }
                        cur_empty = false;
                    }
                    _ => return None,
                }
            }
            _ => return None,
        }
        i += 1;
    }
    if cur_empty && !compounds.is_empty() && pending_comb.is_some() {
        return None;
    }
    compounds.push((cur, pending_comb));
    // compounds are left-to-right with combinator that precedes each; convert to rightmost-first,
    // where each entry's combinator links to the next entry (its left neighbour).
    let n = compounds.len();
    let mut parts = Vec::with_capacity(n);
    for idx in (0..n).rev() {
        let comb = compounds[idx].1.clone();
        parts.push((compounds[idx].0.clone(), comb));
    }
    // The leftmost compound must not have a combinator.
    if let Some(last) = parts.last_mut() {
        last.1 = None;
    }
    Some(Selector { parts, pseudo_element })
}

fn parse_attr_sel(inner: &[Tok]) -> Option<AttrSel> {
    let name = match inner.first()? {
        Tok::Ident(n) => n.to_ascii_lowercase(),
        _ => return None,
    };
    if inner.len() == 1 {
        return Some(AttrSel { name, op: AttrOp::Exists, value: String::new(), case_insensitive: false });
    }
    let (op, vi) = match (&inner.get(1)?, inner.get(2)) {
        (Tok::Delim('='), _) => (AttrOp::Eq, 2),
        (Tok::Delim('~'), Some(Tok::Delim('='))) => (AttrOp::Includes, 3),
        (Tok::Delim('|'), Some(Tok::Delim('='))) => (AttrOp::DashMatch, 3),
        (Tok::Delim('^'), Some(Tok::Delim('='))) => (AttrOp::Prefix, 3),
        (Tok::Delim('$'), Some(Tok::Delim('='))) => (AttrOp::Suffix, 3),
        (Tok::Delim('*'), Some(Tok::Delim('='))) => (AttrOp::Substring, 3),
        _ => return None,
    };
    let value = match inner.get(vi)? {
        Tok::Ident(s) | Tok::Str(s) => s.clone(),
        Tok::Number(n) => fmt_num(*n),
        _ => return None,
    };
    let case_insensitive = matches!(inner.get(vi + 1), Some(Tok::Ident(f)) if f.eq_ignore_ascii_case("i"));
    Some(AttrSel { name, op, value, case_insensitive })
}

fn parse_nth(inner: &[Tok]) -> Option<(i32, i32)> {
    let s = tokens_to_string(inner).replace(' ', "").to_ascii_lowercase();
    match s.as_str() {
        "odd" => return Some((2, 1)),
        "even" => return Some((2, 0)),
        _ => {}
    }
    if let Some(npos) = s.find('n') {
        let a_str = &s[..npos];
        let a = match a_str {
            "" | "+" => 1,
            "-" => -1,
            _ => a_str.parse().ok()?,
        };
        let b_str = &s[npos + 1..];
        let b = if b_str.is_empty() { 0 } else { b_str.trim_start_matches('+').parse().ok()? };
        Some((a, b))
    } else {
        Some((0, s.parse().ok()?))
    }
}

// ---------------------------------------------------------------- matching

#[derive(Debug, Clone, Copy, Default)]
pub struct MatchState {
    pub hover: Option<NodeId>,
    pub focus: Option<NodeId>,
    pub active: Option<NodeId>,
    /// Whether the focused element shows a focus ring (`:focus-visible`).
    pub focus_visible: bool,
}

pub fn matches_any(doc: &Document, node: NodeId, sels: &[Selector], st: &MatchState) -> bool {
    sels.iter().any(|s| s.pseudo_element.is_none() && matches_selector(doc, node, s, st))
}

pub fn matches_selector(doc: &Document, node: NodeId, sel: &Selector, st: &MatchState) -> bool {
    match_from(doc, node, &sel.parts, 0, st)
}

fn match_from(doc: &Document, node: NodeId, parts: &[(Compound, Option<Combinator>)], idx: usize, st: &MatchState) -> bool {
    let (compound, comb) = &parts[idx];
    if !matches_compound(doc, node, compound, st) {
        return false;
    }
    let Some(comb) = comb else { return true };
    if idx + 1 >= parts.len() {
        return true;
    }
    match comb {
        Combinator::Child => doc.parent(node).is_some_and(|p| doc.is_element(p) && match_from(doc, p, parts, idx + 1, st)),
        Combinator::Descendant => {
            let mut cur = doc.parent(node);
            while let Some(p) = cur {
                if !doc.is_element(p) {
                    return false;
                }
                if match_from(doc, p, parts, idx + 1, st) {
                    return true;
                }
                cur = doc.parent(p);
            }
            false
        }
        Combinator::NextSibling => doc.prev_element_sibling(node).is_some_and(|s| match_from(doc, s, parts, idx + 1, st)),
        Combinator::Subsequent => {
            let mut cur = doc.prev_element_sibling(node);
            while let Some(s) = cur {
                if match_from(doc, s, parts, idx + 1, st) {
                    return true;
                }
                cur = doc.prev_element_sibling(s);
            }
            false
        }
    }
}

fn nth_matches(a: i32, b: i32, pos: i32) -> bool {
    // pos is 1-based
    if a == 0 {
        return pos == b;
    }
    let diff = pos - b;
    diff % a == 0 && diff / a >= 0
}

fn matches_compound(doc: &Document, node: NodeId, c: &Compound, st: &MatchState) -> bool {
    let Some(el) = doc.element(node) else { return false };
    if let Some(tag) = &c.tag {
        if *tag != el.tag {
            return false;
        }
    }
    if let Some(id) = &c.id {
        if doc.attr(node, "id") != Some(id.as_str()) {
            return false;
        }
    }
    for cls in &c.classes {
        if !doc.has_class(node, cls) {
            return false;
        }
    }
    for a in &c.attrs {
        let Some(v) = doc.attr(node, &a.name) else { return false };
        let (v, want) = if a.case_insensitive { (v.to_lowercase(), a.value.to_lowercase()) } else { (v.to_string(), a.value.clone()) };
        let ok = match a.op {
            AttrOp::Exists => true,
            AttrOp::Eq => v == want,
            AttrOp::Includes => v.split_whitespace().any(|x| x == want),
            AttrOp::DashMatch => v == want || v.starts_with(&format!("{want}-")),
            AttrOp::Prefix => !want.is_empty() && v.starts_with(&want),
            AttrOp::Suffix => !want.is_empty() && v.ends_with(&want),
            AttrOp::Substring => !want.is_empty() && v.contains(&want),
        };
        if !ok {
            return false;
        }
    }
    for p in &c.pseudos {
        if !matches_pseudo(doc, node, p, st) {
            return false;
        }
    }
    true
}

fn sibling_position(doc: &Document, node: NodeId, of_type: bool, from_end: bool) -> (i32, i32) {
    let Some(parent) = doc.parent(node) else { return (1, 1) };
    let tag = doc.tag(node);
    let sibs: Vec<NodeId> = doc
        .children(parent)
        .iter()
        .copied()
        .filter(|&s| doc.is_element(s) && (!of_type || doc.tag(s) == tag))
        .collect();
    let idx = sibs.iter().position(|&s| s == node).unwrap_or(0) as i32;
    let total = sibs.len() as i32;
    if from_end { (total - idx, total) } else { (idx + 1, total) }
}

fn matches_pseudo(doc: &Document, node: NodeId, p: &Pseudo, st: &MatchState) -> bool {
    match p {
        Pseudo::Hover => st.hover.is_some_and(|h| doc.is_ancestor(node, h)),
        Pseudo::Active => st.active.is_some_and(|h| doc.is_ancestor(node, h)),
        Pseudo::Focus => st.focus == Some(node),
        Pseudo::FocusVisible => st.focus_visible && st.focus == Some(node),
        Pseudo::FocusWithin => st.focus.is_some_and(|f| doc.is_ancestor(node, f)),
        Pseudo::FirstChild => sibling_position(doc, node, false, false).0 == 1,
        Pseudo::LastChild => sibling_position(doc, node, false, true).0 == 1,
        Pseudo::OnlyChild => sibling_position(doc, node, false, false).1 == 1,
        Pseudo::FirstOfType => sibling_position(doc, node, true, false).0 == 1,
        Pseudo::LastOfType => sibling_position(doc, node, true, true).0 == 1,
        Pseudo::OnlyOfType => sibling_position(doc, node, true, false).1 == 1,
        Pseudo::NthChild(a, b) => nth_matches(*a, *b, sibling_position(doc, node, false, false).0),
        Pseudo::NthLastChild(a, b) => nth_matches(*a, *b, sibling_position(doc, node, false, true).0),
        Pseudo::NthOfType(a, b) => nth_matches(*a, *b, sibling_position(doc, node, true, false).0),
        Pseudo::NthLastOfType(a, b) => nth_matches(*a, *b, sibling_position(doc, node, true, true).0),
        Pseudo::Not(list) => !list.iter().any(|s| matches_selector(doc, node, s, st)),
        Pseudo::Is(list) | Pseudo::Where(list) => list.iter().any(|s| matches_selector(doc, node, s, st)),
        Pseudo::Has(list) => doc.element_descendants(node).into_iter().any(|d| list.iter().any(|s| matches_selector(doc, d, s, st))),
        Pseudo::Checked => doc.has_attr(node, "checked") || (doc.tag_is(node, "option") && doc.has_attr(node, "selected")),
        Pseudo::Disabled => doc.has_attr(node, "disabled"),
        Pseudo::Enabled => matches!(doc.tag(node), Some("input" | "button" | "select" | "textarea")) && !doc.has_attr(node, "disabled"),
        Pseudo::Required => doc.has_attr(node, "required"),
        Pseudo::Root => doc.parent(node) == Some(0),
        Pseudo::Empty => doc.children(node).iter().all(|&c| matches!(&doc.node(c).data, crate::dom::NodeData::Comment(_))),
        Pseudo::Link => doc.tag_is(node, "a") && doc.has_attr(node, "href"),
        Pseudo::Never => false,
    }
}

pub fn query_selector_all(doc: &Document, root: NodeId, selector: &str) -> Result<Vec<NodeId>, String> {
    let sels = parse_selector_list_str(selector).ok_or_else(|| format!("'{selector}' is not a valid selector"))?;
    let st = MatchState::default();
    Ok(doc.element_descendants(root).into_iter().filter(|&n| matches_any(doc, n, &sels, &st)).collect())
}

pub fn query_selector(doc: &Document, root: NodeId, selector: &str) -> Result<Option<NodeId>, String> {
    let sels = parse_selector_list_str(selector).ok_or_else(|| format!("'{selector}' is not a valid selector"))?;
    let st = MatchState::default();
    Ok(doc.element_descendants(root).into_iter().find(|&n| matches_any(doc, n, &sels, &st)))
}

// ---------------------------------------------------------------- colors

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const BLACK: Color = Color { r: 0, g: 0, b: 0, a: 255 };
    pub const WHITE: Color = Color { r: 255, g: 255, b: 255, a: 255 };
    pub const TRANSPARENT: Color = Color { r: 0, g: 0, b: 0, a: 0 };

    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color { r, g, b, a: 255 }
    }
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
        Color { r, g, b, a }
    }
    pub fn with_alpha_mul(self, f: f32) -> Color {
        Color { a: (self.a as f32 * f).round().clamp(0.0, 255.0) as u8, ..self }
    }
    pub fn to_css(self) -> String {
        if self.a == 255 {
            format!("rgb({}, {}, {})", self.r, self.g, self.b)
        } else {
            format!("rgba({}, {}, {}, {})", self.r, self.g, self.b, fmt_num(self.a as f32 / 255.0))
        }
    }
}

pub fn parse_hex_color(h: &str) -> Option<Color> {
    let hex = |s: &str| u8::from_str_radix(s, 16).ok();
    let dup = |c: char| -> Option<u8> { hex(&format!("{c}{c}")) };
    let cs: Vec<char> = h.chars().collect();
    match cs.len() {
        3 => Some(Color::rgb(dup(cs[0])?, dup(cs[1])?, dup(cs[2])?)),
        4 => Some(Color::rgba(dup(cs[0])?, dup(cs[1])?, dup(cs[2])?, dup(cs[3])?)),
        6 => Some(Color::rgb(hex(&h[0..2])?, hex(&h[2..4])?, hex(&h[4..6])?)),
        8 => Some(Color::rgba(hex(&h[0..2])?, hex(&h[2..4])?, hex(&h[4..6])?, hex(&h[6..8])?)),
        _ => None,
    }
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let h = ((h % 360.0) + 360.0) % 360.0 / 360.0;
    let hue = |p: f32, q: f32, mut t: f32| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    let (r, g, b) = if s == 0.0 {
        (l, l, l)
    } else {
        let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
        let p = 2.0 * l - q;
        (hue(p, q, h + 1.0 / 3.0), hue(p, q, h), hue(p, q, h - 1.0 / 3.0))
    };
    ((r * 255.0).round() as u8, (g * 255.0).round() as u8, (b * 255.0).round() as u8)
}

/// Parse a color from a slice of tokens (whitespace allowed).
pub fn parse_color(toks: &[Tok]) -> Option<Color> {
    let toks: Vec<&Tok> = toks.iter().filter(|t| **t != Tok::Ws).collect();
    match toks.first()? {
        Tok::Hash(h) => parse_hex_color(h),
        Tok::Ident(name) => named_color(&name.to_ascii_lowercase()),
        Tok::Function(f) if matches!(f.as_str(), "rgb" | "rgba" | "hsl" | "hsla") => {
            let mut nums: Vec<(f32, bool)> = Vec::new();
            for t in &toks[1..] {
                match t {
                    Tok::Number(n) => nums.push((*n, false)),
                    Tok::Percentage(n) => nums.push((*n, true)),
                    Tok::Dimension(n, u) if u == "deg" => nums.push((*n, false)),
                    Tok::Dimension(n, u) if u == "turn" => nums.push((*n * 360.0, false)),
                    Tok::Ident(s) if s == "none" => nums.push((0.0, false)),
                    Tok::RParen => break,
                    _ => {}
                }
            }
            if nums.len() < 3 {
                return None;
            }
            let alpha = nums.get(3).map(|&(v, pct)| if pct { v / 100.0 } else { v }).unwrap_or(1.0);
            let a = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
            if f.starts_with("rgb") {
                let ch = |(v, pct): (f32, bool)| if pct { (v * 2.55).round().clamp(0.0, 255.0) as u8 } else { v.round().clamp(0.0, 255.0) as u8 };
                Some(Color::rgba(ch(nums[0]), ch(nums[1]), ch(nums[2]), a))
            } else {
                let (r, g, b) = hsl_to_rgb(nums[0].0, nums[1].0 / 100.0, nums[2].0 / 100.0);
                Some(Color::rgba(r, g, b, a))
            }
        }
        _ => None,
    }
}

pub fn named_color(name: &str) -> Option<Color> {
    let hex: u32 = match name {
        "transparent" => return Some(Color::TRANSPARENT),
        "black" => 0x000000,
        "white" => 0xffffff,
        "red" => 0xff0000,
        "green" => 0x008000,
        "blue" => 0x0000ff,
        "yellow" => 0xffff00,
        "cyan" | "aqua" => 0x00ffff,
        "magenta" | "fuchsia" => 0xff00ff,
        "gray" | "grey" => 0x808080,
        "silver" => 0xc0c0c0,
        "maroon" => 0x800000,
        "olive" => 0x808000,
        "lime" => 0x00ff00,
        "teal" => 0x008080,
        "navy" => 0x000080,
        "purple" => 0x800080,
        "orange" => 0xffa500,
        "aliceblue" => 0xf0f8ff,
        "antiquewhite" => 0xfaebd7,
        "aquamarine" => 0x7fffd4,
        "azure" => 0xf0ffff,
        "beige" => 0xf5f5dc,
        "bisque" => 0xffe4c4,
        "blanchedalmond" => 0xffebcd,
        "blueviolet" => 0x8a2be2,
        "brown" => 0xa52a2a,
        "burlywood" => 0xdeb887,
        "cadetblue" => 0x5f9ea0,
        "chartreuse" => 0x7fff00,
        "chocolate" => 0xd2691e,
        "coral" => 0xff7f50,
        "cornflowerblue" => 0x6495ed,
        "cornsilk" => 0xfff8dc,
        "crimson" => 0xdc143c,
        "darkblue" => 0x00008b,
        "darkcyan" => 0x008b8b,
        "darkgoldenrod" => 0xb8860b,
        "darkgray" | "darkgrey" => 0xa9a9a9,
        "darkgreen" => 0x006400,
        "darkkhaki" => 0xbdb76b,
        "darkmagenta" => 0x8b008b,
        "darkolivegreen" => 0x556b2f,
        "darkorange" => 0xff8c00,
        "darkorchid" => 0x9932cc,
        "darkred" => 0x8b0000,
        "darksalmon" => 0xe9967a,
        "darkseagreen" => 0x8fbc8f,
        "darkslateblue" => 0x483d8b,
        "darkslategray" | "darkslategrey" => 0x2f4f4f,
        "darkturquoise" => 0x00ced1,
        "darkviolet" => 0x9400d3,
        "deeppink" => 0xff1493,
        "deepskyblue" => 0x00bfff,
        "dimgray" | "dimgrey" => 0x696969,
        "dodgerblue" => 0x1e90ff,
        "firebrick" => 0xb22222,
        "floralwhite" => 0xfffaf0,
        "forestgreen" => 0x228b22,
        "gainsboro" => 0xdcdcdc,
        "ghostwhite" => 0xf8f8ff,
        "gold" => 0xffd700,
        "goldenrod" => 0xdaa520,
        "greenyellow" => 0xadff2f,
        "honeydew" => 0xf0fff0,
        "hotpink" => 0xff69b4,
        "indianred" => 0xcd5c5c,
        "indigo" => 0x4b0082,
        "ivory" => 0xfffff0,
        "khaki" => 0xf0e68c,
        "lavender" => 0xe6e6fa,
        "lavenderblush" => 0xfff0f5,
        "lawngreen" => 0x7cfc00,
        "lemonchiffon" => 0xfffacd,
        "lightblue" => 0xadd8e6,
        "lightcoral" => 0xf08080,
        "lightcyan" => 0xe0ffff,
        "lightgoldenrodyellow" => 0xfafad2,
        "lightgray" | "lightgrey" => 0xd3d3d3,
        "lightgreen" => 0x90ee90,
        "lightpink" => 0xffb6c1,
        "lightsalmon" => 0xffa07a,
        "lightseagreen" => 0x20b2aa,
        "lightskyblue" => 0x87cefa,
        "lightslategray" | "lightslategrey" => 0x778899,
        "lightsteelblue" => 0xb0c4de,
        "lightyellow" => 0xffffe0,
        "limegreen" => 0x32cd32,
        "linen" => 0xfaf0e6,
        "mediumaquamarine" => 0x66cdaa,
        "mediumblue" => 0x0000cd,
        "mediumorchid" => 0xba55d3,
        "mediumpurple" => 0x9370db,
        "mediumseagreen" => 0x3cb371,
        "mediumslateblue" => 0x7b68ee,
        "mediumspringgreen" => 0x00fa9a,
        "mediumturquoise" => 0x48d1cc,
        "mediumvioletred" => 0xc71585,
        "midnightblue" => 0x191970,
        "mintcream" => 0xf5fffa,
        "mistyrose" => 0xffe4e1,
        "moccasin" => 0xffe4b5,
        "navajowhite" => 0xffdead,
        "oldlace" => 0xfdf5e6,
        "olivedrab" => 0x6b8e23,
        "orangered" => 0xff4500,
        "orchid" => 0xda70d6,
        "palegoldenrod" => 0xeee8aa,
        "palegreen" => 0x98fb98,
        "paleturquoise" => 0xafeeee,
        "palevioletred" => 0xdb7093,
        "papayawhip" => 0xffefd5,
        "peachpuff" => 0xffdab9,
        "peru" => 0xcd853f,
        "pink" => 0xffc0cb,
        "plum" => 0xdda0dd,
        "powderblue" => 0xb0e0e6,
        "rebeccapurple" => 0x663399,
        "rosybrown" => 0xbc8f8f,
        "royalblue" => 0x4169e1,
        "saddlebrown" => 0x8b4513,
        "salmon" => 0xfa8072,
        "sandybrown" => 0xf4a460,
        "seagreen" => 0x2e8b57,
        "seashell" => 0xfff5ee,
        "sienna" => 0xa0522d,
        "skyblue" => 0x87ceeb,
        "slateblue" => 0x6a5acd,
        "slategray" | "slategrey" => 0x708090,
        "snow" => 0xfffafa,
        "springgreen" => 0x00ff7f,
        "steelblue" => 0x4682b4,
        "tan" => 0xd2b48c,
        "thistle" => 0xd8bfd8,
        "tomato" => 0xff6347,
        "turquoise" => 0x40e0d0,
        "violet" => 0xee82ee,
        "wheat" => 0xf5deb3,
        "whitesmoke" => 0xf5f5f5,
        "yellowgreen" => 0x9acd32,
        _ => return None,
    };
    Some(Color::rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html::parse_document;

    #[test]
    fn parse_rules() {
        let s = parse_stylesheet("a, .b > p:first-child { color: red !important; margin: 0 auto } @media (max-width: 600px) { div { x: y } }");
        assert_eq!(s.rules.len(), 2);
        assert!(s.rules[0].decls[0].important);
        assert!(s.rules[1].media.as_ref().unwrap().matches(500.0, 100.0));
        assert!(!s.rules[1].media.as_ref().unwrap().matches(800.0, 100.0));
    }

    #[test]
    fn specificity() {
        let s = parse_selector_list_str("#a .b c:hover").unwrap();
        assert_eq!(s[0].specificity(), (1, 2, 1));
    }

    #[test]
    fn matching() {
        let doc = parse_document(r#"<div id=x class="c"><ul><li>a</li><li class=s>b</li></ul><p>p</p></div>"#);
        assert_eq!(query_selector_all(&doc, 0, "#x li").unwrap().len(), 2);
        assert_eq!(query_selector_all(&doc, 0, "ul > li:nth-child(2)").unwrap().len(), 1);
        assert_eq!(query_selector_all(&doc, 0, "ul + p").unwrap().len(), 1);
        assert_eq!(query_selector_all(&doc, 0, "li:not(.s)").unwrap().len(), 1);
        assert_eq!(query_selector_all(&doc, 0, "div.c > ul li.s").unwrap().len(), 1);
        assert_eq!(query_selector_all(&doc, 0, "[class^=s]").unwrap().len(), 1);
    }

    #[test]
    fn colors() {
        assert_eq!(parse_color(&tokenize("#f00")), Some(Color::rgb(255, 0, 0)));
        assert_eq!(parse_color(&tokenize("rgba(0, 0, 255, 0.5)")), Some(Color::rgba(0, 0, 255, 128)));
        assert_eq!(parse_color(&tokenize("hsl(120 100% 50%)")), Some(Color::rgb(0, 255, 0)));
    }
}
