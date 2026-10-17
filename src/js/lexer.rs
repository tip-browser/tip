//! JavaScript tokenizer.

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Ident(String),
    Keyword(&'static str),
    Num(f64),
    BigInt(String),
    Str(String),
    Template(Vec<TemplatePart>),
    Regex(String, String),
    Punct(&'static str),
    PrivateName(String),
    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TemplatePart {
    Str { cooked: String, raw: String },
    Expr { src: String, line: u32 },
}

#[derive(Debug, Clone)]
pub struct Token {
    pub tok: Tok,
    pub nl_before: bool,
    pub line: u32,
    pub col: u32,
    /// Char offsets into the source.
    pub start: usize,
    pub end: usize,
}

const KEYWORDS: &[&str] = &[
    "var", "let", "const", "function", "return", "if", "else", "for", "while", "do", "break", "continue", "new", "delete", "typeof",
    "instanceof", "in", "of", "this", "null", "undefined_kw", "true", "false", "class", "extends", "super", "throw", "try", "catch",
    "finally", "switch", "case", "default", "void", "yield", "async", "await", "static", "get", "set", "debugger", "with", "import",
    "export",
];

// Words that are keywords only in certain positions are lexed as identifiers and checked by the parser.
const RESERVED: &[&str] = &[
    "var", "const", "function", "return", "if", "else", "for", "while", "do", "break", "continue", "new", "delete", "typeof",
    "instanceof", "in", "this", "null", "true", "false", "class", "extends", "super", "throw", "try", "catch", "finally", "switch",
    "case", "default", "void", "debugger", "with", "import", "export",
];

const PUNCTS: &[&str] = &[
    ">>>=", "...", "===", "!==", "**=", "<<=", ">>=", ">>>", "&&=", "||=", "??=", "=>", "==", "!=", "<=", ">=", "&&", "||", "??",
    "?.", "++", "--", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<", ">>", "**", "{", "}", "(", ")", "[", "]", ";", ",",
    "<", ">", "+", "-", "*", "/", "%", "&", "|", "^", "!", "~", "?", ":", "=", ".", "@", "#",
];

pub fn is_reserved(s: &str) -> bool {
    RESERVED.contains(&s)
}

fn keyword(s: &str) -> Option<&'static str> {
    if !is_reserved(s) {
        return None;
    }
    KEYWORDS.iter().find(|k| **k == s).copied()
}

pub struct Lexer<'a> {
    src: &'a [char],
    pos: usize,
    line: u32,
    col: u32,
    base_line: u32,
}

fn is_id_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '$' || (c as u32 > 127 && !c.is_whitespace())
}

fn is_id_part(c: char) -> bool {
    is_id_start(c) || c.is_ascii_digit() || c == '\u{200c}' || c == '\u{200d}'
}

impl<'a> Lexer<'a> {
    fn peek(&self) -> Option<char> {
        self.src.get(self.pos).copied()
    }
    fn peek_at(&self, n: usize) -> Option<char> {
        self.src.get(self.pos + n).copied()
    }
    fn bump(&mut self) -> Option<char> {
        let c = self.src.get(self.pos).copied()?;
        self.pos += 1;
        if c == '\n' {
            self.line += 1;
            self.col = 0;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    fn err(&self, msg: &str) -> String {
        format!("SyntaxError: {} (line {})", msg, self.line + self.base_line)
    }

    /// Skip whitespace and comments; returns whether a newline was crossed.
    fn skip_trivia(&mut self) -> Result<bool, String> {
        let mut nl = false;
        loop {
            match self.peek() {
                Some('\n') | Some('\u{2028}') | Some('\u{2029}') => {
                    nl = true;
                    self.bump();
                }
                Some(c) if c.is_whitespace() || c == '\u{feff}' => {
                    self.bump();
                }
                Some('/') if self.peek_at(1) == Some('/') => {
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                Some('/') if self.peek_at(1) == Some('*') => {
                    self.bump();
                    self.bump();
                    loop {
                        match self.bump() {
                            None => return Err(self.err("unterminated comment")),
                            Some('*') if self.peek() == Some('/') => {
                                self.bump();
                                break;
                            }
                            Some('\n') => nl = true,
                            _ => {}
                        }
                    }
                }
                Some('<') if self.peek_at(1) == Some('!') && self.peek_at(2) == Some('-') && self.peek_at(3) == Some('-') => {
                    // HTML-like comment
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                Some('-') if self.peek_at(1) == Some('-') && self.peek_at(2) == Some('>') && (nl || self.pos == 0) => {
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                Some('#') if self.pos == 0 && self.peek_at(1) == Some('!') => {
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                _ => return Ok(nl),
            }
        }
    }

    fn read_escape(&mut self, raw: &mut String) -> Result<Option<String>, String> {
        let c = self.bump().ok_or_else(|| self.err("bad escape"))?;
        raw.push(c);
        Ok(Some(match c {
            'n' => "\n".into(),
            't' => "\t".into(),
            'r' => "\r".into(),
            'b' => "\u{8}".into(),
            'f' => "\u{c}".into(),
            'v' => "\u{b}".into(),
            '0' if !self.peek().is_some_and(|c| c.is_ascii_digit()) => "\0".into(),
            'x' => {
                let mut h = String::new();
                for _ in 0..2 {
                    let d = self.bump().ok_or_else(|| self.err("bad hex escape"))?;
                    raw.push(d);
                    h.push(d);
                }
                let v = u32::from_str_radix(&h, 16).map_err(|_| self.err("bad hex escape"))?;
                char::from_u32(v).map(|c| c.to_string()).unwrap_or_default()
            }
            'u' => {
                let v = self.read_unicode_escape(raw)?;
                // surrogate pairs
                if (0xD800..0xDC00).contains(&v) && self.peek() == Some('\\') && self.peek_at(1) == Some('u') {
                    let save = (self.pos, self.line, self.col);
                    self.bump();
                    self.bump();
                    let mut r2 = String::new();
                    let lo = self.read_unicode_escape(&mut r2)?;
                    if (0xDC00..0xE000).contains(&lo) {
                        raw.push_str("\\u");
                        raw.push_str(&r2);
                        let cp = 0x10000 + ((v - 0xD800) << 10) + (lo - 0xDC00);
                        return Ok(Some(char::from_u32(cp).map(|c| c.to_string()).unwrap_or_default()));
                    }
                    (self.pos, self.line, self.col) = save;
                }
                char::from_u32(v).map(|c| c.to_string()).unwrap_or_else(|| "\u{fffd}".into())
            }
            '\r' => {
                if self.peek() == Some('\n') {
                    self.bump();
                }
                String::new()
            }
            '\n' | '\u{2028}' | '\u{2029}' => String::new(),
            c => c.to_string(),
        }))
    }

    fn read_unicode_escape(&mut self, raw: &mut String) -> Result<u32, String> {
        if self.peek() == Some('{') {
            self.bump();
            raw.push('{');
            let mut h = String::new();
            while let Some(c) = self.bump() {
                raw.push(c);
                if c == '}' {
                    break;
                }
                h.push(c);
            }
            return u32::from_str_radix(&h, 16).map_err(|_| self.err("bad unicode escape"));
        }
        let mut h = String::new();
        for _ in 0..4 {
            let d = self.bump().ok_or_else(|| self.err("bad unicode escape"))?;
            raw.push(d);
            h.push(d);
        }
        u32::from_str_radix(&h, 16).map_err(|_| self.err("bad unicode escape"))
    }

    fn read_string(&mut self, q: char) -> Result<String, String> {
        let mut s = String::new();
        let mut raw = String::new();
        loop {
            match self.bump() {
                None | Some('\n') => return Err(self.err("unterminated string literal")),
                Some(c) if c == q => break,
                Some('\\') => {
                    if let Some(e) = self.read_escape(&mut raw)? {
                        s.push_str(&e);
                    }
                }
                Some(c) => s.push(c),
            }
        }
        Ok(s)
    }

    fn read_template(&mut self) -> Result<Vec<TemplatePart>, String> {
        let mut parts = Vec::new();
        let mut cooked = String::new();
        let mut raw = String::new();
        loop {
            match self.bump() {
                None => return Err(self.err("unterminated template literal")),
                Some('`') => break,
                Some('\\') => {
                    raw.push('\\');
                    if let Some(e) = self.read_escape(&mut raw)? {
                        cooked.push_str(&e);
                    }
                }
                Some('$') if self.peek() == Some('{') => {
                    self.bump();
                    parts.push(TemplatePart::Str { cooked: std::mem::take(&mut cooked), raw: std::mem::take(&mut raw) });
                    let line = self.line + self.base_line;
                    // lex the expression up to its matching brace and capture its source
                    let begin = self.pos;
                    let mut depth = 1;
                    let mut prev: Option<Tok> = None;
                    let src = loop {
                        let t = self.next_token(prev.as_ref())?;
                        match t.tok {
                            Tok::Eof => return Err(self.err("unterminated template expression")),
                            Tok::Punct("{") => depth += 1,
                            Tok::Punct("}") => {
                                depth -= 1;
                                if depth == 0 {
                                    break self.src[begin..t.start].iter().collect::<String>();
                                }
                            }
                            _ => {}
                        }
                        prev = Some(t.tok);
                    };
                    parts.push(TemplatePart::Expr { src, line });
                }
                Some('\r') => {
                    if self.peek() == Some('\n') {
                        self.bump();
                    }
                    cooked.push('\n');
                    raw.push('\n');
                }
                Some(c) => {
                    cooked.push(c);
                    raw.push(c);
                }
            }
        }
        parts.push(TemplatePart::Str { cooked, raw });
        Ok(parts)
    }

    fn read_number(&mut self) -> Result<Tok, String> {
        let start = self.pos;
        let c = self.peek().unwrap();
        if c == '0' {
            let radix = match self.peek_at(1) {
                Some('x') | Some('X') => 16,
                Some('o') | Some('O') => 8,
                Some('b') | Some('B') => 2,
                _ => 0,
            };
            if radix != 0 {
                self.bump();
                self.bump();
                let mut s = String::new();
                while let Some(c) = self.peek() {
                    if c.is_digit(radix) {
                        s.push(c);
                        self.bump();
                    } else if c == '_' {
                        self.bump();
                    } else {
                        break;
                    }
                }
                if self.peek() == Some('n') {
                    self.bump();
                    return Ok(Tok::BigInt(i128::from_str_radix(&s, radix).unwrap_or(0).to_string()));
                }
                let mut v = 0f64;
                for d in s.chars() {
                    v = v * radix as f64 + d.to_digit(radix).unwrap() as f64;
                }
                return Ok(Tok::Num(v));
            }
            // legacy octal 0777
            if self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) {
                let mut j = 1;
                let mut all_octal = true;
                while let Some(d) = self.peek_at(j) {
                    if !d.is_ascii_digit() {
                        break;
                    }
                    if d > '7' {
                        all_octal = false;
                    }
                    j += 1;
                }
                if all_octal {
                    self.bump();
                    let mut v = 0f64;
                    while let Some(d) = self.peek() {
                        if !d.is_ascii_digit() {
                            break;
                        }
                        v = v * 8.0 + d.to_digit(8).unwrap() as f64;
                        self.bump();
                    }
                    return Ok(Tok::Num(v));
                }
            }
        }
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                s.push(c);
                self.bump();
            } else if c == '_' {
                self.bump();
            } else {
                break;
            }
        }
        if self.peek() == Some('n') {
            self.bump();
            return Ok(Tok::BigInt(s));
        }
        if self.peek() == Some('.') {
            s.push('.');
            self.bump();
            while let Some(c) = self.peek() {
                if c.is_ascii_digit() {
                    s.push(c);
                    self.bump();
                } else if c == '_' {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        if matches!(self.peek(), Some('e') | Some('E')) {
            let save = (self.pos, self.line, self.col);
            let mut e = String::from("e");
            self.bump();
            if matches!(self.peek(), Some('+') | Some('-')) {
                e.push(self.bump().unwrap());
            }
            if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                while let Some(c) = self.peek() {
                    if c.is_ascii_digit() {
                        e.push(c);
                        self.bump();
                    } else {
                        break;
                    }
                }
                s.push_str(&e);
            } else {
                (self.pos, self.line, self.col) = save;
            }
        }
        if self.peek().is_some_and(is_id_start) {
            return Err(self.err("identifier starts immediately after numeric literal"));
        }
        let _ = start;
        s.parse::<f64>().map(Tok::Num).map_err(|_| self.err("bad number"))
    }

    fn read_regex(&mut self) -> Result<Tok, String> {
        // assumes leading '/' consumed
        let mut pat = String::new();
        let mut in_class = false;
        loop {
            let c = self.bump().ok_or_else(|| self.err("unterminated regex"))?;
            match c {
                '\n' => return Err(self.err("unterminated regex")),
                '\\' => {
                    pat.push(c);
                    if let Some(n) = self.bump() {
                        pat.push(n);
                    }
                }
                '[' => {
                    in_class = true;
                    pat.push(c);
                }
                ']' => {
                    in_class = false;
                    pat.push(c);
                }
                '/' if !in_class => break,
                _ => pat.push(c),
            }
        }
        let mut flags = String::new();
        while let Some(c) = self.peek() {
            if is_id_part(c) {
                flags.push(c);
                self.bump();
            } else {
                break;
            }
        }
        Ok(Tok::Regex(pat, flags))
    }
}

fn regex_allowed(prev: Option<&Tok>) -> bool {
    match prev {
        None => true,
        Some(Tok::Num(_)) | Some(Tok::Str(_)) | Some(Tok::Template(_)) | Some(Tok::Regex(..)) | Some(Tok::BigInt(_)) | Some(Tok::PrivateName(_)) => false,
        Some(Tok::Ident(_)) => false,
        Some(Tok::Keyword(k)) => !matches!(*k, "this" | "super" | "null" | "true" | "false"),
        Some(Tok::Punct(p)) => !matches!(*p, ")" | "]" | "}" | "++" | "--"),
        Some(Tok::Eof) => true,
    }
}

pub fn tokenize(src: &str, base_line: u32) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut lx = Lexer { src: &chars, pos: 0, line: 0, col: 0, base_line };
    let mut out: Vec<Token> = Vec::new();
    loop {
        let t = lx.next_token(out.last().map(|t| &t.tok))?;
        let eof = t.tok == Tok::Eof;
        out.push(t);
        if eof {
            break;
        }
    }
    Ok(out)
}

impl Lexer<'_> {
    /// Lex one token; `prev` decides whether `/` starts a regex.
    fn next_token(&mut self, prev: Option<&Tok>) -> Result<Token, String> {
        let lx = self;
        let nl = lx.skip_trivia()?;
        let line = lx.line + lx.base_line;
        let col = lx.col;
        let start = lx.pos;
        let Some(c) = lx.peek() else {
            return Ok(Token { tok: Tok::Eof, nl_before: true, line, col, start, end: start });
        };
        let tok = if is_id_start(c) || c == '\\' {
            let mut s = String::new();
            while let Some(c) = lx.peek() {
                if is_id_part(c) {
                    s.push(c);
                    lx.bump();
                } else if c == '\\' && lx.peek_at(1) == Some('u') {
                    lx.bump();
                    lx.bump();
                    let mut raw = String::new();
                    let v = lx.read_unicode_escape(&mut raw)?;
                    s.push(char::from_u32(v).unwrap_or('_'));
                } else {
                    break;
                }
            }
            match keyword(&s) {
                Some(k) => Tok::Keyword(k),
                None => Tok::Ident(s),
            }
        } else if c.is_ascii_digit() || (c == '.' && lx.peek_at(1).is_some_and(|d| d.is_ascii_digit())) {
            lx.read_number()?
        } else if c == '"' || c == '\'' {
            lx.bump();
            Tok::Str(lx.read_string(c)?)
        } else if c == '`' {
            lx.bump();
            Tok::Template(lx.read_template()?)
        } else if c == '#' && lx.peek_at(1).is_some_and(is_id_start) {
            lx.bump();
            let mut s = String::new();
            while let Some(c) = lx.peek() {
                if is_id_part(c) {
                    s.push(c);
                    lx.bump();
                } else {
                    break;
                }
            }
            Tok::PrivateName(s)
        } else if c == '/' && regex_allowed(prev) {
            lx.bump();
            lx.read_regex()?
        } else {
            let mut found = None;
            for p in PUNCTS {
                let pc: Vec<char> = p.chars().collect();
                if lx.src[lx.pos..].starts_with(&pc) {
                    // `?.` followed by a digit is a conditional + number
                    if *p == "?." && lx.peek_at(2).is_some_and(|d| d.is_ascii_digit()) {
                        continue;
                    }
                    found = Some(*p);
                    break;
                }
            }
            let p = found.ok_or_else(|| lx.err(&format!("unexpected character '{c}'")))?;
            for _ in 0..p.chars().count() {
                lx.bump();
            }
            Tok::Punct(p)
        };
        Ok(Token { tok, nl_before: nl, line, col, start, end: lx.pos })
    }
}
