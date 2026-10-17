//! Backtracking regular expression engine implementing JavaScript syntax.

#[derive(Debug, Clone)]
enum ClassItem {
    Range(char, char),
    Digit(bool),
    Word(bool),
    Space(bool),
}

#[derive(Debug, Clone)]
struct Class {
    items: Vec<ClassItem>,
    negate: bool,
}

#[derive(Debug, Clone)]
enum Node {
    Empty,
    Char(char),
    Any,
    Class(Class),
    Start,
    End,
    WordB(bool),
    Group(Box<Node>, Option<usize>),
    Alt(Vec<Node>),
    Concat(Vec<Node>),
    Repeat { node: Box<Node>, min: u32, max: u32, greedy: bool },
    BackRef(usize),
    NamedBackRef(String),
    Look { node: Box<Node>, ahead: bool, negate: bool },
}

#[derive(Debug)]
pub struct Regex {
    pub source: String,
    pub flags: String,
    node: Node,
    pub ncaps: usize,
    pub names: Vec<(String, usize)>,
    pub icase: bool,
    pub multiline: bool,
    pub dotall: bool,
    pub global: bool,
    pub sticky: bool,
    pub unicode: bool,
}

pub type Caps = Vec<Option<(usize, usize)>>;

struct P<'a> {
    s: &'a [char],
    i: usize,
    ncaps: usize,
    names: Vec<(String, usize)>,
    unicode: bool,
}

impl P<'_> {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }
    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn err<T>(&self, m: &str) -> Result<T, String> {
        Err(format!("Invalid regular expression: {m}"))
    }

    fn alt(&mut self) -> Result<Node, String> {
        let mut alts = vec![self.concat()?];
        while self.eat('|') {
            alts.push(self.concat()?);
        }
        Ok(if alts.len() == 1 { alts.pop().unwrap() } else { Node::Alt(alts) })
    }

    fn concat(&mut self) -> Result<Node, String> {
        let mut items = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let atom = self.atom()?;
            let atom = self.quant(atom)?;
            items.push(atom);
        }
        Ok(match items.len() {
            0 => Node::Empty,
            1 => items.pop().unwrap(),
            _ => Node::Concat(items),
        })
    }

    fn number(&mut self) -> Option<u32> {
        let st = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        if st == self.i {
            return None;
        }
        self.s[st..self.i].iter().collect::<String>().parse().ok()
    }

    fn quant(&mut self, atom: Node) -> Result<Node, String> {
        let (min, max) = match self.peek() {
            Some('*') => {
                self.i += 1;
                (0, u32::MAX)
            }
            Some('+') => {
                self.i += 1;
                (1, u32::MAX)
            }
            Some('?') => {
                self.i += 1;
                (0, 1)
            }
            Some('{') => {
                let save = self.i;
                self.i += 1;
                match self.number() {
                    Some(n) => {
                        let max = if self.eat(',') { self.number().unwrap_or(u32::MAX) } else { n };
                        if !self.eat('}') {
                            self.i = save;
                            return Ok(atom);
                        }
                        (n, max)
                    }
                    None => {
                        self.i = save;
                        return Ok(atom);
                    }
                }
            }
            _ => return Ok(atom),
        };
        let greedy = !self.eat('?');
        if matches!(atom, Node::Start | Node::End | Node::WordB(_)) {
            return Ok(atom);
        }
        Ok(Node::Repeat { node: Box::new(atom), min, max, greedy })
    }

    fn atom(&mut self) -> Result<Node, String> {
        let c = self.peek().unwrap();
        self.i += 1;
        Ok(match c {
            '.' => Node::Any,
            '^' => Node::Start,
            '$' => Node::End,
            '(' => {
                let mut cap = None;
                let mut look = None;
                if self.eat('?') {
                    if self.eat(':') {
                    } else if self.eat('=') {
                        look = Some((true, false));
                    } else if self.eat('!') {
                        look = Some((true, true));
                    } else if self.peek() == Some('<') && matches!(self.s.get(self.i + 1), Some('=') | Some('!')) {
                        self.i += 1;
                        let neg = self.peek() == Some('!');
                        self.i += 1;
                        look = Some((false, neg));
                    } else if self.eat('<') {
                        let st = self.i;
                        while self.peek().is_some_and(|c| c != '>') {
                            self.i += 1;
                        }
                        let name: String = self.s[st..self.i].iter().collect();
                        self.eat('>');
                        self.ncaps += 1;
                        cap = Some(self.ncaps);
                        self.names.push((name, self.ncaps));
                    } else {
                        return self.err("invalid group");
                    }
                } else {
                    self.ncaps += 1;
                    cap = Some(self.ncaps);
                }
                let inner = self.alt()?;
                if !self.eat(')') {
                    return self.err("unterminated group");
                }
                match look {
                    Some((ahead, negate)) => Node::Look { node: Box::new(inner), ahead, negate },
                    None => Node::Group(Box::new(inner), cap),
                }
            }
            '[' => Node::Class(self.class()?),
            '\\' => self.escape(false)?,
            ')' => return self.err("unmatched ')'"),
            c => Node::Char(c),
        })
    }

    fn escape_char(&mut self) -> Result<Option<char>, String> {
        let c = self.peek().ok_or("\\ at end of pattern")?;
        self.i += 1;
        Ok(Some(match c {
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            'v' => '\u{b}',
            'f' => '\u{c}',
            '0' => '\0',
            'x' => {
                let h: String = self.s.get(self.i..self.i + 2).map(|s| s.iter().collect()).unwrap_or_default();
                match u32::from_str_radix(&h, 16) {
                    Ok(v) => {
                        self.i += 2;
                        char::from_u32(v).unwrap_or('x')
                    }
                    Err(_) => 'x',
                }
            }
            'u' => {
                if self.peek() == Some('{') {
                    let st = self.i + 1;
                    let mut j = st;
                    while self.s.get(j).is_some_and(|c| *c != '}') {
                        j += 1;
                    }
                    let h: String = self.s[st..j].iter().collect();
                    self.i = j + 1;
                    char::from_u32(u32::from_str_radix(&h, 16).unwrap_or(0xfffd)).unwrap_or('\u{fffd}')
                } else {
                    let h: String = self.s.get(self.i..self.i + 4).map(|s| s.iter().collect()).unwrap_or_default();
                    match u32::from_str_radix(&h, 16) {
                        Ok(v) => {
                            self.i += 4;
                            // surrogate pair
                            if (0xD800..0xDC00).contains(&v) && self.s.get(self.i) == Some(&'\\') && self.s.get(self.i + 1) == Some(&'u') {
                                let h2: String = self.s.get(self.i + 2..self.i + 6).map(|s| s.iter().collect()).unwrap_or_default();
                                if let Ok(lo) = u32::from_str_radix(&h2, 16) {
                                    if (0xDC00..0xE000).contains(&lo) {
                                        self.i += 6;
                                        return Ok(char::from_u32(0x10000 + ((v - 0xD800) << 10) + (lo - 0xDC00)));
                                    }
                                }
                            }
                            char::from_u32(v).unwrap_or('\u{fffd}')
                        }
                        Err(_) => 'u',
                    }
                }
            }
            'c' => {
                let l = self.peek().unwrap_or('@');
                self.i += 1;
                char::from_u32((l as u32) % 32).unwrap_or('c')
            }
            c => c,
        }))
    }

    fn escape(&mut self, in_class: bool) -> Result<Node, String> {
        let c = self.peek().ok_or("\\ at end of pattern")?;
        match c {
            'd' | 'D' | 'w' | 'W' | 's' | 'S' => {
                self.i += 1;
                let item = match c {
                    'd' => ClassItem::Digit(false),
                    'D' => ClassItem::Digit(true),
                    'w' => ClassItem::Word(false),
                    'W' => ClassItem::Word(true),
                    's' => ClassItem::Space(false),
                    _ => ClassItem::Space(true),
                };
                return Ok(Node::Class(Class { items: vec![item], negate: false }));
            }
            'b' if !in_class => {
                self.i += 1;
                return Ok(Node::WordB(true));
            }
            'B' if !in_class => {
                self.i += 1;
                return Ok(Node::WordB(false));
            }
            'b' if in_class => {
                self.i += 1;
                return Ok(Node::Char('\u{8}'));
            }
            '1'..='9' if !in_class => {
                let n = self.number().unwrap_or(0) as usize;
                return Ok(Node::BackRef(n));
            }
            'k' if !in_class && self.s.get(self.i + 1) == Some(&'<') => {
                self.i += 2;
                let st = self.i;
                while self.peek().is_some_and(|c| c != '>') {
                    self.i += 1;
                }
                let name: String = self.s[st..self.i].iter().collect();
                self.eat('>');
                return Ok(Node::NamedBackRef(name));
            }
            'p' | 'P' if self.unicode && self.s.get(self.i + 1) == Some(&'{') => {
                // Unicode property escapes: approximate common ones.
                let neg = c == 'P';
                self.i += 2;
                let st = self.i;
                while self.peek().is_some_and(|c| c != '}') {
                    self.i += 1;
                }
                let name: String = self.s[st..self.i].iter().collect();
                self.eat('}');
                let item = match name.as_str() {
                    "L" | "Letter" | "Alphabetic" => ClassItem::Word(false),
                    "N" | "Nd" => ClassItem::Digit(false),
                    _ => ClassItem::Word(false),
                };
                return Ok(Node::Class(Class { items: vec![item], negate: neg }));
            }
            _ => {}
        }
        let ch = self.escape_char()?.unwrap_or(c);
        Ok(Node::Char(ch))
    }

    fn class(&mut self) -> Result<Class, String> {
        let negate = self.eat('^');
        let mut items = Vec::new();
        loop {
            let Some(c) = self.peek() else { return self.err("unterminated character class") };
            if c == ']' {
                self.i += 1;
                break;
            }
            let lo = if c == '\\' {
                self.i += 1;
                match self.escape(true)? {
                    Node::Char(ch) => ch,
                    Node::Class(cl) => {
                        items.extend(cl.items);
                        continue;
                    }
                    _ => continue,
                }
            } else {
                self.i += 1;
                c
            };
            if self.peek() == Some('-') && self.s.get(self.i + 1).is_some_and(|c| *c != ']') {
                self.i += 1;
                let hc = self.peek().unwrap();
                let hi = if hc == '\\' {
                    self.i += 1;
                    match self.escape(true)? {
                        Node::Char(ch) => ch,
                        Node::Class(cl) => {
                            items.push(ClassItem::Range(lo, lo));
                            items.push(ClassItem::Range('-', '-'));
                            items.extend(cl.items);
                            continue;
                        }
                        _ => lo,
                    }
                } else {
                    self.i += 1;
                    hc
                };
                items.push(ClassItem::Range(lo, hi));
            } else {
                items.push(ClassItem::Range(lo, lo));
            }
        }
        Ok(Class { items, negate })
    }
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn fold(c: char) -> char {
    let mut l = c.to_lowercase();
    match (l.next(), l.next()) {
        (Some(x), None) => x,
        _ => c,
    }
}

impl Class {
    fn matches(&self, c: char, icase: bool) -> bool {
        let hit = |c: char| {
            self.items.iter().any(|it| match it {
                ClassItem::Range(a, b) => c >= *a && c <= *b,
                ClassItem::Digit(n) => c.is_ascii_digit() != *n,
                ClassItem::Word(n) => is_word(c) != *n,
                ClassItem::Space(n) => (c.is_whitespace() || c == '\u{feff}') != *n,
            })
        };
        let r = hit(c) || (icase && (hit(fold(c)) || c.to_uppercase().next().is_some_and(hit)));
        r != self.negate
    }
}

struct M<'a> {
    re: &'a Regex,
    s: &'a [char],
    steps: u64,
}

impl M<'_> {
    fn char_eq(&self, a: char, b: char) -> bool {
        a == b || (self.re.icase && fold(a) == fold(b))
    }

    fn single(&self, n: &Node, pos: usize) -> Option<bool> {
        // Returns Some(match) for single-character nodes.
        let c = self.s.get(pos).copied();
        match n {
            Node::Char(x) => Some(c.is_some_and(|c| self.char_eq(c, *x))),
            Node::Any => Some(c.is_some_and(|c| self.re.dotall || !matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}'))),
            Node::Class(cl) => Some(c.is_some_and(|c| cl.matches(c, self.re.icase))),
            _ => None,
        }
    }

    fn m(&mut self, n: &Node, pos: usize, caps: &mut Caps, k: &mut dyn FnMut(&mut Self, usize, &mut Caps) -> bool) -> bool {
        self.steps += 1;
        if self.steps > 50_000_000 {
            return false;
        }
        match n {
            Node::Empty => k(self, pos, caps),
            Node::Char(_) | Node::Any | Node::Class(_) => {
                if self.single(n, pos) == Some(true) {
                    k(self, pos + 1, caps)
                } else {
                    false
                }
            }
            Node::Start => {
                let ok = pos == 0 || (self.re.multiline && matches!(self.s[pos - 1], '\n' | '\r' | '\u{2028}' | '\u{2029}'));
                ok && k(self, pos, caps)
            }
            Node::End => {
                let ok = pos == self.s.len() || (self.re.multiline && matches!(self.s[pos], '\n' | '\r' | '\u{2028}' | '\u{2029}'));
                ok && k(self, pos, caps)
            }
            Node::WordB(want) => {
                let a = pos > 0 && is_word(self.s[pos - 1]);
                let b = pos < self.s.len() && is_word(self.s[pos]);
                ((a != b) == *want) && k(self, pos, caps)
            }
            Node::Group(inner, cap) => match cap {
                None => self.m(inner, pos, caps, k),
                Some(i) => {
                    let i = *i;
                    let start = pos;
                    self.m(inner, pos, caps, &mut |me: &mut Self, p, caps: &mut Caps| {
                        let old = caps[i];
                        caps[i] = Some((start, p));
                        if k(me, p, caps) {
                            true
                        } else {
                            caps[i] = old;
                            false
                        }
                    })
                }
            },
            Node::Alt(alts) => {
                for a in alts {
                    let saved = caps.clone();
                    if self.m(a, pos, caps, k) {
                        return true;
                    }
                    *caps = saved;
                }
                false
            }
            Node::Concat(items) => self.seq(items, 0, pos, caps, k),
            Node::Repeat { node, min, max, greedy } => {
                if matches!(**node, Node::Char(_) | Node::Any | Node::Class(_)) {
                    // fast path for single-character repetition
                    let mut n = 0usize;
                    while (n as u32) < *max && self.single(node, pos + n) == Some(true) {
                        n += 1;
                    }
                    if (n as u32) < *min {
                        return false;
                    }
                    if *greedy {
                        let mut c = n;
                        loop {
                            if k(self, pos + c, caps) {
                                return true;
                            }
                            if c == *min as usize {
                                return false;
                            }
                            c -= 1;
                        }
                    } else {
                        for c in *min as usize..=n {
                            if k(self, pos + c, caps) {
                                return true;
                            }
                        }
                        return false;
                    }
                }
                self.rep(node, *min, *max, *greedy, 0, pos, caps, k)
            }
            Node::BackRef(i) => {
                let Some(Some((a, b))) = caps.get(*i).copied() else { return k(self, pos, caps) };
                let len = b - a;
                if pos + len > self.s.len() {
                    return false;
                }
                for j in 0..len {
                    if !self.char_eq(self.s[a + j], self.s[pos + j]) {
                        return false;
                    }
                }
                k(self, pos + len, caps)
            }
            Node::NamedBackRef(name) => {
                let idx = self.re.names.iter().find(|(n, _)| n == name).map(|x| x.1).unwrap_or(0);
                self.m(&Node::BackRef(idx), pos, caps, k)
            }
            Node::Look { node, ahead, negate } => {
                let mut saved = caps.clone();
                let found = if *ahead {
                    self.m(node, pos, &mut saved, &mut |_, _, _| true)
                } else {
                    let mut ok = false;
                    for st in (0..=pos).rev() {
                        let target = pos;
                        if self.m(node, st, &mut saved, &mut |_, p, _| p == target) {
                            ok = true;
                            break;
                        }
                    }
                    ok
                };
                if found != *negate {
                    if found && !*negate {
                        *caps = saved;
                    }
                    k(self, pos, caps)
                } else {
                    false
                }
            }
        }
    }

    fn seq(&mut self, items: &[Node], i: usize, pos: usize, caps: &mut Caps, k: &mut dyn FnMut(&mut Self, usize, &mut Caps) -> bool) -> bool {
        if i == items.len() {
            return k(self, pos, caps);
        }
        let (first, _) = items.split_at(i + 1);
        let node = &first[i];
        self.m(node, pos, caps, &mut |me: &mut Self, p, caps: &mut Caps| me.seq(items, i + 1, p, caps, k))
    }

    #[allow(clippy::too_many_arguments)]
    fn rep(&mut self, node: &Node, min: u32, max: u32, greedy: bool, count: u32, pos: usize, caps: &mut Caps, k: &mut dyn FnMut(&mut Self, usize, &mut Caps) -> bool) -> bool {
        let try_more = |me: &mut Self, caps: &mut Caps, k: &mut dyn FnMut(&mut Self, usize, &mut Caps) -> bool| -> bool {
            if count >= max {
                return false;
            }
            me.m(node, pos, caps, &mut |me: &mut Self, p, caps: &mut Caps| {
                if p == pos && count >= min {
                    return false; // empty iteration: stop
                }
                me.rep(node, min, max, greedy, count + 1, p, caps, k)
            })
        };
        if greedy {
            if try_more(self, caps, k) {
                return true;
            }
            count >= min && k(self, pos, caps)
        } else {
            if count >= min && k(self, pos, caps) {
                return true;
            }
            try_more(self, caps, k)
        }
    }
}

impl Regex {
    pub fn new(pattern: &str, flags: &str) -> Result<Regex, String> {
        for f in flags.chars() {
            if !"gimsuyd v".contains(f) {
                return Err(format!("Invalid regular expression flags '{flags}'"));
            }
        }
        let chars: Vec<char> = pattern.chars().collect();
        let unicode = flags.contains('u') || flags.contains('v');
        let mut p = P { s: &chars, i: 0, ncaps: 0, names: vec![], unicode };
        let node = p.alt()?;
        if p.i < chars.len() {
            return p.err("unmatched ')'");
        }
        Ok(Regex {
            source: pattern.to_string(),
            flags: flags.to_string(),
            node,
            ncaps: p.ncaps,
            names: p.names,
            icase: flags.contains('i'),
            multiline: flags.contains('m'),
            dotall: flags.contains('s'),
            global: flags.contains('g'),
            sticky: flags.contains('y'),
            unicode,
        })
    }

    /// Find a match starting at or after `start` (char index). Returns captures (index 0 = whole match).
    pub fn exec_at(&self, s: &[char], start: usize) -> Option<Caps> {
        let mut m = M { re: self, s, steps: 0 };
        let last = if self.sticky { start } else { s.len() };
        let mut st = start;
        while st <= last && st <= s.len() {
            let mut caps: Caps = vec![None; self.ncaps + 1];
            let mut end = None;
            if m.m(&self.node, st, &mut caps, &mut |_, p, _| {
                end = Some(p);
                true
            }) {
                caps[0] = Some((st, end.unwrap()));
                return Some(caps);
            }
            if m.steps > 50_000_000 {
                return None;
            }
            st += 1;
        }
        None
    }

    pub fn is_match(&self, s: &str) -> bool {
        let chars: Vec<char> = s.chars().collect();
        self.exec_at(&chars, 0).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(p: &str, f: &str, s: &str) -> Option<String> {
        let re = Regex::new(p, f).unwrap();
        let c: Vec<char> = s.chars().collect();
        re.exec_at(&c, 0).map(|caps| {
            let (a, b) = caps[0].unwrap();
            c[a..b].iter().collect()
        })
    }

    #[test]
    fn basics() {
        assert_eq!(find("a+b", "", "xxaaab"), Some("aaab".into()));
        assert_eq!(find("^\\d{3}-\\d{4}$", "", "555-1234"), Some("555-1234".into()));
        assert_eq!(find("(\\w+)@(\\w+)\\.com", "", "mail bob@example.com now"), Some("bob@example.com".into()));
        assert_eq!(find("colou?r", "i", "COLOR"), Some("COLOR".into()));
        assert_eq!(find("a.*?b", "", "a1b2b"), Some("a1b".into()));
        assert_eq!(find("(a|ab)(c|bcd)(d*)", "", "abcd"), Some("abcd".into()));
        assert_eq!(find("\\bfoo\\b", "", "a foo b"), Some("foo".into()));
        assert_eq!(find("(?<=\\$)\\d+", "", "cost $42"), Some("42".into()));
        assert_eq!(find("x(?!y)", "", "xyxz"), Some("x".into()));
        assert_eq!(find("(\\w)\\1", "", "abccd"), Some("cc".into()));
        assert_eq!(find("[^a-c]+", "", "abcdef"), Some("def".into()));
        assert_eq!(find("^$", "", ""), Some("".into()));
    }

    #[test]
    fn long_input() {
        let s = "a".repeat(100000) + "b";
        assert_eq!(find("a*b", "", &s).map(|x| x.len()), Some(100001));
    }
}
