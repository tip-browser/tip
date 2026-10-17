//! Tokenizer for the Rust subset run by pages (`<script type="text/rust">`).

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Ident(String),
    /// Integer literal with optional type suffix.
    Int(u128, Option<String>),
    Float(f64, Option<String>),
    Str(String),
    ByteStr(Vec<u8>),
    Char(char),
    Byte(u8),
    Lifetime(String),
    Punct(&'static str),
    Eof,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub tok: Tok,
    pub line: u32,
    pub col: u32,
}

const PUNCTS: &[&str] = &[
    "<<=", ">>=", "...", "..=", "::", "->", "=>", "==", "!=", "<=", ">=", "&&", "||", "+=", "-=", "*=", "/=", "%=", "^=", "&=", "|=", "<<", ">>", "..", "+", "-",
    "*", "/", "%", "^", "!", "&", "|", "=", "<", ">", "@", ".", ",", ";", ":", "#", "$", "?", "{", "}", "[", "]", "(", ")",
];

pub fn tokenize(src: &str) -> Result<Vec<Token>, (String, u32, u32)> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let (mut i, mut line, mut col) = (0usize, 1u32, 1u32);
    let err = |m: &str, l: u32, c: u32| Err((m.to_string(), l, c));
    macro_rules! bump {
        () => {{
            let c = chars[i];
            i += 1;
            if c == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
            c
        }};
    }
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            bump!();
            continue;
        }
        // comments (block comments nest)
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                bump!();
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            let (sl, sc) = (line, col);
            bump!();
            bump!();
            let mut depth = 1;
            while depth > 0 {
                if i >= chars.len() {
                    return err("unterminated block comment", sl, sc);
                }
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    bump!();
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    bump!();
                }
                bump!();
            }
            continue;
        }
        let (tl, tc) = (line, col);
        let push = |out: &mut Vec<Token>, tok: Tok| out.push(Token { tok, line: tl, col: tc });
        // raw strings r"..." r#"..."#
        if (c == 'r' || (c == 'b' && chars.get(i + 1) == Some(&'r'))) && {
            let k = if c == 'b' { i + 2 } else { i + 1 };
            let mut j = k;
            while chars.get(j) == Some(&'#') {
                j += 1;
            }
            chars.get(j) == Some(&'"')
        } {
            let is_bytes = c == 'b';
            bump!();
            if is_bytes {
                bump!();
            }
            let mut hashes = 0;
            while chars[i] == '#' {
                hashes += 1;
                bump!();
            }
            bump!(); // "
            let mut s = String::new();
            loop {
                if i >= chars.len() {
                    return err("unterminated raw string", tl, tc);
                }
                if chars[i] == '"' && (0..hashes).all(|k| chars.get(i + 1 + k) == Some(&'#')) {
                    bump!();
                    for _ in 0..hashes {
                        bump!();
                    }
                    break;
                }
                s.push(bump!());
            }
            push(&mut out, if is_bytes { Tok::ByteStr(s.into_bytes()) } else { Tok::Str(s) });
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            // byte literals b'x' and byte strings b"..."
            if c == 'b' && matches!(chars.get(i + 1), Some('\'') | Some('"')) {
                bump!();
                let q = bump!();
                if q == '\'' {
                    let ch = read_char_body(&chars, &mut i, &mut line, &mut col).map_err(|m| (m, tl, tc))?;
                    if chars.get(i) != Some(&'\'') {
                        return err("unterminated byte literal", tl, tc);
                    }
                    bump!();
                    push(&mut out, Tok::Byte(ch as u32 as u8));
                } else {
                    let s = read_str_body(&chars, &mut i, &mut line, &mut col).map_err(|m| (m, tl, tc))?;
                    push(&mut out, Tok::ByteStr(s.into_bytes()));
                }
                continue;
            }
            let mut s = String::new();
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                s.push(bump!());
            }
            push(&mut out, Tok::Ident(s));
            continue;
        }
        if c.is_ascii_digit() {
            let mut s = String::new();
            let radix = if c == '0' && matches!(chars.get(i + 1), Some('x') | Some('o') | Some('b')) {
                bump!();
                let r = match bump!() {
                    'x' => 16,
                    'o' => 8,
                    _ => 2,
                };
                r
            } else {
                10
            };
            let mut is_float = false;
            while i < chars.len() {
                let d = chars[i];
                if d == '_' {
                    bump!();
                    continue;
                }
                if d.is_digit(radix) || (radix == 16 && d.is_ascii_hexdigit()) {
                    s.push(bump!());
                } else if radix == 10 && d == '.' && !is_float && chars.get(i + 1).is_some_and(|n| n.is_ascii_digit()) {
                    is_float = true;
                    s.push(bump!());
                } else if radix == 10 && d == '.' && !is_float && !chars.get(i + 1).is_some_and(|n| *n == '.' || n.is_alphabetic() || *n == '_') {
                    // `1.` is a float
                    is_float = true;
                    s.push(bump!());
                } else if radix == 10 && (d == 'e' || d == 'E') && (chars.get(i + 1).is_some_and(|n| n.is_ascii_digit()) || (matches!(chars.get(i + 1), Some('+') | Some('-')) && chars.get(i + 2).is_some_and(|n| n.is_ascii_digit()))) {
                    is_float = true;
                    s.push(bump!());
                    if matches!(chars[i], '+' | '-') {
                        s.push(bump!());
                    }
                } else {
                    break;
                }
            }
            let mut suffix = None;
            if i < chars.len() && (chars[i].is_alphabetic()) {
                let mut sf = String::new();
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    sf.push(bump!());
                }
                if sf.starts_with('f') {
                    is_float = true;
                }
                suffix = Some(sf);
            }
            if is_float {
                let v: f64 = s.trim_end_matches('.').parse().map_err(|_| ("invalid float literal".to_string(), tl, tc))?;
                push(&mut out, Tok::Float(v, suffix));
            } else {
                let v = u128::from_str_radix(&s, radix).map_err(|_| ("integer literal is too large".to_string(), tl, tc))?;
                push(&mut out, Tok::Int(v, suffix));
            }
            continue;
        }
        if c == '"' {
            bump!();
            let s = read_str_body(&chars, &mut i, &mut line, &mut col).map_err(|m| (m, tl, tc))?;
            push(&mut out, Tok::Str(s));
            continue;
        }
        if c == '\'' {
            // char literal or lifetime
            let is_char = (chars.get(i + 2) == Some(&'\'')) || chars.get(i + 1) == Some(&'\\');
            if is_char {
                bump!();
                let ch = read_char_body(&chars, &mut i, &mut line, &mut col).map_err(|m| (m, tl, tc))?;
                if chars.get(i) != Some(&'\'') {
                    return err("unterminated character literal", tl, tc);
                }
                bump!();
                push(&mut out, Tok::Char(ch));
            } else {
                bump!();
                let mut s = String::new();
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    s.push(bump!());
                }
                push(&mut out, Tok::Lifetime(s));
            }
            continue;
        }
        let mut matched = None;
        for p in PUNCTS {
            if p.chars().enumerate().all(|(k, pc)| chars.get(i + k) == Some(&pc)) {
                matched = Some(*p);
                break;
            }
        }
        let Some(p) = matched else { return err(&format!("unknown start of token: {c}"), tl, tc) };
        for _ in 0..p.len() {
            bump!();
        }
        push(&mut out, Tok::Punct(p));
    }
    out.push(Token { tok: Tok::Eof, line, col });
    Ok(out)
}

fn escape(chars: &[char], i: &mut usize, line: &mut u32, col: &mut u32) -> Result<char, String> {
    let mut next = || -> Option<char> {
        let c = *chars.get(*i)?;
        *i += 1;
        if c == '\n' {
            *line += 1;
            *col = 1;
        } else {
            *col += 1;
        }
        Some(c)
    };
    Ok(match next().ok_or("unterminated escape")? {
        'n' => '\n',
        't' => '\t',
        'r' => '\r',
        '0' => '\0',
        '\\' => '\\',
        '\'' => '\'',
        '"' => '"',
        'x' => {
            let h: String = [next().unwrap_or('0'), next().unwrap_or('0')].iter().collect();
            char::from_u32(u32::from_str_radix(&h, 16).map_err(|_| "bad \\x escape")?).ok_or("bad \\x escape")?
        }
        'u' => {
            if next() != Some('{') {
                return Err("expected `{` in \\u escape".into());
            }
            let mut h = String::new();
            loop {
                match next() {
                    Some('}') => break,
                    Some('_') => {}
                    Some(c) => h.push(c),
                    None => return Err("unterminated unicode escape".into()),
                }
            }
            char::from_u32(u32::from_str_radix(&h, 16).map_err(|_| "bad unicode escape")?).ok_or("invalid unicode escape")?
        }
        c => return Err(format!("unknown character escape: `{c}`")),
    })
}

fn read_char_body(chars: &[char], i: &mut usize, line: &mut u32, col: &mut u32) -> Result<char, String> {
    let c = *chars.get(*i).ok_or("unterminated character literal")?;
    *i += 1;
    *col += 1;
    if c == '\\' {
        escape(chars, i, line, col)
    } else {
        Ok(c)
    }
}

fn read_str_body(chars: &[char], i: &mut usize, line: &mut u32, col: &mut u32) -> Result<String, String> {
    let mut s = String::new();
    loop {
        let c = *chars.get(*i).ok_or("unterminated double quote string")?;
        *i += 1;
        if c == '\n' {
            *line += 1;
            *col = 1;
        } else {
            *col += 1;
        }
        match c {
            '"' => return Ok(s),
            '\\' => {
                // line continuation: backslash-newline skips leading whitespace
                if chars.get(*i) == Some(&'\n') {
                    while chars.get(*i).is_some_and(|c| c.is_whitespace()) {
                        if chars[*i] == '\n' {
                            *line += 1;
                            *col = 1;
                        }
                        *i += 1;
                    }
                    continue;
                }
                s.push(escape(chars, i, line, col)?);
            }
            c => s.push(c),
        }
    }
}
