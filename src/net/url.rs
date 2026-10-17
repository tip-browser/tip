//! URL parsing and resolution (a practical subset of the WHATWG URL standard).

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub scheme: String,
    pub username: String,
    pub password: String,
    pub host: String,
    pub port: Option<u16>,
    /// For special schemes, always starts with '/'. For opaque URLs (data:, about:), the rest.
    pub path: String,
    pub query: Option<String>,
    pub fragment: Option<String>,
    pub opaque: bool,
}

fn is_special(scheme: &str) -> bool {
    matches!(scheme, "http" | "https" | "file" | "ws" | "wss" | "ftp" | "tip")
}

pub fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        "ftp" => Some(21),
        _ => None,
    }
}

fn encode_set(s: &str, keep: impl Fn(char) -> bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if keep(c) {
            out.push(c);
        } else {
            let mut buf = [0u8; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

fn path_keep(c: char) -> bool {
    c.is_ascii() && !c.is_ascii_control() && !matches!(c, ' ' | '"' | '<' | '>' | '`' | '{' | '}' | '#' | '?')
}

fn query_keep(c: char) -> bool {
    c.is_ascii() && !c.is_ascii_control() && !matches!(c, ' ' | '"' | '<' | '>' | '#')
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// application/x-www-form-urlencoded serialization of a component.
pub fn form_encode(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '*' | '-' | '.' | '_') {
            out.push(c);
        } else if c == ' ' {
            out.push('+');
        } else {
            let mut buf = [0u8; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

fn remove_dot_segments(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let segs: Vec<&str> = path.split('/').collect();
    let n = segs.len();
    for (i, seg) in segs.iter().enumerate() {
        match *seg {
            "." | "%2e" | "%2E" => {
                if i == n - 1 {
                    out.push("");
                }
            }
            ".." | ".%2e" | "%2e." | "%2e%2e" => {
                if out.len() > 1 {
                    out.pop();
                }
                if i == n - 1 {
                    out.push("");
                }
            }
            s => out.push(s),
        }
    }
    let mut r = out.join("/");
    if !r.starts_with('/') {
        r.insert(0, '/');
    }
    r
}

impl Url {
    pub fn parse(input: &str) -> Option<Url> {
        let s = input.trim_matches(|c: char| c <= ' ').replace(['\t', '\n', '\r'], "");
        let colon = s.find(':')?;
        let scheme = &s[..colon];
        if scheme.is_empty() || !scheme.chars().next()?.is_ascii_alphabetic() || !scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
            return None;
        }
        let scheme = scheme.to_ascii_lowercase();
        let rest = &s[colon + 1..];
        if !is_special(&scheme) && !rest.starts_with("//") {
            // opaque
            let (body, frag) = match rest.find('#') {
                Some(i) => (&rest[..i], Some(rest[i + 1..].to_string())),
                None => (rest, None),
            };
            let (path, query) = if scheme == "data" || scheme == "javascript" {
                (body.to_string(), None)
            } else {
                match body.find('?') {
                    Some(i) => (body[..i].to_string(), Some(body[i + 1..].to_string())),
                    None => (body.to_string(), None),
                }
            };
            return Some(Url { scheme, username: String::new(), password: String::new(), host: String::new(), port: None, path, query, fragment: frag, opaque: true });
        }
        let rest = rest.replace('\\', "/");
        let after = rest.trim_start_matches('/');
        let mut u = Url { scheme: scheme.clone(), username: String::new(), password: String::new(), host: String::new(), port: None, path: "/".into(), query: None, fragment: None, opaque: false };
        if scheme == "file" {
            // file:///path or file:/path
            let p = if rest.starts_with("//") { &rest[2..] } else { &rest[..] };
            let (hostpart, pathpart) = match p.find('/') {
                Some(i) => (&p[..i], &p[i..]),
                None => (p, "/"),
            };
            u.host = hostpart.to_string();
            u.split_path_query(pathpart);
            // Windows drive letters: file:///C:/x
            return Some(u);
        }
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        let authority = &after[..end];
        let tail = &after[end..];
        let hostport = match authority.rfind('@') {
            Some(i) => {
                let cred = &authority[..i];
                match cred.find(':') {
                    Some(j) => {
                        u.username = cred[..j].to_string();
                        u.password = cred[j + 1..].to_string();
                    }
                    None => u.username = cred.to_string(),
                }
                &authority[i + 1..]
            }
            None => authority,
        };
        let (host, port) = if hostport.starts_with('[') {
            let close = hostport.find(']')?;
            let h = &hostport[..=close];
            let p = hostport[close + 1..].strip_prefix(':');
            (h.to_string(), p)
        } else {
            match hostport.rfind(':') {
                Some(i) => (hostport[..i].to_string(), Some(&hostport[i + 1..])),
                None => (hostport.to_string(), None),
            }
        };
        if host.is_empty() {
            return None;
        }
        if host.chars().any(|c| c == ' ' || c == '<' || c == '>') {
            return None;
        }
        u.host = host.to_ascii_lowercase();
        if let Some(p) = port {
            if !p.is_empty() {
                let pn: u16 = p.parse().ok()?;
                if Some(pn) != default_port(&scheme) {
                    u.port = Some(pn);
                }
            }
        }
        u.split_path_query(if tail.is_empty() { "/" } else { tail });
        Some(u)
    }

    fn split_path_query(&mut self, s: &str) {
        let (s, frag) = match s.find('#') {
            Some(i) => (&s[..i], Some(s[i + 1..].to_string())),
            None => (s, None),
        };
        let (p, q) = match s.find('?') {
            Some(i) => (&s[..i], Some(encode_set(&s[i + 1..], query_keep))),
            None => (s, None),
        };
        let p = if p.is_empty() { "/" } else { p };
        self.path = remove_dot_segments(&encode_set(p, path_keep));
        self.query = q;
        self.fragment = frag;
    }

    /// Resolve a (possibly relative) reference against this URL.
    pub fn join(&self, r: &str) -> Option<Url> {
        let r = r.trim_matches(|c: char| c <= ' ').replace(['\t', '\n', '\r'], "");
        if let Some(colon) = r.find(':') {
            let scheme = &r[..colon];
            if !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) && scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
                // absolute, unless "http:path" relative form with same special scheme
                let sl = scheme.to_ascii_lowercase();
                if !(sl == self.scheme && is_special(&sl) && !r[colon + 1..].starts_with('/')) {
                    return Url::parse(&r);
                }
                return self.join(&r[colon + 1..]);
            }
        }
        if self.opaque {
            if let Some(f) = r.strip_prefix('#') {
                let mut u = self.clone();
                u.fragment = Some(f.to_string());
                return Some(u);
            }
            return None;
        }
        let r = r.replace('\\', "/");
        if r.starts_with("//") {
            return Url::parse(&format!("{}:{}", self.scheme, r));
        }
        let mut u = self.clone();
        if r.is_empty() {
            u.fragment = None;
            return Some(u);
        }
        if let Some(f) = r.strip_prefix('#') {
            u.fragment = Some(f.to_string());
            return Some(u);
        }
        if r.starts_with('?') {
            u.split_path_query(&format!("{}{}", self.path, r));
            return Some(u);
        }
        if r.starts_with('/') {
            u.split_path_query(&r);
            return Some(u);
        }
        let base_dir = match self.path.rfind('/') {
            Some(i) => &self.path[..=i],
            None => "/",
        };
        u.split_path_query(&format!("{base_dir}{r}"));
        Some(u)
    }

    pub fn host_port(&self) -> String {
        match self.port {
            Some(p) => format!("{}:{}", self.host, p),
            None => self.host.clone(),
        }
    }

    pub fn port_or_default(&self) -> u16 {
        self.port.or_else(|| default_port(&self.scheme)).unwrap_or(80)
    }

    pub fn origin(&self) -> String {
        if self.opaque || self.scheme == "file" {
            return "null".into();
        }
        format!("{}://{}", self.scheme, self.host_port())
    }

    pub fn path_and_query(&self) -> String {
        match &self.query {
            Some(q) => format!("{}?{}", self.path, q),
            None => self.path.clone(),
        }
    }

    pub fn search(&self) -> String {
        match &self.query {
            Some(q) if !q.is_empty() => format!("?{q}"),
            _ => String::new(),
        }
    }

    pub fn hash(&self) -> String {
        match &self.fragment {
            Some(f) if !f.is_empty() => format!("#{f}"),
            _ => String::new(),
        }
    }

    pub fn without_fragment(&self) -> Url {
        Url { fragment: None, ..self.clone() }
    }

    pub fn file_path(&self) -> String {
        let p = percent_decode(&self.path);
        // file:///C:/dir → C:/dir on Windows
        if cfg!(windows) {
            let t = p.trim_start_matches('/');
            if t.len() >= 2 && t.as_bytes()[1] == b':' {
                return t.to_string();
            }
        }
        p
    }

    pub fn from_file_path(path: &str) -> Url {
        // strip Windows verbatim prefix produced by canonicalize()
        let path = path.strip_prefix(r"\\?\").unwrap_or(path);
        let p = path.replace('\\', "/");
        let p = if p.starts_with('/') { p } else { format!("/{p}") };
        Url { scheme: "file".into(), username: String::new(), password: String::new(), host: String::new(), port: None, path: encode_set(&p, path_keep), query: None, fragment: None, opaque: false }
    }
}

impl std::fmt::Display for Url {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:", self.scheme)?;
        if self.opaque {
            write!(f, "{}", self.path)?;
        } else {
            write!(f, "//")?;
            if !self.username.is_empty() {
                write!(f, "{}", self.username)?;
                if !self.password.is_empty() {
                    write!(f, ":{}", self.password)?;
                }
                write!(f, "@")?;
            }
            write!(f, "{}{}", self.host_port(), self.path)?;
        }
        if let Some(q) = &self.query {
            write!(f, "?{q}")?;
        }
        if let Some(fr) = &self.fragment {
            write!(f, "#{fr}")?;
        }
        Ok(())
    }
}

/// Turn user input from the address bar into a URL.
pub fn from_user_input(input: &str) -> Option<Url> {
    let t = input.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(u) = Url::parse(t) {
        if u.opaque && !matches!(u.scheme.as_str(), "about" | "data" | "javascript" | "tip") && !t.contains("//") {
            // "localhost:8080" parses as scheme "localhost"
            if let Some(h) = Url::parse(&format!("http://{t}")) {
                return Some(h);
            }
        }
        return Some(u);
    }
    if std::path::Path::new(t).exists() {
        let abs = std::fs::canonicalize(t).ok()?;
        return Some(Url::from_file_path(&abs.to_string_lossy()));
    }
    if !t.contains(' ') && (t.contains('.') || t.starts_with("localhost")) {
        let scheme = if t.starts_with("localhost") || t.starts_with("127.") { "http" } else { "https" };
        return Url::parse(&format!("{scheme}://{t}"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_join() {
        let b = Url::parse("https://Example.com:443/a/b/c.html?x=1#top").unwrap();
        assert_eq!(b.host, "example.com");
        assert_eq!(b.port, None);
        assert_eq!(b.to_string(), "https://example.com/a/b/c.html?x=1#top");
        assert_eq!(b.join("d.png").unwrap().to_string(), "https://example.com/a/b/d.png");
        assert_eq!(b.join("../x").unwrap().to_string(), "https://example.com/a/x");
        assert_eq!(b.join("/root").unwrap().to_string(), "https://example.com/root");
        assert_eq!(b.join("//cdn.net/lib.js").unwrap().to_string(), "https://cdn.net/lib.js");
        assert_eq!(b.join("?q=2").unwrap().to_string(), "https://example.com/a/b/c.html?q=2");
        assert_eq!(b.join("#sec").unwrap().to_string(), "https://example.com/a/b/c.html?x=1#sec");
        assert_eq!(b.join("http://other.org").unwrap().to_string(), "http://other.org/");
        let f = Url::parse("file:///Users/me/site/index.html").unwrap();
        assert_eq!(f.join("css/a.css").unwrap().to_string(), "file:///Users/me/site/css/a.css");
        let d = Url::parse("data:text/html,<b>hi</b>").unwrap();
        assert!(d.opaque);
        assert_eq!(Url::parse("http://localhost:8080/x").unwrap().port, Some(8080));
        assert_eq!(from_user_input("example.com").unwrap().to_string(), "https://example.com/");
        assert_eq!(Url::parse("http://a.com/a b").unwrap().path, "/a%20b");
    }
}
