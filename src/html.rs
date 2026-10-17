//! HTML tokenizer and tree builder.
//!
//! This is a pragmatic, forgiving parser rather than a full implementation of
//! the WHATWG algorithm: it handles implied html/head/body, void elements,
//! raw-text elements, character references and the common implicit end tags
//! (p, li, dt/dd, option, table parts).

use crate::dom::{Document, NodeId, VOID_ELEMENTS};

#[derive(Debug, PartialEq)]
enum Token {
    Doctype,
    StartTag { name: String, attrs: Vec<(String, String)>, self_closing: bool },
    EndTag { name: String },
    Text(String),
    Comment(String),
}

const RAW_TEXT: &[&str] = &["script", "style", "textarea", "title", "xmp", "noscript"];

struct Tokenizer<'a> {
    src: &'a str,
    pos: usize,
    raw_until: Option<String>,
}

impl<'a> Tokenizer<'a> {
    fn new(src: &'a str) -> Self {
        Tokenizer { src, pos: 0, raw_until: None }
    }

    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn next(&mut self) -> Option<Token> {
        if self.pos >= self.src.len() {
            return None;
        }
        if let Some(tag) = self.raw_until.take() {
            let rest = self.rest();
            let lower = rest.to_ascii_lowercase();
            let needle = format!("</{tag}");
            let end = lower.find(&needle).unwrap_or(rest.len());
            let text = &rest[..end];
            self.pos += end;
            if !text.is_empty() {
                let decoded = if tag == "textarea" || tag == "title" { decode_entities(text) } else { text.to_string() };
                return Some(Token::Text(decoded));
            }
        }
        let rest = self.rest();
        if rest.starts_with("<!--") {
            let body = &rest[4..];
            let end = body.find("-->").unwrap_or(body.len());
            let text = body[..end].to_string();
            self.pos += 4 + (end + 3).min(body.len());
            return Some(Token::Comment(text));
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            let end = rest.find('>').map(|i| i + 1).unwrap_or(rest.len());
            self.pos += end;
            return Some(Token::Doctype);
        }
        if rest.starts_with("</") {
            let after = &rest[2..];
            if after.starts_with(|c: char| c.is_ascii_alphabetic()) {
                let end = after.find('>').map(|i| i + 1).unwrap_or(after.len());
                let name: String = after[..end]
                    .chars()
                    .take_while(|c| !c.is_whitespace() && *c != '>' && *c != '/')
                    .collect::<String>()
                    .to_ascii_lowercase();
                self.pos += 2 + end;
                return Some(Token::EndTag { name });
            }
        }
        if rest.starts_with('<') && rest[1..].starts_with(|c: char| c.is_ascii_alphabetic()) {
            return Some(self.start_tag());
        }
        // Text up to the next '<' that could begin markup.
        let bytes = rest.as_bytes();
        let mut i = if rest.starts_with('<') { 1 } else { 0 };
        while i < bytes.len() {
            if bytes[i] == b'<' {
                let next = bytes.get(i + 1).copied().unwrap_or(b' ');
                if next.is_ascii_alphabetic() || next == b'/' || next == b'!' || next == b'?' {
                    break;
                }
            }
            i += 1;
        }
        let text = &rest[..i];
        self.pos += i;
        Some(Token::Text(decode_entities(text)))
    }

    fn start_tag(&mut self) -> Token {
        let s = self.src;
        let b = s.as_bytes();
        let mut i = self.pos + 1;
        let name_start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' && b[i] != b'/' {
            i += 1;
        }
        let name = s[name_start..i].to_ascii_lowercase();
        let mut attrs: Vec<(String, String)> = Vec::new();
        let mut self_closing = false;
        loop {
            while i < b.len() && (b[i].is_ascii_whitespace()) {
                i += 1;
            }
            if i >= b.len() {
                break;
            }
            if b[i] == b'>' {
                i += 1;
                break;
            }
            if b[i] == b'/' {
                i += 1;
                if i < b.len() && b[i] == b'>' {
                    self_closing = true;
                    i += 1;
                    break;
                }
                continue;
            }
            let an_start = i;
            while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' && b[i] != b'=' && !(b[i] == b'/' && b.get(i + 1) == Some(&b'>')) {
                i += 1;
            }
            if i == an_start {
                i += 1;
                continue;
            }
            let aname = s[an_start..i].to_ascii_lowercase();
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            let mut value = String::new();
            if i < b.len() && b[i] == b'=' {
                i += 1;
                while i < b.len() && b[i].is_ascii_whitespace() {
                    i += 1;
                }
                if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                    let q = b[i];
                    i += 1;
                    let vs = i;
                    while i < b.len() && b[i] != q {
                        i += 1;
                    }
                    value = decode_entities(&s[vs..i]);
                    i = (i + 1).min(b.len());
                } else {
                    let vs = i;
                    while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' {
                        i += 1;
                    }
                    value = decode_entities(&s[vs..i]);
                }
            }
            if !attrs.iter().any(|(k, _)| *k == aname) {
                attrs.push((aname, value));
            }
        }
        self.pos = i;
        if RAW_TEXT.contains(&name.as_str()) && !self_closing {
            self.raw_until = Some(name.clone());
        }
        Token::StartTag { name, attrs, self_closing }
    }
}

pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest[1..].find(|c: char| !(c.is_ascii_alphanumeric() || c == '#')).map(|e| e + 1).unwrap_or(rest.len());
        let name = &rest[1..end];
        let has_semi = rest[end..].starts_with(';');
        let decoded = if let Some(num) = name.strip_prefix('#') {
            let code = if let Some(hex) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
                u32::from_str_radix(hex, 16).ok()
            } else {
                num.parse::<u32>().ok()
            };
            code.and_then(char::from_u32).map(|c| c.to_string())
        } else {
            named_entity(name).map(|s| s.to_string())
        };
        match decoded {
            Some(d) if !name.is_empty() => {
                out.push_str(&d);
                rest = &rest[end + if has_semi { 1 } else { 0 }..];
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn named_entity(name: &str) -> Option<&'static str> {
    Some(match name {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => "\u{a0}",
        "copy" => "©",
        "reg" => "®",
        "trade" => "™",
        "hellip" => "…",
        "mdash" => "—",
        "ndash" => "–",
        "lsquo" => "‘",
        "rsquo" => "’",
        "ldquo" => "“",
        "rdquo" => "”",
        "laquo" => "«",
        "raquo" => "»",
        "bull" => "•",
        "middot" => "·",
        "times" => "×",
        "divide" => "÷",
        "deg" => "°",
        "plusmn" => "±",
        "para" => "¶",
        "sect" => "§",
        "euro" => "€",
        "pound" => "£",
        "yen" => "¥",
        "cent" => "¢",
        "larr" => "←",
        "rarr" => "→",
        "uarr" => "↑",
        "darr" => "↓",
        "harr" => "↔",
        "check" => "✓",
        "hearts" => "♥",
        "star" => "☆",
        "frac12" => "½",
        "frac14" => "¼",
        "frac34" => "¾",
        "sup2" => "²",
        "sup3" => "³",
        "iexcl" => "¡",
        "iquest" => "¿",
        "shy" => "\u{ad}",
        "zwj" => "\u{200d}",
        "zwnj" => "\u{200c}",
        "ensp" => "\u{2002}",
        "emsp" => "\u{2003}",
        "thinsp" => "\u{2009}",
        "eacute" => "é",
        "egrave" => "è",
        "agrave" => "à",
        "aacute" => "á",
        "ccedil" => "ç",
        "ntilde" => "ñ",
        "ouml" => "ö",
        "uuml" => "ü",
        "auml" => "ä",
        "szlig" => "ß",
        "alpha" => "α",
        "beta" => "β",
        "pi" => "π",
        "infin" => "∞",
        "ne" => "≠",
        "le" => "≤",
        "ge" => "≥",
        _ => return None,
    })
}

fn closes_p(tag: &str) -> bool {
    matches!(
        tag,
        "address" | "article" | "aside" | "blockquote" | "details" | "div" | "dl" | "fieldset"
            | "figcaption" | "figure" | "footer" | "form" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
            | "header" | "hr" | "main" | "menu" | "nav" | "ol" | "p" | "pre" | "section" | "table"
            | "ul" | "dd" | "dt" | "li"
    )
}

fn is_head_tag(tag: &str) -> bool {
    matches!(tag, "title" | "meta" | "link" | "style" | "script" | "base" | "noscript")
}

struct TreeBuilder<'d> {
    doc: &'d mut Document,
    stack: Vec<NodeId>,
    html: Option<NodeId>,
    head: Option<NodeId>,
    body: Option<NodeId>,
    fragment: bool,
}

impl<'d> TreeBuilder<'d> {
    fn current(&self) -> NodeId {
        *self.stack.last().unwrap()
    }

    fn current_tag(&self) -> Option<&str> {
        self.doc.tag(self.current())
    }

    fn ensure_html(&mut self) {
        if self.fragment || self.html.is_some() {
            return;
        }
        let html = self.doc.create_element("html");
        self.doc.append_child(0, html);
        self.html = Some(html);
        self.stack = vec![0, html];
    }

    fn ensure_head(&mut self) {
        self.ensure_html();
        if self.fragment || self.head.is_some() {
            return;
        }
        let head = self.doc.create_element("head");
        self.doc.append_child(self.html.unwrap(), head);
        self.head = Some(head);
    }

    fn ensure_body(&mut self) {
        if self.fragment {
            return;
        }
        self.ensure_head();
        if self.body.is_some() {
            if !self.stack.contains(&self.body.unwrap()) {
                self.stack = vec![0, self.html.unwrap(), self.body.unwrap()];
            }
            return;
        }
        let body = self.doc.create_element("body");
        self.doc.append_child(self.html.unwrap(), body);
        self.body = Some(body);
        self.stack = vec![0, self.html.unwrap(), body];
    }

    fn in_body(&self) -> bool {
        self.fragment || self.body.is_some_and(|b| self.stack.contains(&b))
    }

    fn pop_until(&mut self, tag: &str) -> bool {
        if let Some(pos) = self.stack.iter().rposition(|&n| self.doc.tag_is(n, tag)) {
            if pos == 0 {
                return false;
            }
            self.stack.truncate(pos);
            true
        } else {
            false
        }
    }

    fn has_in_scope(&self, tag: &str) -> bool {
        for &n in self.stack.iter().rev() {
            match self.doc.tag(n) {
                Some(t) if t == tag => return true,
                Some("table" | "html" | "body" | "td" | "th" | "button") if tag != "table" => {
                    return false;
                }
                _ => {}
            }
        }
        false
    }

    fn insert(&mut self, node: NodeId) {
        let parent = self.current();
        self.doc.append_child(parent, node);
    }

    fn text(&mut self, text: String) {
        let ws_only = text.chars().all(|c| c.is_ascii_whitespace());
        if !self.fragment && !self.in_body() {
            let in_head_raw = self.stack.len() > 2 && self.head.is_some_and(|h| self.stack.contains(&h));
            if ws_only {
                if in_head_raw {
                    let t = self.doc.create_text(&text);
                    self.insert(t);
                }
                return;
            }
            if !in_head_raw {
                self.ensure_body();
            }
        }
        // Merge adjacent text nodes.
        let parent = self.current();
        if let Some(&last) = self.doc.nodes[parent].children.last() {
            if let crate::dom::NodeData::Text(t) = &mut self.doc.nodes[last].data {
                t.push_str(&text);
                return;
            }
        }
        // Avoid stray whitespace directly inside table structure.
        if ws_only && matches!(self.current_tag(), Some("table" | "tbody" | "thead" | "tfoot" | "tr")) {
            return;
        }
        let t = self.doc.create_text(&text);
        self.insert(t);
    }

    fn start(&mut self, name: String, attrs: Vec<(String, String)>, self_closing: bool) {
        match name.as_str() {
            "html" => {
                if self.fragment {
                    return;
                }
                self.ensure_html();
                let html = self.html.unwrap();
                for (k, v) in attrs {
                    if !self.doc.has_attr(html, &k) {
                        self.doc.set_attr(html, &k, &v);
                    }
                }
                return;
            }
            "head" => {
                if self.fragment {
                    return;
                }
                self.ensure_head();
                let head = self.head.unwrap();
                if self.body.is_none() {
                    self.stack = vec![0, self.html.unwrap(), head];
                }
                return;
            }
            "body" => {
                if self.fragment {
                    return;
                }
                let existed = self.body.is_some();
                self.ensure_body();
                let body = self.body.unwrap();
                if !existed || self.doc.element(body).is_some_and(|e| e.attrs.is_empty()) {
                    for (k, v) in attrs {
                        self.doc.set_attr(body, &k, &v);
                    }
                }
                return;
            }
            _ => {}
        }

        if !self.fragment {
            if is_head_tag(&name) && self.body.is_none() {
                self.ensure_head();
                let head = self.head.unwrap();
                if !self.stack.contains(&head) {
                    self.stack = vec![0, self.html.unwrap(), head];
                }
            } else {
                self.ensure_body();
            }
        }

        // Implied end tags.
        if closes_p(&name) && self.has_in_scope("p") {
            self.pop_until("p");
        }
        match name.as_str() {
            "li" => {
                if let Some(pos) = self.stack.iter().rposition(|&n| matches!(self.doc.tag(n), Some("li" | "ul" | "ol"))) {
                    if self.doc.tag_is(self.stack[pos], "li") {
                        self.stack.truncate(pos);
                    }
                }
            }
            "dt" | "dd" => {
                if let Some(pos) = self.stack.iter().rposition(|&n| matches!(self.doc.tag(n), Some("dt" | "dd" | "dl"))) {
                    if !self.doc.tag_is(self.stack[pos], "dl") {
                        self.stack.truncate(pos);
                    }
                }
            }
            "option" => {
                if self.current_tag() == Some("option") {
                    self.stack.pop();
                }
            }
            "tr" => {
                self.close_table_part(&["tr"], &["table", "tbody", "thead", "tfoot"]);
                if self.current_tag() == Some("table") {
                    let tb = self.doc.create_element("tbody");
                    self.insert(tb);
                    self.stack.push(tb);
                }
            }
            "td" | "th" => {
                self.close_table_part(&["td", "th"], &["tr", "table"]);
                if matches!(self.current_tag(), Some("table" | "tbody" | "thead" | "tfoot")) {
                    if self.current_tag() == Some("table") {
                        let tb = self.doc.create_element("tbody");
                        self.insert(tb);
                        self.stack.push(tb);
                    }
                    let tr = self.doc.create_element("tr");
                    self.insert(tr);
                    self.stack.push(tr);
                }
            }
            "tbody" | "thead" | "tfoot" => {
                self.close_table_part(&["tbody", "thead", "tfoot"], &["table"]);
            }
            "a" => {
                // Nested anchors are not allowed.
                if self.stack.iter().any(|&n| self.doc.tag_is(n, "a")) {
                    self.pop_until("a");
                }
            }
            _ => {}
        }

        let el = self.doc.create_element_with_attrs(&name, attrs);
        self.insert(el);
        let is_void = VOID_ELEMENTS.contains(&name.as_str());
        if !is_void && !(self_closing && is_foreign_or_custom(&name)) {
            self.stack.push(el);
        }
    }

    fn close_table_part(&mut self, close: &[&str], stop: &[&str]) {
        for i in (1..self.stack.len()).rev() {
            let t = self.doc.tag(self.stack[i]).unwrap_or("");
            if close.contains(&t) {
                self.stack.truncate(i);
                return;
            }
            if stop.contains(&t) {
                return;
            }
        }
    }

    fn end(&mut self, name: String) {
        match name.as_str() {
            "html" | "body" if !self.fragment => {
                return;
            }
            "head" if !self.fragment => {
                if let Some(html) = self.html {
                    if self.body.is_none() {
                        self.stack = vec![0, html];
                    }
                }
                return;
            }
            "p" => {
                if !self.has_in_scope("p") {
                    let p = self.doc.create_element("p");
                    self.insert(p);
                    return;
                }
            }
            "br" => {
                self.start("br".into(), vec![], true);
                return;
            }
            _ => {}
        }
        // Don't let a stray end tag pop past table/body boundaries.
        if let Some(pos) = self.stack.iter().rposition(|&n| self.doc.tag_is(n, &name)) {
            if pos > 0 {
                let crosses_boundary = self.stack[pos + 1..]
                    .iter()
                    .any(|&n| matches!(self.doc.tag(n), Some("table")) && name != "table" && !matches!(name.as_str(), "tbody" | "thead" | "tfoot" | "tr" | "td" | "th"));
                if !crosses_boundary {
                    self.stack.truncate(pos);
                }
            }
        }
    }
}

fn is_foreign_or_custom(name: &str) -> bool {
    name.contains('-') || matches!(name, "svg" | "path" | "circle" | "rect" | "line" | "polyline" | "polygon" | "g" | "use" | "ellipse" | "stop")
}

fn build(doc: &mut Document, src: &str, fragment_parent: Option<NodeId>) {
    let mut tb = TreeBuilder {
        stack: vec![fragment_parent.unwrap_or(0)],
        doc,
        html: None,
        head: None,
        body: None,
        fragment: fragment_parent.is_some(),
    };
    let mut tok = Tokenizer::new(src);
    while let Some(t) = tok.next() {
        match t {
            Token::Doctype => {}
            Token::Comment(c) => {
                let n = tb.doc.create_comment(&c);
                tb.insert(n);
            }
            Token::Text(t) => tb.text(t),
            Token::StartTag { name, attrs, self_closing } => tb.start(name, attrs, self_closing),
            Token::EndTag { name } => tb.end(name),
        }
    }
    if !tb.fragment {
        tb.ensure_body();
    }
}

pub fn parse_document(src: &str) -> Document {
    let mut doc = Document::new();
    build(&mut doc, src, None);
    doc
}

/// Parse `src` as a fragment and append the resulting nodes to `parent`.
pub fn parse_fragment_into(doc: &mut Document, parent: NodeId, src: &str) {
    build(doc, src, Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implied_structure() {
        let doc = parse_document("<title>Hi</title><p>One<p>Two");
        let body = doc.body().unwrap();
        assert_eq!(doc.element_children(body).len(), 2);
        assert_eq!(doc.title(), "Hi");
    }

    #[test]
    fn entities_and_attrs() {
        let doc = parse_document(r#"<div id=a class="x y" data-v='1'>a &amp; b &lt;3 &#x41;</div>"#);
        let d = doc.get_element_by_id("a").unwrap();
        assert_eq!(doc.text_content(d), "a & b <3 A");
        assert!(doc.has_class(d, "y"));
        assert_eq!(doc.attr(d, "data-v"), Some("1"));
    }

    #[test]
    fn script_raw_text() {
        let doc = parse_document("<script>if (a < b && c > d) {}</script><p>x</p>");
        let s = doc.elements_by_tag(0, "script")[0];
        assert_eq!(doc.text_content(s), "if (a < b && c > d) {}");
    }

    #[test]
    fn lists_and_tables() {
        let doc = parse_document("<ul><li>a<li>b</ul><table><tr><td>1<td>2</table>");
        assert_eq!(doc.elements_by_tag(0, "li").len(), 2);
        assert_eq!(doc.elements_by_tag(0, "tbody").len(), 1);
        assert_eq!(doc.elements_by_tag(0, "td").len(), 2);
    }

    #[test]
    fn fragment() {
        let mut doc = parse_document("<div id=x></div>");
        let x = doc.get_element_by_id("x").unwrap();
        parse_fragment_into(&mut doc, x, "<b>bold</b> text");
        assert_eq!(doc.inner_html(x), "<b>bold</b> text");
    }
}
