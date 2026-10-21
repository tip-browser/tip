//! DOM bindings: exposes the document to JavaScript.

pub mod cssom;
pub mod events;
pub mod scripts;
pub mod window;

use std::collections::HashMap;
use std::rc::Rc;

use crate::css;
use crate::dom::{NodeData, NodeId};
use crate::js::interp::{arg, throw, Interp, R};
use crate::js::value::*;
use crate::page::PageHost;

pub fn ph(it: &mut Interp) -> &mut PageHost {
    it.host_mut::<PageHost>().expect("page host")
}

pub fn ph_ref(it: &Interp) -> &PageHost {
    it.host::<PageHost>().expect("page host")
}

/// Prototype objects for DOM wrappers.
pub struct DomProtos {
    pub event_target: Obj,
    pub node: Obj,
    pub element: Obj,
    pub html_element: Obj,
    pub document: Obj,
    pub text: Obj,
    pub comment: Obj,
    pub fragment: Obj,
    pub by_tag: HashMap<&'static str, Obj>,
    pub style_decl: Obj,
    pub token_list: Obj,
    pub event: Obj,
}

fn protos(it: &Interp) -> Rc<DomProtos> {
    it.ext.get("dom_protos").cloned().and_then(|p| p.downcast::<DomProtos>().ok()).expect("DOM prototypes not installed")
}

// ---------------------------------------------------------------- host objects

pub struct NodeHost(pub NodeId);
pub struct StyleHost(pub NodeId, pub bool /* computed */);
pub struct ClassListHost(pub NodeId);
pub struct DatasetHost(pub NodeId);

pub fn node_of(v: &Value) -> Option<NodeId> {
    let Value::Obj(o) = v else { return None };
    let h = o.host()?;
    h.as_any().downcast_ref::<NodeHost>().map(|n| n.0)
}

pub fn this_node(it: &mut Interp, this: &Value) -> R<NodeId> {
    if let Some(n) = node_of(this) {
        return Ok(n);
    }
    if let Value::Obj(o) = this {
        if o.ptr_eq(&it.global) {
            return Ok(0);
        }
    }
    throw!(it, "TypeError", "Illegal invocation")
}

pub fn wrap(it: &mut Interp, id: NodeId) -> Value {
    if let Some(o) = ph(it).wrappers.get(&id) {
        return Value::Obj(o.clone());
    }
    let p = protos(it);
    let (proto, class) = {
        let h = ph(it);
        match &h.doc.node(id).data {
            NodeData::Document => (p.document.clone(), "HTMLDocument"),
            NodeData::Fragment => (p.fragment.clone(), "DocumentFragment"),
            NodeData::Text(_) => (p.text.clone(), "Text"),
            NodeData::Comment(_) => (p.comment.clone(), "Comment"),
            NodeData::Element(e) => (p.by_tag.get(e.tag.as_str()).cloned().unwrap_or_else(|| p.html_element.clone()), "HTMLElement"),
        }
    };
    let o = Obj::new(ObjKind::Host(Rc::new(NodeHost(id))), Some(proto), class);
    ph(it).wrappers.insert(id, o.clone());
    Value::Obj(o)
}

pub fn wrap_opt(it: &mut Interp, id: Option<NodeId>) -> Value {
    match id {
        Some(i) => wrap(it, i),
        None => Value::Null,
    }
}

pub fn node_list(it: &mut Interp, ids: Vec<NodeId>) -> Value {
    let items: Vec<Value> = ids.into_iter().map(|i| wrap(it, i)).collect();
    let arr = it.new_array(items);
    if let Value::Obj(o) = &arr {
        o.borrow_mut().class = "NodeList";
        let tag = PropKey::Sym(it.sym_to_string_tag.clone());
        o.borrow_mut().props.insert(tag, Prop::hidden(Value::str("NodeList")));
        let item = it.native("item", 1, |it, this, args| {
            let i = it.to_integer(&arg(args, 0))?;
            let v = it.get(&this, &PropKey::Str(Rc::from((i.max(0.0) as usize).to_string().as_str())))?;
            Ok(if v.is_undefined() { Value::Null } else { v })
        });
        o.set_hidden("item", item);
    }
    arr
}

fn camel_to_kebab(s: &str) -> String {
    if s.starts_with("--") {
        return s.to_string();
    }
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    if out.starts_with("webkit-") || out.starts_with("moz-") || out.starts_with("ms-") {
        out.insert(0, '-');
    }
    if out == "css-float" {
        return "float".into();
    }
    out
}

fn kebab_to_camel(s: &str) -> String {
    let mut out = String::new();
    let mut up = false;
    for c in s.chars() {
        if c == '-' {
            up = true;
        } else if up {
            out.push(c.to_ascii_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    out
}

const BLOCK_TAGS: &[&str] = &["div", "p", "h1", "h2", "h3", "h4", "h5", "h6", "li", "ul", "ol", "section", "article", "header", "footer", "nav", "main", "aside", "pre", "blockquote", "table", "tr", "form", "dl", "dt", "dd", "figure", "figcaption", "hr", "address", "details", "summary"];

fn inner_text(h: &PageHost, n: NodeId) -> String {
    fn walk(h: &PageHost, n: NodeId, out: &mut String) {
        match &h.doc.node(n).data {
            NodeData::Text(t) => {
                let pre = h.doc.parent(n).is_some_and(|p| matches!(h.doc.tag(p), Some("pre" | "textarea")));
                if pre {
                    out.push_str(t);
                } else {
                    for c in t.chars() {
                        if c.is_whitespace() {
                            if !out.ends_with([' ', '\n']) && !out.is_empty() {
                                out.push(' ');
                            }
                        } else {
                            out.push(c);
                        }
                    }
                }
            }
            NodeData::Element(e) => {
                if matches!(e.tag.as_str(), "script" | "style" | "head" | "template" | "noscript") || h.doc.has_attr(n, "hidden") {
                    return;
                }
                if e.tag == "br" {
                    while out.ends_with(' ') {
                        out.pop();
                    }
                    out.push('\n');
                    return;
                }
                let block = BLOCK_TAGS.contains(&e.tag.as_str());
                if block && !out.is_empty() && !out.ends_with('\n') {
                    while out.ends_with(' ') {
                        out.pop();
                    }
                    out.push('\n');
                }
                for &c in h.doc.children(n) {
                    walk(h, c, out);
                }
                if block && !out.ends_with('\n') {
                    while out.ends_with(' ') {
                        out.pop();
                    }
                    out.push('\n');
                }
                if matches!(e.tag.as_str(), "td" | "th") {
                    out.push('\t');
                }
            }
            _ => {
                for &c in h.doc.children(n) {
                    walk(h, c, out);
                }
            }
        }
    }
    let mut out = String::new();
    walk(h, n, &mut out);
    out.trim_matches(|c: char| c == '\n' || c == ' ').to_string()
}

fn rect_obj(it: &mut Interp, x: f32, y: f32, w: f32, h: f32) -> Value {
    let o = it.new_object();
    for (k, v) in [("x", x), ("y", y), ("width", w), ("height", h), ("left", x), ("top", y), ("right", x + w), ("bottom", y + h)] {
        o.set_prop(k, Value::Num(v as f64));
    }
    let json = it.native("toJSON", 0, |_, this, _| Ok(this));
    o.set_hidden("toJSON", json);
    Value::Obj(o)
}

fn client_rect(it: &mut Interp, n: NodeId) -> Value {
    let h = ph(it);
    let sc = h.scroll;
    let r = h.node_rect(n);
    match r {
        Some(r) => {
            let fixed = h.layout().find_box(n).is_some_and(|b| b.fixed);
            let (dx, dy) = if fixed { (0.0, 0.0) } else { sc };
            rect_obj(it, r.x - dx, r.y - dy, r.w, r.h)
        }
        None => rect_obj(it, 0.0, 0.0, 0.0, 0.0),
    }
}

fn offset_parent(h: &mut PageHost, n: NodeId) -> Option<NodeId> {
    let styles = h.styles();
    let mut cur = h.doc.parent(n);
    while let Some(p) = cur {
        if !h.doc.is_element(p) {
            break;
        }
        if h.doc.tag_is(p, "body") {
            return Some(p);
        }
        if styles.get(p).is_some_and(|s| s.is_positioned()) || matches!(h.doc.tag(p), Some("td" | "th" | "table")) {
            return Some(p);
        }
        cur = h.doc.parent(p);
    }
    h.doc.body()
}

fn resolved_attr_url(h: &PageHost, n: NodeId, attr: &str) -> String {
    match h.doc.attr(n, attr) {
        Some(v) => h.resolve_url(v).map(|u| u.to_string()).unwrap_or_else(|| v.to_string()),
        None => String::new(),
    }
}

fn str_v(s: impl AsRef<str>) -> Value {
    Value::str(s.as_ref())
}

const REFLECTED: &[&str] = &["id", "title", "lang", "dir", "name", "alt", "placeholder", "rel", "target", "role", "accessKey", "autocomplete", "min", "max", "step", "pattern", "enctype", "download", "hreflang", "media", "sizes", "srcset", "crossOrigin", "loading", "inputMode", "enterKeyHint", "label", "colSpan", "rowSpan", "width", "height", "maxLength", "size", "rows", "cols", "wrap", "accept", "as", "integrity", "nonce", "referrerPolicy", "shape", "coords", "headers", "scope", "abbr", "datetime", "cite", "content", "httpEquiv", "charset", "frameBorder", "scrolling", "srcdoc", "allow", "marginWidth", "marginHeight", "border", "align", "valign", "bgColor"];
/// The content attribute behind a reflected IDL property.
fn reflected_attr(k: &str) -> String {
    match k {
        "httpEquiv" => "http-equiv".into(),
        "acceptCharset" => "accept-charset".into(),
        _ => k.to_ascii_lowercase(),
    }
}

const BOOL_REFLECTED: &[&str] = &["disabled", "hidden", "required", "readOnly", "multiple", "autofocus", "noValidate", "defer", "async", "open", "reversed", "allowFullscreen", "draggable", "spellcheck", "autoplay", "controls", "loop", "muted", "novalidate", "formNoValidate", "isMap", "default", "inert"];

impl HostObject for NodeHost {
    fn class_name(&self) -> &'static str {
        "Node"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn inspect(&self, it: &mut Interp) -> Option<String> {
        let h = ph(it);
        Some(match &h.doc.node(self.0).data {
            NodeData::Element(e) => {
                let mut s = format!("<{}", e.tag);
                for (k, v) in e.attrs.iter().take(4) {
                    s.push_str(&format!(" {k}=\"{}\"", v.chars().take(40).collect::<String>()));
                }
                s.push('>');
                s
            }
            NodeData::Text(t) => format!("#text \"{}\"", t.chars().take(60).collect::<String>()),
            NodeData::Comment(t) => format!("<!--{t}-->"),
            NodeData::Document => "#document".into(),
            NodeData::Fragment => "#document-fragment".into(),
        })
    }

    fn get(&self, it: &mut Interp, key: &PropKey) -> Option<R<Value>> {
        let k = key.as_str()?;
        let n = self.0;
        if matches!(k, "contentWindow" | "contentDocument") && ph(it).doc.tag_is(n, "iframe") {
            // other frames are separate windows: reachable through a WindowProxy only
            return Some(Ok(if k == "contentWindow" { crate::frames::content_window(it, n) } else { Value::Null }));
        }
        let h = ph(it);
        let is_el = h.doc.is_element(n);
        let tag = h.doc.tag(n).unwrap_or("").to_string();
        let v = match k {
            "nodeType" => Value::Num(match &h.doc.node(n).data {
                NodeData::Element(_) => 1.0,
                NodeData::Text(_) => 3.0,
                NodeData::Comment(_) => 8.0,
                NodeData::Document => 9.0,
                NodeData::Fragment => 11.0,
            }),
            "nodeName" => str_v(match &h.doc.node(n).data {
                NodeData::Element(e) => e.tag.to_uppercase(),
                NodeData::Text(_) => "#text".into(),
                NodeData::Comment(_) => "#comment".into(),
                NodeData::Document => "#document".into(),
                NodeData::Fragment => "#document-fragment".into(),
            }),
            "tagName" if is_el => str_v(tag.to_uppercase()),
            "localName" if is_el => str_v(&tag),
            "namespaceURI" if is_el => str_v("http://www.w3.org/1999/xhtml"),
            "textContent" => {
                if n == 0 {
                    Value::Null
                } else {
                    str_v(h.doc.text_content(n))
                }
            }
            // HTMLScriptElement / HTMLTitleElement / HTMLAnchorElement / HTMLOptionElement .text
            "text" if matches!(tag.as_str(), "script" | "title" | "a" | "option") => str_v(h.doc.text_content(n)),
            "nodeValue" | "data" | "wholeText" => match &h.doc.node(n).data {
                NodeData::Text(t) | NodeData::Comment(t) => str_v(t),
                _ => Value::Null,
            },
            "length" if !is_el => match &h.doc.node(n).data {
                NodeData::Text(t) | NodeData::Comment(t) => Value::Num(utf16_len(t) as f64),
                _ => return None,
            },
            "innerHTML" => str_v(h.doc.inner_html(n)),
            "outerHTML" => str_v(h.doc.outer_html(n)),
            "innerText" | "outerText" => str_v(inner_text(h, n)),
            "id" => str_v(h.doc.attr(n, "id").unwrap_or("")),
            "className" => str_v(h.doc.attr(n, "class").unwrap_or("")),
            "parentNode" => {
                let p = h.doc.parent(n);
                return Some(Ok(wrap_opt(it, p)));
            }
            "parentElement" => {
                let p = h.doc.parent(n).filter(|&p| h.doc.is_element(p));
                return Some(Ok(wrap_opt(it, p)));
            }
            "childNodes" => {
                let c = h.doc.children(n).to_vec();
                return Some(Ok(node_list(it, c)));
            }
            "children" => {
                let c = h.doc.element_children(n);
                return Some(Ok(node_list(it, c)));
            }
            "childElementCount" => Value::Num(h.doc.element_children(n).len() as f64),
            "firstChild" => {
                let c = h.doc.children(n).first().copied();
                return Some(Ok(wrap_opt(it, c)));
            }
            "lastChild" => {
                let c = h.doc.children(n).last().copied();
                return Some(Ok(wrap_opt(it, c)));
            }
            "firstElementChild" => {
                let c = h.doc.element_children(n).first().copied();
                return Some(Ok(wrap_opt(it, c)));
            }
            "lastElementChild" => {
                let c = h.doc.element_children(n).last().copied();
                return Some(Ok(wrap_opt(it, c)));
            }
            "nextSibling" => {
                let c = h.doc.next_sibling(n);
                return Some(Ok(wrap_opt(it, c)));
            }
            "previousSibling" => {
                let c = h.doc.prev_sibling(n);
                return Some(Ok(wrap_opt(it, c)));
            }
            "nextElementSibling" => {
                let c = h.doc.next_element_sibling(n);
                return Some(Ok(wrap_opt(it, c)));
            }
            "previousElementSibling" => {
                let c = h.doc.prev_element_sibling(n);
                return Some(Ok(wrap_opt(it, c)));
            }
            "isConnected" => Value::Bool(h.doc.is_connected(n)),
            "ownerDocument" => {
                if n == 0 {
                    Value::Null
                } else {
                    return Some(Ok(wrap(it, 0)));
                }
            }
            "attributes" if is_el => {
                // a NamedNodeMap of Attr objects (prototypes from the prelude)
                let attrs = h.doc.element(n).map(|e| e.attrs.clone()).unwrap_or_default();
                let proto_of = |it: &mut Interp, name: &str| it.global.get_own(name).and_then(|c| c.as_obj().and_then(|c| c.get_own("prototype"))).and_then(|p| p.as_obj().cloned());
                let attr_proto = proto_of(it, "Attr").unwrap_or_else(|| it.p.object.clone());
                let map_proto = proto_of(it, "NamedNodeMap").unwrap_or_else(|| it.p.object.clone());
                let owner = wrap(it, n);
                let map = Obj::new(ObjKind::Ordinary, Some(map_proto), "NamedNodeMap");
                let len = attrs.len();
                for (i, (k, v)) in attrs.into_iter().enumerate() {
                    let o = Obj::new(ObjKind::Ordinary, Some(attr_proto.clone()), "Attr");
                    o.set_hidden("name", str_v(&k));
                    o.set_hidden("localName", str_v(&k));
                    o.set_hidden("nodeName", str_v(&k));
                    o.set_hidden("_value", str_v(&v));
                    o.set_hidden("ownerElement", owner.clone());
                    map.set_prop(&i.to_string(), Value::Obj(o));
                }
                map.set_hidden("length", Value::Num(len as f64));
                map.set_hidden("_owner", owner);
                return Some(Ok(Value::Obj(map)));
            }
            "style" if is_el => Value::Obj(Obj::new(ObjKind::Host(Rc::new(StyleHost(n, false))), Some(protos(it).style_decl.clone()), "CSSStyleDeclaration")),
            "classList" if is_el => Value::Obj(Obj::new(ObjKind::Host(Rc::new(ClassListHost(n))), Some(protos(it).token_list.clone()), "DOMTokenList")),
            "dataset" if is_el => Value::Obj(Obj::new(ObjKind::Host(Rc::new(DatasetHost(n))), Some(it.p.object.clone()), "DOMStringMap")),
            "value" if matches!(tag.as_str(), "input" | "textarea" | "select" | "option" | "button" | "output" | "li" | "progress" | "meter" | "param" | "data") => {
                if tag == "option" {
                    str_v(h.option_value(n))
                } else if matches!(tag.as_str(), "input" | "textarea" | "select") {
                    str_v(h.control_value(n))
                } else {
                    str_v(h.doc.attr(n, "value").unwrap_or(""))
                }
            }
            "valueAsNumber" if tag == "input" => Value::Num(h.control_value(n).trim().parse::<f64>().unwrap_or(f64::NAN)),
            "defaultValue" => str_v(if tag == "textarea" { h.doc.text_content(n) } else { h.doc.attr(n, "value").unwrap_or("").to_string() }),
            "checked" if tag == "input" => Value::Bool(h.is_checked(n)),
            "defaultChecked" => Value::Bool(h.doc.has_attr(n, "checked")),
            "selected" if tag == "option" => {
                let sel = h.doc.parent(n).and_then(|p| if h.doc.tag_is(p, "select") { Some(p) } else { h.doc.parent(p) });
                Value::Bool(match sel {
                    Some(s) => h.control_value(s) == h.option_value(n),
                    None => h.doc.has_attr(n, "selected"),
                })
            }
            "selectedIndex" if tag == "select" => Value::Num(h.selected_index(n) as f64),
            "options" if tag == "select" => {
                let o = h.doc.elements_by_tag(n, "option");
                return Some(Ok(node_list(it, o)));
            }
            "type" if is_el => str_v(match tag.as_str() {
                "input" => h.doc.attr(n, "type").unwrap_or("text").to_ascii_lowercase(),
                "button" => h.doc.attr(n, "type").unwrap_or("submit").to_ascii_lowercase(),
                "select" => if h.doc.has_attr(n, "multiple") { "select-multiple".into() } else { "select-one".into() },
                "textarea" => "textarea".into(),
                _ => h.doc.attr(n, "type").unwrap_or("").to_string(),
            }),
            "href" if matches!(tag.as_str(), "a" | "link" | "area" | "base") => str_v(resolved_attr_url(h, n, "href")),
            "src" if matches!(tag.as_str(), "img" | "script" | "iframe" | "audio" | "video" | "source" | "embed" | "input" | "track") => str_v(resolved_attr_url(h, n, "src")),
            "action" if tag == "form" => str_v(if h.doc.has_attr(n, "action") { resolved_attr_url(h, n, "action") } else { h.url.to_string() }),
            "method" if tag == "form" => str_v(h.doc.attr(n, "method").unwrap_or("get").to_ascii_lowercase()),
            "htmlFor" => str_v(h.doc.attr(n, "for").unwrap_or("")),
            "tabIndex" => Value::Num(h.doc.attr(n, "tabindex").and_then(|v| v.trim().parse().ok()).unwrap_or(if h.is_focusable(n) { 0.0 } else { -1.0 })),
            "form" => {
                let mut cur = h.doc.parent(n);
                while let Some(p) = cur {
                    if h.doc.tag_is(p, "form") {
                        break;
                    }
                    cur = h.doc.parent(p);
                }
                return Some(Ok(wrap_opt(it, cur)));
            }
            "elements" if tag == "form" => {
                let els: Vec<NodeId> = h.doc.element_descendants(n).into_iter().filter(|&e| matches!(h.doc.tag(e), Some("input" | "select" | "textarea" | "button"))).collect();
                return Some(Ok(node_list(it, els)));
            }
            "content" if tag == "template" => {
                // template contents are kept as regular children; expose a fragment clone
                let kids = h.doc.children(n).to_vec();
                let frag = h.doc.create_fragment();
                for k in kids {
                    let c = h.doc.clone_node(k, true);
                    h.doc.append_child(frag, c);
                }
                return Some(Ok(wrap(it, frag)));
            }
            "naturalWidth" | "naturalHeight" if tag == "img" => {
                let url = h.image_url_for(n);
                let size = url.and_then(|u| match h.images.get(&u) {
                    Some(crate::page::ImageEntry::Loaded(i)) => Some((i.w, i.h)),
                    _ => None,
                });
                Value::Num(size.map(|(w, hh)| if k == "naturalWidth" { w } else { hh }).unwrap_or(0) as f64)
            }
            "complete" if tag == "img" => {
                h.sync_images();
                let url = h.image_url_for(n);
                Value::Bool(url.is_none_or(|u| !matches!(h.images.get(&u), Some(crate::page::ImageEntry::Loading))))
            }
            "offsetWidth" | "offsetHeight" | "clientWidth" | "clientHeight" | "scrollWidth" | "scrollHeight" => {
                if n == 0 {
                    return None;
                }
                let is_root = h.doc.document_element() == Some(n) || (h.doc.body() == Some(n) && k.starts_with("scroll"));
                if is_root && (k.starts_with("client") || k.starts_with("scroll")) {
                    let (dw, dh) = h.doc_size();
                    let (vw, vh) = h.viewport;
                    return Some(Ok(Value::Num(match k {
                        "clientWidth" => vw,
                        "clientHeight" => vh,
                        "scrollWidth" => dw,
                        _ => dh,
                    } as f64)));
                }
                let l = h.layout();
                let v = match l.find_box(n) {
                    Some(b) => {
                        let bb = b.border_box();
                        let pb = b.padding_box();
                        match k {
                            "offsetWidth" => bb.w,
                            "offsetHeight" => bb.h,
                            "clientWidth" => pb.w,
                            "clientHeight" => pb.h,
                            "scrollWidth" => pb.w + b.scroll_max.0,
                            _ => pb.h + b.scroll_max.1,
                        }
                    }
                    None => l.bounding_rect(n).map(|r| if k.ends_with("Width") { r.w } else { r.h }).unwrap_or(0.0),
                };
                Value::Num(v.round() as f64)
            }
            "offsetTop" | "offsetLeft" => {
                let r = h.node_rect(n);
                let op = offset_parent(h, n);
                let pr = op.and_then(|p| if h.doc.tag_is(p, "body") { None } else { h.node_rect(p) });
                let v = match r {
                    Some(r) => {
                        if k == "offsetTop" {
                            r.y - pr.map(|p| p.y).unwrap_or(0.0)
                        } else {
                            r.x - pr.map(|p| p.x).unwrap_or(0.0)
                        }
                    }
                    None => 0.0,
                };
                Value::Num(v.round() as f64)
            }
            "offsetParent" => {
                let op = offset_parent(h, n);
                return Some(Ok(wrap_opt(it, op)));
            }
            "clientTop" | "clientLeft" => {
                let l = h.layout();
                Value::Num(l.find_box(n).map(|b| if k == "clientTop" { b.border.top } else { b.border.left }).unwrap_or(0.0) as f64)
            }
            "scrollTop" | "scrollLeft" => {
                let root = h.doc.document_element() == Some(n) || h.doc.body() == Some(n);
                let s = if root { h.scroll } else { h.elem_scroll.get(&n).copied().unwrap_or((0.0, 0.0)) };
                Value::Num(if k == "scrollTop" { s.1 } else { s.0 } as f64)
            }
            "contentEditable" => str_v(h.doc.attr(n, "contenteditable").unwrap_or("inherit")),
            "isContentEditable" => Value::Bool(h.doc.has_attr(n, "contenteditable")),
            // document-level properties
            "documentElement" if n == 0 => {
                let d = h.doc.document_element();
                return Some(Ok(wrap_opt(it, d)));
            }
            "body" if n == 0 => {
                let d = h.doc.body();
                return Some(Ok(wrap_opt(it, d)));
            }
            "head" if n == 0 => {
                let d = h.doc.head();
                return Some(Ok(wrap_opt(it, d)));
            }
            "title" if n == 0 => str_v(h.doc.title()),
            "readyState" if n == 0 => str_v(h.ready_state),
            "URL" | "documentURI" if n == 0 => str_v(h.url.to_string()),
            "baseURI" => str_v(h.resolve_url("").map(|u| u.to_string()).unwrap_or_else(|| h.url.to_string())),
            "domain" if n == 0 => str_v(&h.url.host),
            "referrer" if n == 0 => str_v(""),
            "characterSet" | "charset" | "inputEncoding" if n == 0 => str_v("UTF-8"),
            "contentType" if n == 0 => str_v("text/html"),
            "compatMode" if n == 0 => str_v("CSS1Compat"),
            "visibilityState" if n == 0 => str_v("visible"),
            "hidden" if n == 0 => Value::Bool(false),
            "cookie" if n == 0 => str_v(crate::net::document_cookie(&h.url)),
            "activeElement" if n == 0 => {
                let f = h.focus.or(h.doc.body());
                return Some(Ok(wrap_opt(it, f)));
            }
            "defaultView" if n == 0 => Value::Obj(it.global.clone()),
            "location" if n == 0 => return Some(it.get_str(&Value::Obj(it.global.clone()), "location")),
            "forms" | "images" | "links" | "scripts" | "anchors" if n == 0 => {
                let t = match k {
                    "forms" => "form",
                    "images" => "img",
                    "scripts" => "script",
                    _ => "a",
                };
                let list: Vec<NodeId> = h.doc.elements_by_tag(0, t).into_iter().filter(|&e| k != "links" || h.doc.has_attr(e, "href")).collect();
                return Some(Ok(node_list(it, list)));
            }
            "currentScript" if n == 0 => {
                let cs = window::current_script(it);
                return Some(Ok(cs));
            }
            "fonts" if n == 0 => {
                let o = it.new_object();
                let p = crate::js::builtins::promise::promise_resolve(it, Value::Undefined).unwrap_or(Value::Undefined);
                o.set_prop("ready", p);
                o.set_prop("status", str_v("loaded"));
                Value::Obj(o)
            }
            _ => {
                if is_el {
                    if REFLECTED.contains(&k) {
                        let attr = reflected_attr(k);
                        return Some(Ok(str_v(h.doc.attr(n, &attr).unwrap_or(""))));
                    }
                    if BOOL_REFLECTED.contains(&k) {
                        let attr = k.to_ascii_lowercase();
                        return Some(Ok(Value::Bool(h.doc.has_attr(n, &attr))));
                    }
                    if k.starts_with("on") && k.len() > 2 && k.chars().all(|c| c.is_ascii_lowercase()) {
                        // inline handler attributes are compiled lazily by the dispatcher
                        return Some(Ok(Value::Null));
                    }
                }
                return None;
            }
        };
        Some(Ok(v))
    }

    fn set(&self, it: &mut Interp, key: &PropKey, v: &Value) -> Option<R<()>> {
        let k = key.as_str()?;
        let n = self.0;
        // event handler properties are plain expandos
        if k.starts_with("on") {
            return None;
        }
        let tag = ph(it).doc.tag(n).unwrap_or("").to_string();
        let is_el = !tag.is_empty();
        let sval = |it: &mut Interp| -> R<String> {
            if v.is_nullish() && matches!(k, "textContent" | "innerHTML") {
                return Ok(String::new());
            }
            Ok(it.to_str(v)?.to_string())
        };
        let r: R<()> = (|| {
            match k {
                "textContent" | "innerText" | "outerText" | "nodeValue" | "data" | "text" if k != "text" || matches!(tag.as_str(), "script" | "title" | "a" | "option") => {
                    let s = sval(it)?;
                    let h = ph(it);
                    if k == "innerText" && s.contains('\n') && is_el {
                        h.doc.remove_all_children(n);
                        for (i, line) in s.split('\n').enumerate() {
                            if i > 0 {
                                let br = h.doc.create_element("br");
                                h.doc.append_child(n, br);
                            }
                            if !line.is_empty() {
                                let t = h.doc.create_text(line);
                                h.doc.append_child(n, t);
                            }
                        }
                    } else {
                        h.doc.set_text_content(n, &s);
                    }
                }
                "innerHTML" => {
                    let s = sval(it)?;
                    let h = ph(it);
                    h.doc.remove_all_children(n);
                    crate::html::parse_fragment_into(&mut h.doc, n, &s);
                    scripts::mark_started(it, n);
                }
                "outerHTML" => {
                    let s = sval(it)?;
                    let h = ph(it);
                    if let Some(p) = h.doc.parent(n) {
                        let frag = h.doc.create_fragment();
                        crate::html::parse_fragment_into(&mut h.doc, frag, &s);
                        scripts::mark_started(it, frag);
                        let h = ph(it);
                        let next = h.doc.next_sibling(n);
                        h.doc.detach(n);
                        h.doc.insert_before(p, frag, next);
                    }
                }
                "id" | "className" | "htmlFor" => {
                    let s = sval(it)?;
                    let attr = match k {
                        "className" => "class",
                        "htmlFor" => "for",
                        _ => "id",
                    };
                    ph(it).doc.set_attr(n, attr, &s);
                }
                "value" if is_el => {
                    let s = sval(it)?;
                    let h = ph(it);
                    let is_range = tag == "input" && h.doc.attr(n, "type").is_some_and(|t| t.eq_ignore_ascii_case("range"));
                    if is_range {
                        let v = h.sanitize_range(n, s.trim().parse::<f64>().unwrap_or(f64::NAN));
                        h.set_control_value(n, v);
                    } else if tag == "select" {
                        h.set_control_value(n, s);
                    } else if matches!(tag.as_str(), "input" | "textarea") {
                        h.set_control_value(n, s);
                    } else {
                        h.doc.set_attr(n, "value", &s);
                    }
                }
                "defaultValue" => {
                    let s = sval(it)?;
                    let h = ph(it);
                    if tag == "textarea" {
                        h.doc.set_text_content(n, &s);
                    } else {
                        h.doc.set_attr(n, "value", &s);
                    }
                }
                "valueAsNumber" if tag == "input" => {
                    let num = it.to_number(v)?;
                    let h = ph(it);
                    let is_range = h.doc.attr(n, "type").is_some_and(|t| t.eq_ignore_ascii_case("range"));
                    let s = if is_range { h.sanitize_range(n, num) } else { crate::js::value::num_to_string(num) };
                    h.set_control_value(n, s);
                }
                "checked" => {
                    let b = v.truthy();
                    ph(it).set_checked(n, b);
                }
                "selected" if tag == "option" => {
                    if v.truthy() {
                        let h = ph(it);
                        let sel = h.doc.parent(n).and_then(|p| if h.doc.tag_is(p, "select") { Some(p) } else { h.doc.parent(p) });
                        if let Some(s) = sel {
                            let val = h.option_value(n);
                            h.set_control_value(s, val);
                        }
                    }
                }
                "selectedIndex" if tag == "select" => {
                    let i = it.to_integer(v)?;
                    let h = ph(it);
                    let opts = h.doc.elements_by_tag(n, "option");
                    if let Some(&o) = opts.get(i.max(0.0) as usize) {
                        let val = h.option_value(o);
                        h.set_control_value(n, val);
                    }
                }
                "href" | "src" | "action" | "method" | "type" | "tabIndex" => {
                    let s = sval(it)?;
                    let attr = if k == "tabIndex" { "tabindex".to_string() } else { k.to_string() };
                    ph(it).doc.set_attr(n, &attr, &s);
                }
                "title" if n == 0 => {
                    let s = sval(it)?;
                    let h = ph(it);
                    let t = match h.doc.elements_by_tag(0, "title").first() {
                        Some(&t) => t,
                        None => {
                            let t = h.doc.create_element("title");
                            if let Some(head) = h.doc.head() {
                                h.doc.append_child(head, t);
                            }
                            t
                        }
                    };
                    h.doc.set_text_content(t, &s);
                }
                "cookie" if n == 0 => {
                    let s = sval(it)?;
                    let u = ph(it).url.clone();
                    crate::net::set_document_cookie(&u, &s);
                }
                "scrollTop" | "scrollLeft" => {
                    let val = it.to_number(v)? as f32;
                    let h = ph(it);
                    let root = h.doc.document_element() == Some(n) || h.doc.body() == Some(n);
                    if root {
                        if k == "scrollTop" {
                            h.scroll.1 = val;
                        } else {
                            h.scroll.0 = val;
                        }
                        h.clamp_scroll();
                    } else {
                        let e = h.elem_scroll.entry(n).or_insert((0.0, 0.0));
                        if k == "scrollTop" {
                            e.1 = val.max(0.0);
                        } else {
                            e.0 = val.max(0.0);
                        }
                        h.invalidate();
                    }
                }
                "style" if is_el => {
                    let s = sval(it)?;
                    ph(it).doc.set_attr(n, "style", &s);
                }
                "dir" | "lang" | "title" | "name" | "alt" | "placeholder" | "rel" | "target" | "role" | "min" | "max" | "step" | "pattern" | "width" | "height" | "rows" | "cols" | "maxLength" | "colSpan" | "rowSpan" | "label" | "download" | "srcset" | "sizes" | "loading" | "autocomplete" | "content" => {
                    let s = sval(it)?;
                    ph(it).doc.set_attr(n, &k.to_ascii_lowercase(), &s);
                }
                _ if is_el && REFLECTED.contains(&k) => {
                    let s = sval(it)?;
                    ph(it).doc.set_attr(n, &reflected_attr(k), &s);
                }
                _ if is_el && BOOL_REFLECTED.contains(&k) => {
                    let attr = k.to_ascii_lowercase();
                    let h = ph(it);
                    if v.truthy() {
                        h.doc.set_attr(n, &attr, "");
                    } else {
                        h.doc.remove_attr(n, &attr);
                    }
                }
                _ => return Err(Value::Undefined),
            }
            Ok(())
        })();
        match r {
            Err(Value::Undefined) => None,
            r => Some(r),
        }
    }

    fn keys(&self, _it: &mut Interp) -> Vec<PropKey> {
        vec![]
    }
}

// ---------------------------------------------------------------- style

fn inline_decls(h: &PageHost, n: NodeId) -> Vec<(String, String, bool)> {
    css::parse_declarations(h.doc.attr(n, "style").unwrap_or("")).into_iter().map(|d| (d.name.clone(), d.value_string(), d.important)).collect()
}

fn write_decls(h: &mut PageHost, n: NodeId, decls: &[(String, String, bool)]) {
    let s = decls.iter().map(|(k, v, imp)| format!("{k}: {v}{};", if *imp { " !important" } else { "" })).collect::<Vec<_>>().join(" ");
    if s.is_empty() {
        h.doc.remove_attr(n, "style");
    } else {
        h.doc.set_attr(n, "style", &s);
    }
}

pub fn style_set_property(h: &mut PageHost, n: NodeId, prop: &str, value: &str, important: bool) {
    let mut decls = inline_decls(h, n);
    decls.retain(|(k, _, _)| k != prop);
    if !value.trim().is_empty() {
        decls.push((prop.to_string(), value.trim().to_string(), important));
    }
    write_decls(h, n, &decls);
}

impl HostObject for StyleHost {
    fn class_name(&self) -> &'static str {
        "CSSStyleDeclaration"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn get(&self, it: &mut Interp, key: &PropKey) -> Option<R<Value>> {
        let k = key.as_str()?;
        let h = ph(it);
        if self.1 {
            // computed style
            if k == "length" || k == "cssText" {
                return None;
            }
            let prop = camel_to_kebab(k);
            let styles = h.styles();
            let v = styles.get(self.0).map(|s| s.get_property(&prop)).unwrap_or_default();
            if v.is_empty() && !k.contains('-') && styles.get(self.0).is_some_and(|s| !s.to_properties().iter().any(|(p, _)| *p == prop)) {
                return None;
            }
            return Some(Ok(str_v(v)));
        }
        match k {
            "cssText" => Some(Ok(str_v(h.doc.attr(self.0, "style").unwrap_or("")))),
            "length" => Some(Ok(Value::Num(inline_decls(h, self.0).len() as f64))),
            _ => {
                let prop = camel_to_kebab(k);
                if prop.contains(char::is_uppercase) {
                    return None;
                }
                let decls = inline_decls(h, self.0);
                let v = decls.iter().rev().find(|(p, _, _)| *p == prop).map(|(_, v, _)| v.clone());
                match v {
                    Some(v) => Some(Ok(str_v(v))),
                    None => {
                        if k.chars().all(|c| c.is_ascii_alphabetic() || c == '-') && k.len() > 2 && !matches!(k, "constructor" | "toString" | "valueOf" | "item" | "setProperty" | "getPropertyValue" | "removeProperty" | "getPropertyPriority" | "hasOwnProperty") {
                            Some(Ok(str_v("")))
                        } else {
                            None
                        }
                    }
                }
            }
        }
    }
    fn set(&self, it: &mut Interp, key: &PropKey, v: &Value) -> Option<R<()>> {
        let k = key.as_str()?;
        if self.1 {
            return Some(Ok(()));
        }
        let val = if v.is_nullish() { String::new() } else { it.to_string_lossy(v) };
        let h = ph(it);
        if k == "cssText" {
            h.doc.set_attr(self.0, "style", &val);
            return Some(Ok(()));
        }
        let prop = camel_to_kebab(k);
        let important = val.contains("!important");
        style_set_property(h, self.0, &prop, val.replace("!important", "").trim(), important);
        Some(Ok(()))
    }
    fn keys(&self, it: &mut Interp) -> Vec<PropKey> {
        inline_decls(ph(it), self.0).into_iter().map(|(k, _, _)| PropKey::Str(Rc::from(kebab_to_camel(&k).as_str()))).collect()
    }
}

impl HostObject for ClassListHost {
    fn class_name(&self) -> &'static str {
        "DOMTokenList"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn get(&self, it: &mut Interp, key: &PropKey) -> Option<R<Value>> {
        let h = ph(it);
        let classes: Vec<String> = h.doc.classes(self.0).into_iter().map(|s| s.to_string()).collect();
        if let Some(i) = key.array_index() {
            return Some(Ok(classes.get(i).map(str_v).unwrap_or(Value::Undefined)));
        }
        match key.as_str()? {
            "length" => Some(Ok(Value::Num(classes.len() as f64))),
            "value" => Some(Ok(str_v(h.doc.attr(self.0, "class").unwrap_or("")))),
            _ => None,
        }
    }
    fn set(&self, it: &mut Interp, key: &PropKey, v: &Value) -> Option<R<()>> {
        if key.as_str()? == "value" {
            let s = it.to_string_lossy(v);
            ph(it).doc.set_attr(self.0, "class", &s);
            return Some(Ok(()));
        }
        None
    }
    fn keys(&self, it: &mut Interp) -> Vec<PropKey> {
        let n = ph(it).doc.classes(self.0).len();
        (0..n).map(|i| PropKey::Str(Rc::from(i.to_string().as_str()))).collect()
    }
}

fn data_attr(k: &str) -> String {
    format!("data-{}", camel_to_kebab(k))
}

impl HostObject for DatasetHost {
    fn class_name(&self) -> &'static str {
        "DOMStringMap"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn get(&self, it: &mut Interp, key: &PropKey) -> Option<R<Value>> {
        let k = key.as_str()?;
        let h = ph(it);
        h.doc.attr(self.0, &data_attr(k)).map(|v| Ok(str_v(v)))
    }
    fn set(&self, it: &mut Interp, key: &PropKey, v: &Value) -> Option<R<()>> {
        let k = key.as_str()?;
        let s = it.to_string_lossy(v);
        ph(it).doc.set_attr(self.0, &data_attr(k), &s);
        Some(Ok(()))
    }
    fn keys(&self, it: &mut Interp) -> Vec<PropKey> {
        let h = ph(it);
        h.doc.element(self.0).map(|e| e.attrs.iter().filter_map(|(k, _)| k.strip_prefix("data-").map(|r| PropKey::Str(Rc::from(kebab_to_camel(r).as_str())))).collect()).unwrap_or_default()
    }
}

// ---------------------------------------------------------------- DOM methods

fn to_node_or_text(it: &mut Interp, v: &Value) -> R<NodeId> {
    if let Some(n) = node_of(v) {
        return Ok(n);
    }
    let s = it.to_str(v)?;
    Ok(ph(it).doc.create_text(&s))
}

fn check_insert(it: &mut Interp, parent: NodeId, child: NodeId) -> R<()> {
    if ph(it).doc.is_ancestor(child, parent) {
        throw!(it, "HierarchyRequestError", "The new child element contains the parent.");
    }
    Ok(())
}

pub fn closest(h: &PageHost, n: NodeId, sels: &[css::Selector]) -> Option<NodeId> {
    let mut cur = Some(n);
    while let Some(c) = cur {
        if h.doc.is_element(c) && css::matches_any(&h.doc, c, sels, &css::MatchState::default()) {
            return Some(c);
        }
        cur = h.doc.parent(c);
    }
    None
}

fn parse_sel(it: &mut Interp, v: &Value) -> R<Vec<css::Selector>> {
    let s = it.to_str(v)?;
    match css::parse_selector_list_str(&s) {
        Some(sels) => Ok(sels),
        None => throw!(it, "SyntaxError", "Failed to execute 'querySelector': '{s}' is not a valid selector."),
    }
}

fn query_all(it: &mut Interp, root: NodeId, sel: &Value) -> R<Vec<NodeId>> {
    let sels = parse_sel(it, sel)?;
    let h = ph(it);
    let st = css::MatchState { hover: h.hover, focus: h.focus, active: h.active, focus_visible: h.focus_visible };
    Ok(h.doc.element_descendants(root).into_iter().filter(|&e| css::matches_any(&h.doc, e, &sels, &st)).collect())
}

fn install_node_methods(it: &mut Interp, p: &DomProtos) {
    let node = &p.node;
    let el = &p.element;
    it.define_method(node, "appendChild", 1, |it, this, args| {
        let n = this_node(it, &this)?;
        let c = node_of(&arg(args, 0)).ok_or_else(|| it.make_error("TypeError", "Failed to execute 'appendChild': parameter 1 is not of type 'Node'."))?;
        check_insert(it, n, c)?;
        let roots = scripts::roots_of(it, c);
        ph(it).doc.append_child(n, c);
        scripts::run_inserted(it, &roots);
        Ok(arg(args, 0))
    });
    it.define_method(node, "insertBefore", 2, |it, this, args| {
        let n = this_node(it, &this)?;
        let c = node_of(&arg(args, 0)).ok_or_else(|| it.make_error("TypeError", "parameter 1 is not of type 'Node'."))?;
        let before = node_of(&arg(args, 1));
        check_insert(it, n, c)?;
        let roots = scripts::roots_of(it, c);
        ph(it).doc.insert_before(n, c, before);
        scripts::run_inserted(it, &roots);
        Ok(arg(args, 0))
    });
    it.define_method(node, "removeChild", 1, |it, this, args| {
        let n = this_node(it, &this)?;
        let c = node_of(&arg(args, 0)).ok_or_else(|| it.make_error("TypeError", "parameter 1 is not of type 'Node'."))?;
        if ph(it).doc.parent(c) != Some(n) {
            throw!(it, "NotFoundError", "The node to be removed is not a child of this node.");
        }
        ph(it).doc.detach(c);
        Ok(arg(args, 0))
    });
    it.define_method(node, "replaceChild", 2, |it, this, args| {
        let n = this_node(it, &this)?;
        let newc = node_of(&arg(args, 0)).ok_or_else(|| it.make_error("TypeError", "parameter 1 is not of type 'Node'."))?;
        let old = node_of(&arg(args, 1)).ok_or_else(|| it.make_error("TypeError", "parameter 2 is not of type 'Node'."))?;
        let roots = scripts::roots_of(it, newc);
        let h = ph(it);
        let next = h.doc.next_sibling(old);
        h.doc.detach(old);
        h.doc.insert_before(n, newc, next);
        scripts::run_inserted(it, &roots);
        Ok(arg(args, 1))
    });
    it.define_method(node, "cloneNode", 1, |it, this, args| {
        let n = this_node(it, &this)?;
        let deep = arg(args, 0).truthy();
        let c = ph(it).doc.clone_node(n, deep);
        Ok(wrap(it, c))
    });
    it.define_method(node, "contains", 1, |it, this, args| {
        let n = this_node(it, &this)?;
        Ok(Value::Bool(match node_of(&arg(args, 0)) {
            Some(c) => ph(it).doc.is_ancestor(n, c),
            None => false,
        }))
    });
    it.define_method(node, "hasChildNodes", 0, |it, this, _| {
        let n = this_node(it, &this)?;
        Ok(Value::Bool(!ph(it).doc.children(n).is_empty()))
    });
    it.define_method(node, "getRootNode", 0, |it, this, _| {
        let mut n = this_node(it, &this)?;
        while let Some(p) = ph(it).doc.parent(n) {
            n = p;
        }
        Ok(wrap(it, n))
    });
    it.define_method(node, "isEqualNode", 1, |it, this, args| {
        let n = this_node(it, &this)?;
        Ok(Value::Bool(match node_of(&arg(args, 0)) {
            Some(o) => ph(it).doc.outer_html(n) == ph(it).doc.outer_html(o),
            None => false,
        }))
    });
    it.define_method(node, "isSameNode", 1, |it, this, args| {
        let n = this_node(it, &this)?;
        Ok(Value::Bool(node_of(&arg(args, 0)) == Some(n)))
    });
    it.define_method(node, "normalize", 0, |_, _, _| Ok(Value::Undefined));
    it.define_method(node, "compareDocumentPosition", 1, |it, this, args| {
        let a = this_node(it, &this)?;
        let Some(b) = node_of(&arg(args, 0)) else { return Ok(Value::Num(1.0)) };
        let h = ph(it);
        if a == b {
            return Ok(Value::Num(0.0));
        }
        if h.doc.is_ancestor(a, b) {
            return Ok(Value::Num(20.0));
        }
        if h.doc.is_ancestor(b, a) {
            return Ok(Value::Num(10.0));
        }
        let order = h.doc.descendants(0);
        let ia = order.iter().position(|&x| x == a);
        let ib = order.iter().position(|&x| x == b);
        Ok(Value::Num(if ib > ia { 4.0 } else { 2.0 }))
    });
    // ChildNode / ParentNode mixins
    for (name, mode) in [("append", 0), ("prepend", 1), ("before", 2), ("after", 3), ("replaceWith", 4), ("replaceChildren", 5)] {
        it.define_method(node, name, 0, move |it, this, args| {
            let n = this_node(it, &this)?;
            let mut nodes = Vec::new();
            for a in args {
                nodes.push(to_node_or_text(it, a)?);
            }
            let mut roots = Vec::new();
            for &c in &nodes {
                roots.extend(scripts::roots_of(it, c));
            }
            let h = ph(it);
            match mode {
                0 => {
                    for c in nodes {
                        h.doc.append_child(n, c);
                    }
                }
                1 => {
                    let first = h.doc.children(n).first().copied();
                    for c in nodes {
                        h.doc.insert_before(n, c, first);
                    }
                }
                5 => {
                    h.doc.remove_all_children(n);
                    for c in nodes {
                        h.doc.append_child(n, c);
                    }
                }
                _ => {
                    let Some(p) = h.doc.parent(n) else { return Ok(Value::Undefined) };
                    let anchor = if mode == 2 { Some(n) } else { h.doc.next_sibling(n) };
                    for c in nodes {
                        h.doc.insert_before(p, c, anchor);
                    }
                    if mode == 4 {
                        h.doc.detach(n);
                    }
                }
            }
            scripts::run_inserted(it, &roots);
            Ok(Value::Undefined)
        });
    }
    it.define_method(node, "remove", 0, |it, this, _| {
        let n = this_node(it, &this)?;
        ph(it).doc.detach(n);
        Ok(Value::Undefined)
    });
    for (name, all) in [("querySelector", false), ("querySelectorAll", true)] {
        it.define_method(node, name, 1, move |it, this, args| {
            let n = this_node(it, &this)?;
            let found = query_all(it, n, &arg(args, 0))?;
            if all {
                Ok(node_list(it, found))
            } else {
                let f = found.first().copied();
                Ok(wrap_opt(it, f))
            }
        });
    }
    it.define_method(node, "getElementsByTagName", 1, |it, this, args| {
        let n = this_node(it, &this)?;
        let t = it.to_str(&arg(args, 0))?;
        let found = ph(it).doc.elements_by_tag(n, &t);
        Ok(node_list(it, found))
    });
    it.define_method(node, "getElementsByClassName", 1, |it, this, args| {
        let n = this_node(it, &this)?;
        let names: Vec<String> = it.to_str(&arg(args, 0))?.split_whitespace().map(|s| s.to_string()).collect();
        let h = ph(it);
        let found: Vec<NodeId> = h.doc.element_descendants(n).into_iter().filter(|&e| names.iter().all(|c| h.doc.has_class(e, c))).collect();
        Ok(node_list(it, found))
    });
    it.define_method(node, "getElementsByName", 1, |it, this, args| {
        let n = this_node(it, &this)?;
        let name = it.to_str(&arg(args, 0))?;
        let h = ph(it);
        let found: Vec<NodeId> = h.doc.element_descendants(n).into_iter().filter(|&e| h.doc.attr(e, "name") == Some(&*name)).collect();
        Ok(node_list(it, found))
    });
    // Element
    it.define_method(el, "getAttribute", 1, |it, this, args| {
        let n = this_node(it, &this)?;
        let k = it.to_str(&arg(args, 0))?.to_ascii_lowercase();
        Ok(ph(it).doc.attr(n, &k).map(str_v).unwrap_or(Value::Null))
    });
    it.define_method(el, "getAttributeNS", 2, |it, this, args| {
        let n = this_node(it, &this)?;
        let k = it.to_str(&arg(args, 1))?.to_ascii_lowercase();
        Ok(ph(it).doc.attr(n, &k).map(str_v).unwrap_or(Value::Null))
    });
    it.define_method(el, "setAttribute", 2, |it, this, args| {
        let n = this_node(it, &this)?;
        let k = it.to_str(&arg(args, 0))?.to_ascii_lowercase();
        let v = it.to_str(&arg(args, 1))?;
        ph(it).doc.set_attr(n, &k, &v);
        if k == "value" {
            // value attribute resets a pristine control
        }
        Ok(Value::Undefined)
    });
    it.define_method(el, "setAttributeNS", 3, |it, this, args| {
        let n = this_node(it, &this)?;
        let k = it.to_str(&arg(args, 1))?.to_ascii_lowercase();
        let v = it.to_str(&arg(args, 2))?;
        ph(it).doc.set_attr(n, &k, &v);
        Ok(Value::Undefined)
    });
    it.define_method(el, "removeAttribute", 1, |it, this, args| {
        let n = this_node(it, &this)?;
        let k = it.to_str(&arg(args, 0))?.to_ascii_lowercase();
        ph(it).doc.remove_attr(n, &k);
        Ok(Value::Undefined)
    });
    it.define_method(el, "hasAttribute", 1, |it, this, args| {
        let n = this_node(it, &this)?;
        let k = it.to_str(&arg(args, 0))?.to_ascii_lowercase();
        Ok(Value::Bool(ph(it).doc.has_attr(n, &k)))
    });
    it.define_method(el, "hasAttributes", 0, |it, this, _| {
        let n = this_node(it, &this)?;
        Ok(Value::Bool(ph(it).doc.element(n).is_some_and(|e| !e.attrs.is_empty())))
    });
    it.define_method(el, "toggleAttribute", 1, |it, this, args| {
        let n = this_node(it, &this)?;
        let k = it.to_str(&arg(args, 0))?.to_ascii_lowercase();
        let force = arg(args, 1);
        let h = ph(it);
        let on = if force.is_undefined() { !h.doc.has_attr(n, &k) } else { force.truthy() };
        if on {
            h.doc.set_attr(n, &k, "");
        } else {
            h.doc.remove_attr(n, &k);
        }
        Ok(Value::Bool(on))
    });
    it.define_method(el, "getAttributeNames", 0, |it, this, _| {
        let n = this_node(it, &this)?;
        let names: Vec<Value> = ph(it).doc.element(n).map(|e| e.attrs.iter().map(|(k, _)| str_v(k)).collect()).unwrap_or_default();
        Ok(it.new_array(names))
    });
    it.define_method(el, "matches", 1, |it, this, args| {
        let n = this_node(it, &this)?;
        let sels = parse_sel(it, &arg(args, 0))?;
        let h = ph(it);
        let st = css::MatchState { hover: h.hover, focus: h.focus, active: h.active, focus_visible: h.focus_visible };
        Ok(Value::Bool(css::matches_any(&h.doc, n, &sels, &st)))
    });
    let m = el.get_own("matches").unwrap();
    el.set_hidden("webkitMatchesSelector", m);
    it.define_method(el, "closest", 1, |it, this, args| {
        let n = this_node(it, &this)?;
        let sels = parse_sel(it, &arg(args, 0))?;
        let c = closest(ph(it), n, &sels);
        Ok(wrap_opt(it, c))
    });
    it.define_method(el, "getBoundingClientRect", 0, |it, this, _| {
        let n = this_node(it, &this)?;
        Ok(client_rect(it, n))
    });
    it.define_method(el, "getClientRects", 0, |it, this, _| {
        let n = this_node(it, &this)?;
        // one rect per fragment (an inline element broken over lines has several)
        let h = ph(it);
        let sc = h.scroll;
        let l = h.layout();
        let (dx, dy) = if l.find_box(n).is_some_and(|b| b.fixed) { (0.0, 0.0) } else { sc };
        let rects = l.rects.get(&n).cloned().unwrap_or_default();
        if rects.is_empty() {
            let r = if ph(it).node_rect(n).is_some() { vec![client_rect(it, n)] } else { vec![] };
            return Ok(it.new_array(r));
        }
        let vals = rects.into_iter().map(|r| rect_obj(it, r.x - dx, r.y - dy, r.w, r.h)).collect();
        Ok(it.new_array(vals))
    });
    it.define_method(el, "scrollIntoView", 1, |it, this, _| {
        let n = this_node(it, &this)?;
        let h = ph(it);
        if let Some(r) = h.node_rect(n) {
            h.scroll.1 = r.y - 8.0;
            h.clamp_scroll();
        }
        Ok(Value::Undefined)
    });
    for name in ["scrollTo", "scroll", "scrollBy"] {
        it.define_method(el, name, 2, move |it, this, args| {
            let n = this_node(it, &this)?;
            let (x, y) = window::scroll_args(it, args)?;
            let h = ph(it);
            let e = h.elem_scroll.entry(n).or_insert((0.0, 0.0));
            if name == "scrollBy" {
                e.0 = (e.0 + x.unwrap_or(0.0)).max(0.0);
                e.1 = (e.1 + y.unwrap_or(0.0)).max(0.0);
            } else {
                if let Some(x) = x {
                    e.0 = x.max(0.0);
                }
                if let Some(y) = y {
                    e.1 = y.max(0.0);
                }
            }
            h.invalidate();
            Ok(Value::Undefined)
        });
    }
    it.define_method(el, "insertAdjacentHTML", 2, |it, this, args| {
        let n = this_node(it, &this)?;
        let pos = it.to_str(&arg(args, 0))?.to_ascii_lowercase();
        let html = it.to_str(&arg(args, 1))?;
        let h = ph(it);
        let frag = h.doc.create_fragment();
        crate::html::parse_fragment_into(&mut h.doc, frag, &html);
        scripts::mark_started(it, frag);
        insert_adjacent(ph(it), n, &pos, frag);
        Ok(Value::Undefined)
    });
    it.define_method(el, "insertAdjacentElement", 2, |it, this, args| {
        let n = this_node(it, &this)?;
        let pos = it.to_str(&arg(args, 0))?.to_ascii_lowercase();
        let Some(c) = node_of(&arg(args, 1)) else { return Ok(Value::Null) };
        let roots = scripts::roots_of(it, c);
        insert_adjacent(ph(it), n, &pos, c);
        scripts::run_inserted(it, &roots);
        Ok(arg(args, 1))
    });
    it.define_method(el, "insertAdjacentText", 2, |it, this, args| {
        let n = this_node(it, &this)?;
        let pos = it.to_str(&arg(args, 0))?.to_ascii_lowercase();
        let t = it.to_str(&arg(args, 1))?;
        let h = ph(it);
        let tn = h.doc.create_text(&t);
        insert_adjacent(h, n, &pos, tn);
        Ok(Value::Undefined)
    });
    it.define_method(el, "attachShadow", 1, |it, this, _| {
        // Shadow DOM is not supported; return the element itself as its own "root".
        let _ = this_node(it, &this)?;
        Ok(this)
    });
    it.define_method(el, "animate", 2, |it, _, _| {
        let o = it.new_object();
        let p = crate::js::builtins::promise::promise_resolve(it, Value::Undefined)?;
        o.set_prop("finished", p);
        let noop = it.native("cancel", 0, |_, _, _| Ok(Value::Undefined));
        o.set_prop("cancel", noop.clone());
        o.set_prop("play", noop.clone());
        o.set_prop("pause", noop);
        Ok(Value::Obj(o))
    });
    it.define_method(el, "getAnimations", 0, |it, _, _| Ok(it.new_array(vec![])));
    for name in ["focus", "blur", "click", "select", "submit", "reset", "requestSubmit", "showModal", "show", "close", "showPopover", "hidePopover", "checkValidity", "reportValidity", "setSelectionRange", "setCustomValidity", "requestFullscreen", "play", "pause", "load"] {
        it.define_method(el, name, 0, move |it, this, args| {
            let n = this_node(it, &this)?;
            window::element_action(it, n, name, args)
        });
    }
    it.define_method(el, "getContext", 1, |_, _, _| Ok(Value::Null));
    it.define_method(el, "toString", 0, |it, this, _| {
        let n = this_node(it, &this)?;
        let h = ph(it);
        if h.doc.tag_is(n, "a") {
            return Ok(str_v(resolved_attr_url(h, n, "href")));
        }
        Ok(str_v("[object HTMLElement]"))
    });

    // CSSStyleDeclaration
    let sd = &p.style_decl;
    it.define_method(sd, "setProperty", 3, |it, this, args| {
        let Some(h) = this.as_obj().and_then(|o| o.host()) else { return Ok(Value::Undefined) };
        let Some(s) = h.as_any().downcast_ref::<StyleHost>() else { return Ok(Value::Undefined) };
        let n = s.0;
        let prop = it.to_str(&arg(args, 0))?.to_string();
        let val = if arg(args, 1).is_nullish() { String::new() } else { it.to_str(&arg(args, 1))?.to_string() };
        let imp = it.to_string_lossy(&arg(args, 2)) == "important";
        style_set_property(ph(it), n, &prop, &val, imp);
        Ok(Value::Undefined)
    });
    it.define_method(sd, "getPropertyValue", 1, |it, this, args| {
        let prop = it.to_str(&arg(args, 0))?;
        let v = it.get(&this, &PropKey::Str(Rc::from(kebab_to_camel(&prop).as_str())))?;
        let v = if v.is_undefined() { it.get(&this, &PropKey::Str(prop.clone()))? } else { v };
        Ok(if v.is_undefined() { str_v("") } else { v })
    });
    it.define_method(sd, "removeProperty", 1, |it, this, args| {
        let Some(h) = this.as_obj().and_then(|o| o.host()) else { return Ok(Value::Undefined) };
        let Some(s) = h.as_any().downcast_ref::<StyleHost>() else { return Ok(Value::Undefined) };
        let n = s.0;
        let prop = it.to_str(&arg(args, 0))?.to_string();
        style_set_property(ph(it), n, &prop, "", false);
        Ok(str_v(""))
    });
    it.define_method(sd, "getPropertyPriority", 1, |_, _, _| Ok(str_v("")));

    // DOMTokenList
    let tl = &p.token_list;
    fn tl_node(it: &mut Interp, this: &Value) -> R<NodeId> {
        let h = this.as_obj().and_then(|o| o.host());
        match h.as_ref().and_then(|h| h.as_any().downcast_ref::<ClassListHost>()) {
            Some(c) => Ok(c.0),
            None => throw!(it, "TypeError", "Illegal invocation"),
        }
    }
    fn set_classes(h: &mut PageHost, n: NodeId, classes: &[String]) {
        h.doc.set_attr(n, "class", &classes.join(" "));
    }
    it.define_method(tl, "add", 1, |it, this, args| {
        let n = tl_node(it, &this)?;
        let mut add = Vec::new();
        for a in args {
            add.push(it.to_str(a)?.to_string());
        }
        let h = ph(it);
        let mut cls: Vec<String> = h.doc.classes(n).into_iter().map(|s| s.to_string()).collect();
        for a in add {
            if !cls.contains(&a) {
                cls.push(a);
            }
        }
        set_classes(h, n, &cls);
        Ok(Value::Undefined)
    });
    it.define_method(tl, "remove", 1, |it, this, args| {
        let n = tl_node(it, &this)?;
        let mut rm = Vec::new();
        for a in args {
            rm.push(it.to_str(a)?.to_string());
        }
        let h = ph(it);
        let cls: Vec<String> = h.doc.classes(n).into_iter().map(|s| s.to_string()).filter(|c| !rm.contains(c)).collect();
        set_classes(h, n, &cls);
        Ok(Value::Undefined)
    });
    it.define_method(tl, "toggle", 1, |it, this, args| {
        let n = tl_node(it, &this)?;
        let c = it.to_str(&arg(args, 0))?.to_string();
        let force = arg(args, 1);
        let h = ph(it);
        let mut cls: Vec<String> = h.doc.classes(n).into_iter().map(|s| s.to_string()).collect();
        let has = cls.contains(&c);
        let want = if force.is_undefined() { !has } else { force.truthy() };
        if want && !has {
            cls.push(c);
        } else if !want && has {
            cls.retain(|x| *x != c);
        }
        set_classes(h, n, &cls);
        Ok(Value::Bool(want))
    });
    it.define_method(tl, "contains", 1, |it, this, args| {
        let n = tl_node(it, &this)?;
        let c = it.to_str(&arg(args, 0))?;
        Ok(Value::Bool(ph(it).doc.has_class(n, &c)))
    });
    it.define_method(tl, "replace", 2, |it, this, args| {
        let n = tl_node(it, &this)?;
        let a = it.to_str(&arg(args, 0))?.to_string();
        let b = it.to_str(&arg(args, 1))?.to_string();
        let h = ph(it);
        let mut cls: Vec<String> = h.doc.classes(n).into_iter().map(|s| s.to_string()).collect();
        let found = cls.iter().position(|c| *c == a);
        if let Some(i) = found {
            cls[i] = b;
            set_classes(h, n, &cls);
        }
        Ok(Value::Bool(found.is_some()))
    });
    it.define_method(tl, "item", 1, |it, this, args| {
        let n = tl_node(it, &this)?;
        let i = it.to_integer(&arg(args, 0))? as usize;
        Ok(ph(it).doc.classes(n).get(i).map(|s| str_v(s)).unwrap_or(Value::Null))
    });
    it.define_method(tl, "toString", 0, |it, this, _| {
        let n = tl_node(it, &this)?;
        Ok(str_v(ph(it).doc.attr(n, "class").unwrap_or("")))
    });
    it.define_method(tl, "forEach", 1, |it, this, args| {
        let n = tl_node(it, &this)?;
        let cls: Vec<String> = ph(it).doc.classes(n).into_iter().map(|s| s.to_string()).collect();
        let f = arg(args, 0);
        for (i, c) in cls.into_iter().enumerate() {
            it.call(&f, Value::Undefined, &[str_v(c), Value::Num(i as f64), this.clone()])?;
        }
        Ok(Value::Undefined)
    });
    let si = it.sym_iterator.clone();
    it.define_method_sym(tl, &si, "[Symbol.iterator]", |it, this, _| {
        let n = tl_node(it, &this)?;
        let cls: Vec<Value> = ph(it).doc.classes(n).into_iter().map(str_v).collect();
        Ok(crate::js::builtins::make_list_iterator(it, cls))
    });
}

fn insert_adjacent(h: &mut PageHost, n: NodeId, pos: &str, c: NodeId) {
    match pos {
        "beforebegin" => {
            if let Some(p) = h.doc.parent(n) {
                h.doc.insert_before(p, c, Some(n));
            }
        }
        "afterbegin" => {
            let first = h.doc.children(n).first().copied();
            h.doc.insert_before(n, c, first);
        }
        "beforeend" => {
            h.doc.append_child(n, c);
        }
        "afterend" => {
            if let Some(p) = h.doc.parent(n) {
                let next = h.doc.next_sibling(n);
                h.doc.insert_before(p, c, next);
            }
        }
        _ => {}
    }
}

fn install_document_methods(it: &mut Interp, p: &DomProtos) {
    let d = &p.document;
    // Where characters [start, end) of a text node are, for Range in prelude.js.
    let text_rects = it.native("__textRects", 3, |it, _, args| {
        let n = this_node(it, &arg(args, 0))?;
        let start = it.to_number(&arg(args, 1))?.max(0.0) as usize;
        let end = it.to_number(&arg(args, 2))?.max(0.0) as usize;
        let h = ph(it);
        let sc = h.scroll;
        // Text is laid out per element: the parent's direct text, with this node's offset in it.
        let Some(owner) = h.doc.parent(n) else { return Ok(it.new_array(vec![])) };
        let mut text = String::new();
        let mut base = None;
        for &c in h.doc.children(owner) {
            if c == n {
                base = Some(text.encode_utf16().count());
            }
            if let NodeData::Text(t) = &h.doc.node(c).data {
                text.push_str(t);
            }
        }
        let base = base.unwrap_or(0);
        let rects = h.layout().text_rects(owner, &text, base + start, base + end);
        let vals = rects
            .into_iter()
            .map(|(r, fixed)| {
                let (dx, dy) = if fixed { (0.0, 0.0) } else { sc };
                rect_obj(it, r.x - dx, r.y - dy, r.w, r.h)
            })
            .collect();
        Ok(it.new_array(vals))
    });
    d.set_hidden("__textRects", text_rects);
    it.define_method(d, "getElementById", 1, |it, _, args| {
        let id = it.to_str(&arg(args, 0))?;
        let f = ph(it).doc.get_element_by_id(&id);
        Ok(wrap_opt(it, f))
    });
    it.define_method(d, "createElement", 1, |it, _, args| {
        let t = it.to_str(&arg(args, 0))?;
        if t.is_empty() || !t.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == ':') {
            throw!(it, "InvalidCharacterError", "The tag name provided ('{t}') is not a valid name.");
        }
        let n = ph(it).doc.create_element(&t);
        Ok(wrap(it, n))
    });
    it.define_method(d, "createElementNS", 2, |it, _, args| {
        let t = it.to_str(&arg(args, 1))?;
        let n = ph(it).doc.create_element(&t);
        Ok(wrap(it, n))
    });
    it.define_method(d, "createTextNode", 1, |it, _, args| {
        let t = it.to_str(&arg(args, 0))?;
        let n = ph(it).doc.create_text(&t);
        Ok(wrap(it, n))
    });
    it.define_method(d, "createComment", 1, |it, _, args| {
        let t = it.to_str(&arg(args, 0))?;
        let n = ph(it).doc.create_comment(&t);
        Ok(wrap(it, n))
    });
    it.define_method(d, "createDocumentFragment", 0, |it, _, _| {
        let n = ph(it).doc.create_fragment();
        Ok(wrap(it, n))
    });
    it.define_method(d, "importNode", 2, |it, _, args| {
        let Some(n) = node_of(&arg(args, 0)) else { return Ok(Value::Null) };
        let c = ph(it).doc.clone_node(n, arg(args, 1).truthy());
        Ok(wrap(it, c))
    });
    it.define_method(d, "adoptNode", 1, |_, _, args| Ok(arg(args, 0)));
    it.define_method(d, "createEvent", 1, |it, _, args| {
        let t = it.to_string_lossy(&arg(args, 0));
        events::new_event(it, "", &Value::Undefined, &t)
    });
    it.define_method(d, "write", 1, |it, _, args| {
        let mut s = String::new();
        for a in args {
            s.push_str(&it.to_str(a)?);
        }
        window::document_write(it, &s);
        Ok(Value::Undefined)
    });
    it.define_method(d, "writeln", 1, |it, _, args| {
        let mut s = String::new();
        for a in args {
            s.push_str(&it.to_str(a)?);
        }
        s.push('\n');
        window::document_write(it, &s);
        Ok(Value::Undefined)
    });
    it.define_method(d, "open", 0, |_, this, _| Ok(this));
    it.define_method(d, "close", 0, |_, _, _| Ok(Value::Undefined));
    it.define_method(d, "hasFocus", 0, |_, _, _| Ok(Value::Bool(true)));
    it.define_method(d, "execCommand", 0, |_, _, _| Ok(Value::Bool(false)));
    it.define_method(d, "getSelection", 0, |it, _, _| window::get_selection(it));
    it.define_method(d, "elementFromPoint", 2, |it, _, args| {
        let x = it.to_number(&arg(args, 0))? as f32;
        let y = it.to_number(&arg(args, 1))? as f32;
        let hit = ph(it).hit_test(x, y);
        let hit = hit.map(|n| {
            let h = ph(it);
            if h.doc.is_element(n) { n } else { h.doc.parent(n).unwrap_or(n) }
        });
        Ok(wrap_opt(it, hit))
    });
}

/// Install DOM globals into a page interpreter.
pub fn install(it: &mut Interp) {
    let op = it.p.object.clone();
    let mk = |proto: &Obj, class: &'static str| Obj::new(ObjKind::Ordinary, Some(proto.clone()), class);
    let event_target = mk(&op, "EventTarget");
    let node = mk(&event_target, "Node");
    let element = mk(&node, "Element");
    let html_element = mk(&element, "HTMLElement");
    let document = mk(&node, "Document");
    let char_data = mk(&node, "CharacterData");
    let text = mk(&char_data, "Text");
    let comment = mk(&char_data, "Comment");
    let fragment = mk(&node, "DocumentFragment");
    let style_decl = mk(&op, "CSSStyleDeclaration");
    let token_list = mk(&op, "DOMTokenList");
    let event = mk(&op, "Event");
    let mut by_tag: HashMap<&'static str, Obj> = HashMap::new();
    let specific: &[(&str, &[&'static str])] = &[
        ("HTMLDivElement", &["div"]),
        ("HTMLSpanElement", &["span"]),
        ("HTMLParagraphElement", &["p"]),
        ("HTMLAnchorElement", &["a"]),
        ("HTMLImageElement", &["img"]),
        ("HTMLInputElement", &["input"]),
        ("HTMLButtonElement", &["button"]),
        ("HTMLFormElement", &["form"]),
        ("HTMLSelectElement", &["select"]),
        ("HTMLOptionElement", &["option"]),
        ("HTMLTextAreaElement", &["textarea"]),
        ("HTMLLabelElement", &["label"]),
        ("HTMLUListElement", &["ul"]),
        ("HTMLOListElement", &["ol"]),
        ("HTMLLIElement", &["li"]),
        ("HTMLTableElement", &["table"]),
        ("HTMLTableRowElement", &["tr"]),
        ("HTMLTableCellElement", &["td", "th"]),
        ("HTMLHeadingElement", &["h1", "h2", "h3", "h4", "h5", "h6"]),
        ("HTMLScriptElement", &["script"]),
        ("HTMLStyleElement", &["style"]),
        ("HTMLLinkElement", &["link"]),
        ("HTMLBodyElement", &["body"]),
        ("HTMLHeadElement", &["head"]),
        ("HTMLHtmlElement", &["html"]),
        ("HTMLCanvasElement", &["canvas"]),
        ("HTMLTemplateElement", &["template"]),
        ("HTMLIFrameElement", &["iframe"]),
        ("HTMLDetailsElement", &["details"]),
        ("HTMLDialogElement", &["dialog"]),
        ("HTMLPreElement", &["pre"]),
        ("HTMLBRElement", &["br"]),
        ("HTMLHRElement", &["hr"]),
        ("HTMLVideoElement", &["video"]),
        ("HTMLAudioElement", &["audio"]),
        ("HTMLMetaElement", &["meta"]),
        ("HTMLTitleElement", &["title"]),
        ("SVGElement", &["svg"]),
    ];
    let mut named: Vec<(&str, Obj)> = Vec::new();
    for (name, tags) in specific {
        let pr = mk(&html_element, "HTMLElement");
        for t in *tags {
            by_tag.insert(t, pr.clone());
        }
        named.push((name, pr));
    }
    let p = DomProtos {
        event_target: event_target.clone(),
        node: node.clone(),
        element: element.clone(),
        html_element: html_element.clone(),
        document: document.clone(),
        text: text.clone(),
        comment,
        fragment,
        by_tag,
        style_decl,
        token_list,
        event: event.clone(),
    };
    install_node_methods(it, &p);
    install_document_methods(it, &p);
    events::install(it, &p);
    let p = Rc::new(p);
    it.ext.insert("dom_protos", p.clone());

    // constructors for instanceof checks
    let illegal = |name: &'static str| move |it: &mut Interp, _: Value, _: &[Value]| -> R<Value> { throw!(it, "TypeError", "Illegal constructor: {name}") };
    for (name, proto) in [("Node", &p.node), ("Element", &p.element), ("HTMLElement", &p.html_element), ("Document", &p.document), ("HTMLDocument", &p.document), ("CharacterData", &char_data), ("Text", &p.text), ("Comment", &p.comment), ("DocumentFragment", &p.fragment), ("CSSStyleDeclaration", &p.style_decl), ("DOMTokenList", &p.token_list)] {
        it.define_ctor(name, 0, proto, illegal(name));
    }
    for (name, proto) in &named {
        let n: &'static str = name;
        it.define_ctor(n, 0, proto, illegal(n));
    }
    // Node constants
    if let Some(Value::Obj(nc)) = it.global.get_own("Node") {
        for (k, v) in [("ELEMENT_NODE", 1), ("TEXT_NODE", 3), ("COMMENT_NODE", 8), ("DOCUMENT_NODE", 9), ("DOCUMENT_FRAGMENT_NODE", 11), ("DOCUMENT_POSITION_PRECEDING", 2), ("DOCUMENT_POSITION_FOLLOWING", 4), ("DOCUMENT_POSITION_CONTAINS", 8), ("DOCUMENT_POSITION_CONTAINED_BY", 16)] {
            nc.set_hidden(k, Value::Num(v as f64));
        }
    }
    window::install(it);
    let doc = wrap(it, 0);
    it.global.set_hidden("document", doc);
    let g = it.global.clone();
    cssom::install(it);
    crate::workers::install(it);
    // window named access: a bare `foo` finds the element with id="foo"
    it.named_lookup = Some(|it, name| {
        let n = ph(it).doc.get_element_by_id(name)?;
        Some(wrap(it, n))
    });
    it.define_method(&g, "__tip_frame_lookup", 2, |it, _, args| {
        let w = it.to_number(&arg(args, 0))? as i64;
        let k = it.to_string_lossy(&arg(args, 1));
        Ok(crate::frames::frame_lookup(it, w, &k))
    });
    it.define_method(&g, "__tip_port_send", 2, |it, _, args| {
        let peer = it.to_number(&arg(args, 0))? as u64;
        crate::frames::port_send(it, peer, &arg(args, 1))?;
        Ok(Value::Undefined)
    });
    it.define_method(&g, "__tip_blob_register", 3, |it, _, args| {
        let url = it.to_string_lossy(&arg(args, 0));
        let bytes = crate::js::builtins::typed::bytes_of(&arg(args, 1)).unwrap_or_default();
        let mime = it.to_string_lossy(&arg(args, 2));
        crate::net::register_blob(&url, &mime, bytes);
        Ok(Value::Undefined)
    });
    it.define_method(&g, "__tip_blob_revoke", 1, |it, _, args| {
        let url = it.to_string_lossy(&arg(args, 0));
        crate::net::revoke_blob(&url);
        Ok(Value::Undefined)
    });
    // web APIs written in JavaScript
    if let Err(e) = it.eval_script(include_str!("prelude.js"), "tip:prelude") {
        let msg = it.describe_error(&e);
        eprintln!("tip: prelude failed: {msg} (line {})", it.cur_line + 1);
    }
    it.completion = Value::Undefined;
}
